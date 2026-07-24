//! `flowproof capture`: a byte-fidelity HTTP capture endpoint.
//!
//! Point a tool-under-test's HTTP connection at this server instead of its
//! real target. It logs every request BYTE FOR BYTE - method, path, all
//! headers, the raw body as text AND as a hexdump - and saves each one to a
//! file, so you can see exactly how the tool serialized the payload (in
//! particular SAP namespace-style `/BA1/..` field names). It answers `200`
//! with a plain ack so the send side completes, isolating whether the
//! problem is what the tool SENDS (not what the target replies).
//!
//! Ported from a stdlib Python capture server. The posture matches
//! `flowproof-adapters`' hand-rolled proxy: a plain [`TcpListener`], std
//! threads, `Connection: close`, read the `Content-Length` body exactly. No
//! async runtime, no HTTP framework. Byte fidelity is the whole point:
//! nothing is reserialized, header casing and order are preserved, and the
//! raw bytes are saved and hexdumped verbatim so a serialization bug cannot
//! hide behind pretty-printing.

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::EXIT_PASS;

/// The largest request body the endpoint will read. A guard against a
/// malformed `content-length`, not a limit a real capture should reach.
const MAX_BODY: usize = 64 * 1024 * 1024;

/// One captured request, kept exactly as it arrived on the wire.
pub struct Captured {
    pub method: String,
    pub path: String,
    pub version: String,
    /// Headers in wire order, raw casing preserved: `(name, value)` pairs.
    pub headers: Vec<(String, String)>,
    /// The raw body bytes, unmodified.
    pub body: Vec<u8>,
}

/// Extract SAP namespace-style field names matching the regex
/// `/[A-Za-z0-9_]+/[A-Za-z0-9_]+` (e.g. `/BA1/C55APPL`) from the raw body,
/// so serialization mangling of those names jumps out. Scanned on bytes for
/// fidelity (never on a decoded string that could have dropped a byte),
/// deduplicated and sorted, matching the reference's non-overlapping,
/// left-to-right behavior. Every matched byte is ASCII (`/` or a word byte),
/// so the hit maps to a `String` without loss.
pub fn namespace_fields(body: &[u8]) -> Vec<String> {
    fn word_byte(b: u8) -> bool {
        b.is_ascii_alphanumeric() || b == b'_'
    }
    let mut hits: BTreeSet<String> = BTreeSet::new();
    let n = body.len();
    let mut i = 0;
    while i < n {
        if body[i] == b'/' {
            let seg1 = i + 1;
            let mut j = seg1;
            while j < n && word_byte(body[j]) {
                j += 1;
            }
            // Need at least one word byte, then a separating `/`.
            if j > seg1 && j < n && body[j] == b'/' {
                let seg2 = j + 1;
                let mut k = seg2;
                while k < n && word_byte(body[k]) {
                    k += 1;
                }
                if k > seg2 {
                    let hit: String = body[i..k].iter().map(|&b| b as char).collect();
                    hits.insert(hit);
                    // Advance past the whole match, as `findall` does.
                    i = k;
                    continue;
                }
            }
        }
        i += 1;
    }
    hits.into_iter().collect()
}

/// A canonical hexdump: 16 bytes per row as `offset  hex bytes  ascii`,
/// non-printable bytes shown as `.`. Ported verbatim from the reference so
/// the saved files read the same. No trailing newline.
pub fn hexdump(data: &[u8]) -> String {
    const WIDTH: usize = 16;
    let mut lines: Vec<String> = Vec::new();
    for (row, chunk) in data.chunks(WIDTH).enumerate() {
        let offset = row * WIDTH;
        let hexs = chunk
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(" ");
        let text: String = chunk
            .iter()
            .map(|&b| {
                if (32..127).contains(&b) {
                    b as char
                } else {
                    '.'
                }
            })
            .collect();
        lines.push(format!(
            "{offset:08x}  {hexs:<width$}  {text}",
            width = WIDTH * 3
        ));
    }
    lines.join("\n")
}

