//! Server-sent-event framing: arbitrary byte chunks in, `data:` payloads out.

/// Incremental SSE parser: feeds on arbitrary byte chunks, yields `data:` payloads.
///
/// Buffers raw bytes and decodes only complete events: a multi-byte char split
/// across TCP chunks would otherwise be mangled by per-chunk lossy decoding.
/// Events end at a blank line — `\n\n` or `\r\n\r\n` (CRLF providers).
#[derive(Default)]
pub struct Sse {
    buf: Vec<u8>,
}

/// Cap on the unconsumed buffer; a stream that never emits a blank line
/// can't grow memory forever.
const SSE_BUF_MAX: usize = 1 << 20;

impl Sse {
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        loop {
            let lf = self.buf.windows(2).position(|w| w == b"\n\n");
            let crlf = self.buf.windows(4).position(|w| w == b"\r\n\r\n");
            let (end, skip) = match (lf, crlf) {
                (Some(a), Some(b)) if b < a => (b, 4),
                (Some(a), _) => (a, 2),
                (None, Some(b)) => (b, 4),
                (None, None) => {
                    if self.buf.len() > SSE_BUF_MAX {
                        self.buf.clear();
                    }
                    break;
                }
            };
            let block = String::from_utf8_lossy(&self.buf[..end]).into_owned();
            self.buf.drain(..end + skip);
            let mut data = String::new();
            for l in block.lines() {
                if let Some(d) = l.strip_prefix("data:") {
                    if !data.is_empty() {
                        data.push('\n');
                    }
                    data.push_str(d.trim_start());
                }
            }
            if !data.is_empty() {
                out.push(data);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_splits_events() {
        let mut s = Sse::default();
        assert!(!s.feed(b"data: {\"a\":1}\n\nda").is_empty());
        let rest = s.feed(b"ta: {\"b\":2}\n\n");
        assert_eq!(rest.len(), 1);
    }

    #[test]
    fn sse_handles_crlf_and_split_multibyte() {
        // CRLF separators (some providers) must delimit events
        let mut s = Sse::default();
        let evs = s.feed(b"data: {\"a\":1}\r\n\r\ndata: {\"b\":2}\r\n\r\n");
        assert_eq!(evs.len(), 2);

        // a multi-byte char split across chunks must survive intact —
        // 你 is E4 BD A0; splitting mid-char used to emit replacement chars
        let mut s = Sse::default();
        let payload = "data: 你\n\n";
        let b = payload.as_bytes();
        let split = b.iter().position(|&c| c == 0xA0).unwrap();
        assert!(s.feed(&b[..split]).is_empty());
        let out = s.feed(&b[split..]);
        assert_eq!(out, vec!["你".to_string()]);
    }

    #[test]
    fn sse_caps_runaway_buffer() {
        // no event boundary ever arrives: the buffer must stay bounded
        let mut s = Sse::default();
        for _ in 0..40 {
            assert!(s.feed(&vec![b'x'; 64 * 1024]).is_empty());
        }
        // after the cap, a fresh event still parses — \n ends the garbage
        // line so "data: ok" is its own SSE field
        let out = s.feed(b"\ndata: ok\n\n");
        assert_eq!(out, vec!["ok".to_string()]);
    }

    #[test]
    fn sse_ignores_non_data_fields_and_picks_up_later_events() {
        let mut s = Sse::default();
        // comments and `event:` lines carry no payload; the block is consumed
        assert!(s.feed(b": keepalive\nevent: ping\n\n").is_empty());
        // ...but the stream keeps working afterwards
        assert_eq!(
            s.feed(b"data: one\ndata: two\n\n"),
            vec!["one\ntwo".to_string()]
        );
    }

    #[test]
    fn sse_waits_for_a_complete_boundary() {
        let mut s = Sse::default();
        // no blank line yet -> nothing emitted, not even on a lone \n
        assert!(s.feed(b"data: {\"a\":1}").is_empty());
        assert!(s.feed(b"\n").is_empty());
        assert_eq!(s.feed(b"\n"), vec![r#"{"a":1}"#.to_string()]);

        // a CRLF boundary split across chunks is still recognised
        let mut s = Sse::default();
        assert!(s.feed(b"data: x\r\n").is_empty());
        assert_eq!(s.feed(b"\r\n"), vec!["x".to_string()]);

        // leading whitespace after `data:` is trimmed, however much there is
        let mut s = Sse::default();
        assert_eq!(s.feed(b"data:   padded\n\n"), vec!["padded".to_string()]);
    }
}
