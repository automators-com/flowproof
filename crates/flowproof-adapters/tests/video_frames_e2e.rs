#![cfg(feature = "web")]

//! A recording made in the browser itself: four seconds of a canvas that
//! changes colour once a second, captured with MediaRecorder. A webm from
//! MediaRecorder states no length, the case `video_frames` has to visit the
//! end of the file to learn - and it needs no video checked into the repo.

const RECORDER: &str = r#"(async () => {
    const canvas = document.createElement('canvas');
    canvas.width = 320; canvas.height = 180;
    document.body.appendChild(canvas);
    const g = canvas.getContext('2d');
    const colours = ['#d33', '#3a3', '#33d', '#dd3'];
    let n = 0;
    const paint = () => { g.fillStyle = colours[n % colours.length]; g.fillRect(0, 0, 320, 180); };
    paint();
    const stream = canvas.captureStream(15);
    const chunks = [];
    const rec = new MediaRecorder(stream, { mimeType: 'video/webm' });
    rec.ondataavailable = e => chunks.push(e.data);
    const done = new Promise(ok => rec.onstop = ok);
    rec.start(250);
    const tick = setInterval(() => { n += 1; paint(); }, 1000);
    await new Promise(ok => setTimeout(ok, 4000));
    clearInterval(tick);
    rec.stop();
    await done;
    const bytes = new Uint8Array(await new Blob(chunks, { type: 'video/webm' }).arrayBuffer());
    return Array.from(bytes, b => b.toString(16).padStart(2, '0')).join('');
})()"#;

#[test]
fn a_browser_recording_yields_its_changed_frames_in_order() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        return;
    }
    let browser = headless_chrome::Browser::new(
        headless_chrome::LaunchOptions::default_builder()
            .path(std::env::var_os("CHROME").map(Into::into))
            .sandbox(false)
            .build()
            .expect("launch options"),
    )
    .expect("browser starts");
    let tab = browser.new_tab().expect("tab");
    tab.navigate_to("data:text/html,<body></body>")
        .and_then(|t| t.wait_until_navigated())
        .expect("blank page");
    let encoded = tab
        .evaluate(RECORDER, true)
        .expect("records")
        .value
        .and_then(|v| v.as_str().map(str::to_string))
        .expect("recording as hex");
    let webm: Vec<u8> = (0..encoded.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&encoded[i..i + 2], 16).expect("hex"))
        .collect();
    let path = std::env::temp_dir().join("flowproof video frames e2e.webm");
    std::fs::write(&path, webm).expect("written");

    let info = flowproof_adapters::video_frames::probe(&path).expect("probes");
    assert!(
        (3000..=5500).contains(&info.duration_ms),
        "a 4 s recording read as {} ms",
        info.duration_ms
    );
    assert_eq!((info.width, info.height), (320, 180));

    let mut samples = 0;
    let (_, frames) = flowproof_adapters::video_frames::extract(
        &path,
        &flowproof_adapters::video_frames::FrameOptions::default(),
        &mut |_, total| samples = total,
    )
    .expect("extracts");
    assert!(samples >= 6, "sampled twice a second: {samples}");
    // Four colours: the first frame, each change, and the last.
    assert!(
        (4..=8).contains(&frames.len()),
        "kept {} frames at {:?}",
        frames.len(),
        frames.iter().map(|f| f.at_ms).collect::<Vec<_>>()
    );
    assert_eq!(frames[0].at_ms, 0);
    assert!(frames.windows(2).all(|w| w[0].at_ms < w[1].at_ms));
    assert!(
        frames.iter().all(|f| f.jpeg.starts_with(&[0xff, 0xd8])),
        "every frame is a JPEG"
    );
    std::fs::remove_file(path).ok();
}