/// The human-readable capture report written to `req-NNN.txt` and printed to
/// stdout. `timestamp` is injected so the format is testable. The body is
/// rendered as text (UTF-8, falling back to a lossy latin-1 view with a note
/// so no byte is dropped) AND as a hexdump: the text is for reading, the
/// hexdump is the byte-fidelity ground truth.
pub fn human_report(req: &Captured, n: usize, timestamp: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!("=== request #{n}  {timestamp} ===\n"));
    out.push_str(&format!("{} {} {}\n\n", req.method, req.path, req.version));
    out.push_str("-- headers --\n");
    for (name, value) in &req.headers {
        out.push_str(&format!("{name}: {value}\n"));
    }
    out.push('\n');
    out.push_str(&format!("-- body ({} bytes) --\n", req.body.len()));
    match std::str::from_utf8(&req.body) {
        Ok(text) => out.push_str(text),
        Err(_) => {
            // Latin-1: every byte maps to a char, so nothing is lost; the
            // hexdump below remains the authoritative view.
            let lossy: String = req.body.iter().map(|&b| b as char).collect();
            out.push_str(&lossy);
            out.push_str("\n[non-utf8: see hexdump]");
        }
    }
    out.push('\n');
    out.push('\n');
    out.push_str("-- namespace-style field names found (/X/Y) --\n");
    let ns = namespace_fields(&req.body);
    if ns.is_empty() {
        out.push_str("(none - already stripped?)");
    } else {
        out.push_str(&ns.join("\n"));
    }
    out.push('\n');
    out.push('\n');
    out.push_str("-- body hexdump --\n");
    out.push_str(&hexdump(&req.body));
    out
}

/// The structured `--json` report for one request: method, path, headers as
/// an ordered list of `[name, value]` pairs (raw casing/order preserved),
/// body length, the extracted namespace fields, and the saved file path.
/// The body itself lives byte-for-byte in the saved file, not inlined here.
pub fn json_report(req: &Captured, n: usize, saved: &Path) -> serde_json::Value {
    let headers: Vec<serde_json::Value> = req
        .headers
        .iter()
        .map(|(name, value)| serde_json::json!([name, value]))
        .collect();
    serde_json::json!({
        "request": n,
        "method": req.method,
        "path": req.path,
        "http_version": req.version,
        "headers": headers,
        "body_len": req.body.len(),
        "namespace_fields": namespace_fields(&req.body),
        "saved": saved,
    })
}

/// Read the request line, all headers (verbatim), and exactly
/// `Content-Length` bytes of body. Returns `None` on a malformed head or a
/// declared length past [`MAX_BODY`]. Header names keep their raw casing and
/// order; values are trimmed of surrounding whitespace only.
fn read_request(reader: &mut BufReader<TcpStream>) -> Option<Captured> {
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).ok()? == 0 {
        return None;
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();
    // A version may be absent on a bare HTTP/0.9-ish line; default it.
    let version = parts.next().unwrap_or("HTTP/1.1").to_string();

    let mut headers: Vec<(String, String)> = Vec::new();
    let mut length = 0usize;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).ok()? == 0 {
            return None;
        }
        let header = header.trim_end_matches(['\r', '\n']);
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            let value = value.trim();
            if name.eq_ignore_ascii_case("content-length") {
                length = value.parse().ok()?;
            }
            // Preserve the name exactly as sent; do not lowercase it.
            headers.push((name.to_string(), value.to_string()));
        }
    }
    if length > MAX_BODY {
        return None;
    }
    let mut body = vec![0u8; length];
    if length > 0 {
        reader.read_exact(&mut body).ok()?;
    }
    Some(Captured {
        method,
        path,
        version,
        headers,
        body,
    })
}

