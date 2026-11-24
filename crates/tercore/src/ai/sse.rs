//! Server-Sent Events framing for the streaming completions endpoint.
//!
//! Parsing is a pure function over a byte buffer so it can be tested without a
//! network: chunks may split events at any byte, including inside a UTF-8
//! sequence.

/// Extracts complete `data:` payloads from `buf`, leaving any trailing partial
/// event in the buffer. Returns payloads in order; `[DONE]` is passed through
/// so the caller can stop.
pub fn drain_events(buf: &mut Vec<u8>) -> Vec<String> {
    let mut out = Vec::new();
    loop {
        let Some((end, sep_len)) = find_event_boundary(buf) else {
            break;
        };
        let raw: Vec<u8> = buf.drain(..end + sep_len).collect();
        let text = String::from_utf8_lossy(&raw[..end]).to_string();
        let mut data_lines = Vec::new();
        for line in text.split('\n') {
            let line = line.trim_end_matches('\r');
            if line.is_empty() || line.starts_with(':') {
                continue;
            }
            if let Some(rest) = line.strip_prefix("data:") {
                data_lines.push(rest.strip_prefix(' ').unwrap_or(rest).to_string());
            }
            // `event:`, `id:` and `retry:` are irrelevant for this endpoint.
        }
        if !data_lines.is_empty() {
            out.push(data_lines.join("\n"));
        }
    }
    out
}

fn find_event_boundary(buf: &[u8]) -> Option<(usize, usize)> {
    // Prefer CRLF when it comes first, otherwise LF-LF.
    let mut i = 0;
    while i + 1 < buf.len() {
        if buf[i] == b'\n' && buf[i + 1] == b'\n' {
            return Some((i, 2));
        }
        if i + 3 < buf.len() && &buf[i..i + 4] == b"\r\n\r\n" {
            return Some((i, 4));
        }
        i += 1;
    }
    None
}

/// Parses a single `data:` payload, tolerating keep-alives.
pub fn is_done(payload: &str) -> bool {
    payload.trim() == "[DONE]"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_multiple_events() {
        let mut buf = b"data: {\"a\":1}\n\ndata: {\"b\":2}\n\n".to_vec();
        let events = drain_events(&mut buf);
        assert_eq!(events, vec!["{\"a\":1}", "{\"b\":2}"]);
        assert!(buf.is_empty());
    }

    #[test]
    fn keeps_partial_event_in_buffer() {
        let mut buf = b"data: {\"a\":1}\n\ndata: {\"b\":".to_vec();
        let events = drain_events(&mut buf);
        assert_eq!(events, vec!["{\"a\":1}"]);
        assert_eq!(String::from_utf8(buf).unwrap(), "data: {\"b\":");
    }

    #[test]
    fn handles_crlf_and_comments() {
        let mut buf = b": keep-alive\r\n\r\ndata: hello\r\n\r\n".to_vec();
        let events = drain_events(&mut buf);
        assert_eq!(events, vec!["hello"]);
    }

    #[test]
    fn joins_multiline_data() {
        let mut buf = b"data: line1\ndata: line2\n\n".to_vec();
        let events = drain_events(&mut buf);
        assert_eq!(events, vec!["line1\nline2"]);
    }

    #[test]
    fn detects_done_sentinel() {
        assert!(is_done("[DONE]"));
        assert!(is_done(" [DONE] \n"));
        assert!(!is_done("{\"x\":1}"));
    }

    #[test]
    fn handles_split_utf8_sequence() {
        let full = "data: {\"t\":\"héllo 🎮\"}\n\n".as_bytes().to_vec();
        let mut buf = Vec::new();
        let mut events = Vec::new();
        // Feed one byte at a time: the parser must never panic or corrupt text.
        for byte in full {
            buf.push(byte);
            events.extend(drain_events(&mut buf));
        }
        assert_eq!(events.len(), 1);
        assert!(events[0].contains("héllo 🎮"));
    }
}
