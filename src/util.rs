use ratatui::style::Style;
use unicode_width::UnicodeWidthChar;

/// Cheap token estimate: ~4 chars/tok for ASCII, ~1 char/tok for non-ASCII.
/// Deliberately avoids a tokenizer dependency: fast startup, zero cost.
pub fn est_tokens(s: &str) -> usize {
    let mut a = 0usize;
    let mut n = 0usize;
    for c in s.chars() {
        if c.is_ascii() {
            a += 1;
        } else {
            n += 1;
        }
    }
    a / 4 + n + 1
}

pub fn fmt_tokens(n: u64) -> String {
    if n >= 10_000 {
        format!("{:.0}k", n as f64 / 1000.0)
    } else if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

/// Styled line = sequence of (text, style) fragments. Stored as scrollback.
pub type SLine = Vec<(String, Style)>;

pub fn sline(text: impl Into<String>, style: Style) -> SLine {
    vec![(text.into(), style)]
}

fn ansi_re() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r"\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b[@-Z\\-_]")
            .unwrap()
    })
}

/// Strip ANSI escapes and non-printable chars, expand tabs.
pub fn sanitize(s: &str) -> String {
    let no_ansi = ansi_re().replace_all(s, "");
    let mut out = String::with_capacity(no_ansi.len());
    for c in no_ansi.chars() {
        match c {
            '\t' => out.push_str("    "),
            '\n' => out.push('\n'),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// Word-agnostic hard wrap of a styled line at display width `w`.
pub fn wrap_sline(line: &SLine, w: usize) -> Vec<SLine> {
    if w == 0 {
        return vec![Vec::new()];
    }
    let mut out: Vec<SLine> = Vec::new();
    let mut cur: SLine = Vec::new();
    let mut cur_w = 0usize;
    for (text, style) in line {
        let mut buf = String::new();
        for ch in text.chars() {
            let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
            if cur_w + cw > w && cur_w > 0 {
                cur.push((std::mem::take(&mut buf), *style));
                out.push(std::mem::take(&mut cur));
                cur_w = 0;
            }
            buf.push(ch);
            cur_w += cw;
        }
        if !buf.is_empty() {
            cur.push((buf, *style));
        }
    }
    out.push(cur);
    out
}

/// Truncate to `max` chars on one line for compact display.
pub fn one_line(s: &str, max: usize) -> String {
    let flat: String = s.chars().map(|c| if c == '\n' { '⏎' } else { c }).collect();
    let mut out = String::new();
    for (i, c) in flat.chars().enumerate() {
        if i >= max {
            out.push('…');
            return out;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_est() {
        assert!(est_tokens("hello world") < 6);
        assert!(est_tokens("你好世界") >= 4);
    }

    #[test]
    fn wrap_basic() {
        let l = sline("abcdefgh", Style::default());
        let w = wrap_sline(&l, 3);
        assert_eq!(w.len(), 3);
        assert_eq!(w[0][0].0, "abc");
    }

    #[test]
    fn sanitize_strips() {
        assert_eq!(sanitize("\x1b[31mhi\x1b[0m"), "hi");
        assert_eq!(sanitize("a\tb"), "a    b");
    }
}