/// A `200 OK` with a plain-text ack body, so the send side completes.
fn ack_response(n: usize) -> Vec<u8> {
    let body = format!("captured request #{n}");
    format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/plain; charset=utf-8\r\n\
         content-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

/// A minimal `400` for a request whose head could not be read at all.
fn bad_request() -> Vec<u8> {
    let body = "malformed request";
    format!(
        "HTTP/1.1 400 Bad Request\r\ncontent-type: text/plain; charset=utf-8\r\n\
         content-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

/// Send `bytes`, then close the write half cleanly and drain whatever the
/// client still had in flight - the same orderly-shutdown dance the agent
/// proxy uses to avoid an RST costing the client its answer on Windows.
fn respond(writer: &mut TcpStream, reader: &mut BufReader<TcpStream>, bytes: &[u8]) {
    let _ = writer.write_all(bytes);
    let _ = writer.flush();
    let _ = writer.shutdown(std::net::Shutdown::Write);
    // Bound the drain with a short read timeout: a client that reads its
    // answer but holds the socket open must not block the single accept
    // loop, which would stall capturing every later request. A timeout
    // surfaces as an `Err`, which ends the loop.
    let _ = reader
        .get_ref()
        .set_read_timeout(Some(std::time::Duration::from_millis(200)));
    let mut sink = [0u8; 4096];
    for _ in 0..16 {
        match reader.read(&mut sink) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
    }
}

/// Handle one connection: read the request, save + print it, ack `200`.
/// Returns the [`Captured`] request and its saved path so tests can assert
/// the outcome without reaching back through the socket. `None` on a
/// malformed request (a `400` is still sent).
fn serve_one(
    stream: TcpStream,
    n: usize,
    out_dir: &Path,
    json: bool,
) -> Option<(Captured, PathBuf)> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut writer = stream;

    let Some(req) = read_request(&mut reader) else {
        respond(&mut writer, &mut reader, &bad_request());
        return None;
    };

    let saved = out_dir.join(format!("req-{n:03}.txt"));
    let timestamp = chrono::Local::now()
        .format("%Y-%m-%dT%H:%M:%S%.3f")
        .to_string();
    let report = human_report(&req, n, &timestamp);
    // Write the byte-fidelity text file; a write failure is noted, not fatal
    // - capturing the next request still has value.
    if let Err(e) = std::fs::write(&saved, &report) {
        eprintln!("warning: could not write {}: {e}", saved.display());
    }

    if json {
        // One structured object per request (JSON Lines): a long-lived
        // listener cannot emit a single closing array.
        println!("{}", json_report(&req, n, &saved));
    } else {
        println!("\n{report}");
        println!("\n[saved -> {}]\n{}", saved.display(), "-".repeat(70));
    }
    let _ = std::io::stdout().flush();

    respond(&mut writer, &mut reader, &ack_response(n));
    Some((req, saved))
}

/// The accept loop: serve connections until `stop` is set, polling so the
/// stop flag is noticed even when no client connects. Factored out so a test
/// can run it against an ephemeral loopback listener.
fn capture_loop(listener: &TcpListener, out_dir: &Path, json: bool, stop: &AtomicBool) {
    let _ = listener.set_nonblocking(true);
    let mut n = 0usize;
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                let _ = stream.set_nonblocking(false);
                n += 1;
                serve_one(stream, n, out_dir, json);
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(_) => break,
        }
    }
}

