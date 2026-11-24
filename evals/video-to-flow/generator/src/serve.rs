//! The fixtures served over plain HTTP on the loopback address, so the app
//! inside `chrome.html`'s iframe is same-origin and a draft can later be
//! recorded against the same address (layer C).

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::PathBuf;

/// Serves `root` on `127.0.0.1:port` from a background thread for as long
/// as the process lives.
pub fn start(root: PathBuf, port: u16) -> anyhow::Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut stream = stream;
            let mut line = String::new();
            if BufReader::new(&stream).read_line(&mut line).is_err() {
                continue;
            }
            let path = line.split_whitespace().nth(1).unwrap_or("/");
            let path = path
                .split('?')
                .next()
                .unwrap_or("/")
                .trim_start_matches('/');
            let (status, body) = match file(&root, path) {
                Some(body) => ("200 OK", body),
                None => ("404 Not Found", b"not found".to_vec()),
            };
            let kind = if path.ends_with(".html") {
                "text/html; charset=utf-8"
            } else {
                "application/octet-stream"
            };
            let head = format!(
                "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\n\
                 Cache-Control: no-store\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
        }
    });
    Ok(())
}

/// A file directly in `root`: no folders, so nothing outside it is served.
fn file(root: &std::path::Path, name: &str) -> Option<Vec<u8>> {
    if name.is_empty() || name.contains(['/', '\\']) || name.starts_with('.') {
        return None;
    }
    std::fs::read(root.join(name)).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_files_in_the_fixture_folder_are_served() {
        let root = std::env::temp_dir();
        assert!(file(&root, "../etc/passwd").is_none());
        assert!(file(&root, ".hidden").is_none());
        assert!(file(&root, "").is_none());
    }
}
