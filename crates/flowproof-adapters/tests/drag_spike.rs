//! The drag dispatch, measured rather than asserted once.
//!
//! A drag either lands or silently does not, and "it worked when I ran it"
//! is exactly the claim this repository does not accept about a mechanism.
//! So this test drives the same drag N times from a fresh page and reports
//! how many landed - the number that decides whether `Drag` may exist.
//!
//! Against a live jQuery UI `sortable` with `connectToSortable` it measured
//! 20/20, up from 4/8 before the dispatch was fixed. Point it at one with
//! `FLOWPROOF_DRAG_URL=<url>`; by default it runs its own fixture.

use flowproof_driver::{AppDriver, UiaSelector, WebBrowserConfig, WebViewport};

/// A mouse-family sortable in miniature: a held pointer, a distance
/// threshold before the drag starts, and a drop that only counts when the
/// pointer is inside the target. It ABORTS on a move that reports no button
/// held, which is what a real jQuery UI does - and that guard bites:
/// stripping the held button from the dispatch takes this from 10/10 to 0.
///
/// What the fixture does NOT cover is the other half of the fix. Reading the
/// two midpoints in one layout only matters when the source and target
/// cannot both be on screen at once, and here they can. That half is what
/// the live measurement is for.
const FIXTURE: &str = r#"<!doctype html><html><body>
    <div id="src" style="width:120px;height:40px;background:#ccc">drag me</div>
    <div style="height:400px"></div>
    <div id="dst" style="width:200px;height:80px;background:#eee">drop here</div>
    <script>
      var down = null, started = false;
      document.getElementById('src').addEventListener('mousedown', function (e) {
        down = {x: e.clientX, y: e.clientY}; started = false;
      });
      document.addEventListener('mousemove', function (e) {
        if (!down) { return; }
        if (!e.buttons) { down = null; return; }          // the button came up
        if (!started && Math.abs(e.clientX - down.x) + Math.abs(e.clientY - down.y) > 4) {
          started = true;
        }
      });
      document.addEventListener('mouseup', function (e) {
        if (!down || !started) { down = null; return; }
        var r = document.getElementById('dst').getBoundingClientRect();
        if (e.clientX >= r.left && e.clientX <= r.right &&
            e.clientY >= r.top && e.clientY <= r.bottom) {
          document.getElementById('dst').innerHTML =
            '<span id="landed">landed</span>';
        }
        down = null; started = false;
      });
    </script></body></html>"#;

fn serve(html: &'static str) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            use std::io::{Read, Write};
            let mut buf = [0u8; 2048];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n{}",
                    html.len(),
                    html
                )
                .as_bytes(),
            );
        }
    });
    format!("http://127.0.0.1:{port}")
}

#[test]
fn the_drag_dispatch_lands_every_time() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping drag measurement: set FLOWPROOF_E2E=1 to run it");
        return;
    }
    let live = std::env::var("FLOWPROOF_DRAG_URL").ok();
    let url = live.clone().unwrap_or_else(|| serve(FIXTURE));
    let source = std::env::var("FLOWPROOF_DRAG_SOURCE").unwrap_or_else(|_| "#src".into());
    let target = std::env::var("FLOWPROOF_DRAG_TARGET").unwrap_or_else(|_| "#dst".into());
    let probe = std::env::var("FLOWPROOF_DRAG_PROBE").unwrap_or_else(|_| "#landed".into());
    let trials: u32 = std::env::var("FLOWPROOF_DRAG_TRIALS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(10);

    let mut landed = 0;
    for i in 1..=trials {
        let mut driver = flowproof_adapters::WebAppDriver::new().expect("browser launches");
        // Tall enough that source and target are both in view: the two
        // midpoints have to be read in one layout.
        driver
            .stage_browser(WebBrowserConfig {
                viewport: Some(WebViewport {
                    width: 1280,
                    height: 2400,
                    device_scale_factor: 1.0,
                    mobile: false,
                    touch: false,
                }),
                user_agent: None,
                args: vec![],
                clock: None,
                random: None,
            })
            .expect("viewport staged");
        driver
            .launch(&url, "", std::time::Duration::from_secs(30))
            .expect("page opens");
        if live.is_some() {
            std::thread::sleep(std::time::Duration::from_millis(1500)); // handlers bind
        }
        let ok = driver
            .drag(&UiaSelector::css(&source), &UiaSelector::css(&target))
            .is_ok()
            && driver
                .element_exists(&UiaSelector::css(&probe))
                .unwrap_or(false);
        if ok {
            landed += 1;
        } else {
            eprintln!("trial {i}: NO DROP");
        }
    }
    eprintln!("drag landed {landed}/{trials}");
    assert_eq!(landed, trials, "the dispatch must land every time");
}