/// `flowproof capture --port <N> [--out <DIR>] [--json]`. Binds `0.0.0.0` so
/// a sender on another machine can reach it, prints a one-line security
/// warning, and runs until Ctrl-C.
pub fn cmd_capture(port: u16, out: Option<PathBuf>, json: bool) -> Result<u8, String> {
    let out_dir = out.unwrap_or_else(|| PathBuf::from("./captured"));
    std::fs::create_dir_all(&out_dir)
        .map_err(|e| format!("could not create out dir {}: {e}", out_dir.display()))?;

    // Bind all interfaces: the motivating case is a sender (a remote API-testing tool) on
    // another machine, or the same box. That reach is deliberate and comes
    // with a real exposure, so it is stated plainly on stderr - keeping
    // stdout clean for the human reports or the `--json` stream.
    let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, port))
        .map_err(|e| format!("could not bind 0.0.0.0:{port}: {e}"))?;
    let bound = listener
        .local_addr()
        .map(|a| a.to_string())
        .unwrap_or_else(|_| format!("0.0.0.0:{port}"));

    eprintln!(
        "WARNING: flowproof capture listens on 0.0.0.0:{port} (ALL network interfaces), is \
         UNAUTHENTICATED, and saves raw request bodies to {}. Anyone who can reach this port \
         can make it write to disk - run it deliberately and stop it (Ctrl-C) when done.",
        out_dir.display()
    );
    eprintln!(
        "flowproof capture listening on http://{bound}/  (saving to {})",
        out_dir.display()
    );
    eprintln!(
        "Point the tool-under-test's endpoint URL here, run it, watch below. Ctrl-C to stop."
    );

    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = Arc::clone(&stop);
        // A clean Ctrl-C: flip the flag, the loop drains and returns 0. Files
        // are flushed per request, so nothing is lost at interrupt time.
        if let Err(e) = ctrlc::set_handler(move || stop.store(true, Ordering::Relaxed)) {
            eprintln!("warning: could not install Ctrl-C handler ({e}); will exit on kill");
        }
    }

    capture_loop(&listener, &out_dir, json, &stop);
    eprintln!("\nstopped.");
    let _ = std::io::stdout().flush();
    // A clean, deliberate Ctrl-C stop is a success, not an error.
    Ok(EXIT_PASS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cap(body: &[u8]) -> Captured {
        Captured {
            method: "POST".into(),
            path: "/".into(),
            version: "HTTP/1.1".into(),
            headers: vec![
                ("Content-Type".into(), "application/json".into()),
                ("SOAPAction".into(), "urn:sap-com:document".into()),
            ],
            body: body.to_vec(),
        }
    }

    #[test]
    fn namespace_regex_matches_sap_names() {
        // The motivating SAP name, and a minimal two-segment name.
        assert_eq!(
            namespace_fields(b"foo /BA1/C55APPL bar"),
            vec!["/BA1/C55APPL".to_string()]
        );
        assert_eq!(namespace_fields(b"x/A/B"), vec!["/A/B".to_string()]);
        // Non-matches: a lone segment, no slashes at all.
        assert!(namespace_fields(b"/onlyone").is_empty());
        assert!(namespace_fields(b"no slashes here").is_empty());
        // A leading empty segment (`//A/B`) still yields the real `/A/B`.
        assert_eq!(namespace_fields(b"//A/B here"), vec!["/A/B".to_string()]);
    }

    #[test]
    fn namespace_fields_dedupe_and_sort() {
        let hits = namespace_fields(b"/BA1/C55APPL then /A/B then /BA1/C55APPL again");
        assert_eq!(hits, vec!["/A/B".to_string(), "/BA1/C55APPL".to_string()]);
    }

    #[test]
    fn hexdump_is_faithful_to_raw_bytes() {
        // Embedded NUL and a high byte must survive as hex and render as `.`.
        let dump = hexdump(b"AB\x00\xff");
        assert!(dump.starts_with("00000000  "), "offset prefix: {dump}");
        assert!(dump.contains("41 42 00 ff"), "hex bytes: {dump}");
        assert!(dump.trim_end().ends_with("AB.."), "ascii gutter: {dump}");
    }

    #[test]
    fn human_report_has_all_sections_and_namespace_hit() {
        let req = cap(br#"{"/BA1/C55APPL":"x"}"#);
        let report = human_report(&req, 7, "2026-07-22T10:00:00.000");
        assert!(report.contains("=== request #7  2026-07-22T10:00:00.000 ==="));
        assert!(report.contains("POST / HTTP/1.1"));
        assert!(report.contains("Content-Type: application/json"));
        // Raw header casing is preserved verbatim.
        assert!(report.contains("SOAPAction: urn:sap-com:document"));
        assert!(report.contains(r#"{"/BA1/C55APPL":"x"}"#));
        assert!(report.contains("/BA1/C55APPL"));
        assert!(report.contains("-- body hexdump --"));
    }

    #[test]
    fn human_report_flags_no_namespace_fields() {
        let report = human_report(&cap(b"plain body"), 1, "t");
        assert!(report.contains("(none - already stripped?)"));
    }

    #[test]
    fn json_report_shape_is_correct() {
        let req = cap(br#"{"/BA1/C55APPL":"x"}"#);
        let value = json_report(&req, 3, Path::new("captured/req-003.txt"));
        assert_eq!(value["request"], 3);
        assert_eq!(value["method"], "POST");
        assert_eq!(value["path"], "/");
        assert_eq!(value["http_version"], "HTTP/1.1");
        assert_eq!(value["body_len"], 20);
        assert_eq!(value["headers"][0][0], "Content-Type");
        assert_eq!(value["headers"][0][1], "application/json");
        // Raw casing preserved in the structured output too.
        assert_eq!(value["headers"][1][0], "SOAPAction");
        assert_eq!(value["namespace_fields"][0], "/BA1/C55APPL");
        assert_eq!(value["saved"], "captured/req-003.txt");
    }

    /// End to end over a real socket on an ephemeral port: send a SAP-ish
    /// body with a namespace field and an embedded control byte, assert the
    /// `req-NNN.txt` file is written with every section and byte-faithful,
    /// and that the response is a plain `200`.
    #[test]
    fn captures_a_request_over_http() {
        let dir = std::env::temp_dir().join(format!("fp-capture-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
        let addr = listener.local_addr().expect("addr");
        let stop = Arc::new(AtomicBool::new(false));

        let handle = {
            let (dir, stop) = (dir.clone(), Arc::clone(&stop));
            std::thread::spawn(move || capture_loop(&listener, &dir, false, &stop))
        };

        // A body with an embedded NUL: byte fidelity has to survive it.
        let body = b"{\"/BA1/C55APPL\":\"\x00val\"}";
        let mut stream = TcpStream::connect(addr).expect("connect");
        let request = format!(
            "POST /sap/rfc HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\n\
             X-Weird-Case: KeepMe\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        stream.write_all(request.as_bytes()).expect("write head");
        stream.write_all(body).expect("write body");
        stream.flush().expect("flush");

        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).expect("read response");
        let response = String::from_utf8_lossy(&raw);
        assert!(
            response.starts_with("HTTP/1.1 200 OK"),
            "200 ack: {response}"
        );
        assert!(
            response.contains("captured request #1"),
            "ack body: {response}"
        );

        stop.store(true, Ordering::Relaxed);
        handle.join().expect("join");

        let saved = dir.join("req-001.txt");
        let text = std::fs::read(&saved).expect("saved file exists");
        let text = String::from_utf8_lossy(&text);
        assert!(
            text.contains("POST /sap/rfc HTTP/1.1"),
            "method/path: {text}"
        );
        assert!(
            text.contains("Content-Type: application/json"),
            "header: {text}"
        );
        // Raw, unusual header casing is preserved.
        assert!(text.contains("X-Weird-Case: KeepMe"), "raw casing: {text}");
        assert!(text.contains("/BA1/C55APPL"), "namespace field: {text}");
        // The embedded NUL is hexdumped as 00 and never mangled the file.
        assert!(
            text.contains("-- body hexdump --"),
            "hexdump section: {text}"
        );
        assert!(text.contains(" 00 "), "NUL byte in hexdump: {text}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
