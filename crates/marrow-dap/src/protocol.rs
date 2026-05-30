//! DAP stdio wire framing: `Content-Length`-delimited JSON, the same envelope
//! family as LSP. Hand-rolled to the subset the debugger needs — no codegen, no
//! schema crate — matching the anti-bloat posture of the MCP transport.

use std::io::{BufRead, Write};

use serde_json::Value as Json;

/// Read one `Content-Length`-framed JSON message from `input`, or `None` at clean
/// end of stream. A header block (`Key: Value` lines) ends at a blank line; the
/// body is exactly `Content-Length` bytes of JSON. Any header other than the
/// length is ignored, as DAP permits.
pub fn read_message(input: &mut impl BufRead) -> Option<Json> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        let read = input.read_line(&mut line).ok()?;
        if read == 0 {
            // EOF before any header: the client closed the stream.
            return None;
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some(value) = trimmed.strip_prefix("Content-Length:") {
            content_length = value.trim().parse().ok();
        }
    }
    let length = content_length?;
    let mut body = vec![0u8; length];
    input.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

/// Write one `Content-Length`-framed JSON message to `out` and flush, so the
/// client reads it promptly.
pub fn write_message(out: &mut impl Write, message: &Json) {
    let body = serde_json::to_vec(message).expect("a JSON message serializes");
    let _ = write!(out, "Content-Length: {}\r\n\r\n", body.len());
    let _ = out.write_all(&body);
    let _ = out.flush();
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_framed_message_round_trips() {
        let message = json!({ "seq": 1, "type": "request", "command": "initialize" });
        let mut buffer: Vec<u8> = Vec::new();
        write_message(&mut buffer, &message);
        // The framing carries the exact byte length and a blank-line separator.
        let text = String::from_utf8(buffer.clone()).unwrap();
        assert!(text.starts_with("Content-Length: "));
        assert!(text.contains("\r\n\r\n"));

        let mut reader = std::io::Cursor::new(buffer);
        let read = read_message(&mut reader).unwrap();
        assert_eq!(read, message);
    }

    #[test]
    fn two_messages_read_in_order() {
        let first = json!({ "seq": 1 });
        let second = json!({ "seq": 2 });
        let mut buffer: Vec<u8> = Vec::new();
        write_message(&mut buffer, &first);
        write_message(&mut buffer, &second);
        let mut reader = std::io::Cursor::new(buffer);
        assert_eq!(read_message(&mut reader), Some(first));
        assert_eq!(read_message(&mut reader), Some(second));
        assert_eq!(read_message(&mut reader), None);
    }

    #[test]
    fn end_of_stream_yields_none() {
        let mut reader = std::io::Cursor::new(Vec::new());
        assert_eq!(read_message(&mut reader), None);
    }

    #[test]
    fn unknown_headers_are_ignored() {
        let body = b"{\"seq\":5}";
        let mut framed = format!(
            "Content-Type: application/vscode-jsonrpc; charset=utf-8\r\nContent-Length: {}\r\n\r\n",
            body.len()
        )
        .into_bytes();
        framed.extend_from_slice(body);
        let mut reader = std::io::Cursor::new(framed);
        assert_eq!(read_message(&mut reader), Some(json!({ "seq": 5 })));
    }
}
