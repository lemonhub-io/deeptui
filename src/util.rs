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
        regex::Regex::new(
            r"\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b[@-Z\\-_]",
        )
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

fn char_w(c: char) -> usize {
    UnicodeWidthChar::width(c).unwrap_or(0)
}

fn span_w(s: &str) -> usize {
    s.chars().map(char_w).sum()
}

/// True for gutter spans (indent, bullets, list numbers, gutter bars) that must
/// be repeated on every wrapped row so hanging indents stay aligned.
fn is_prefix_text(t: &str) -> bool {
    if t.is_empty() {
        return false;
    }
    let bar = |c: char| "▎│─┌┬┐├┼┤└┴┘".contains(c);
    if t.chars().all(bar) || t.chars().all(|c| c.is_whitespace()) {
        return true;
    }
    let glyph = |c: char| {
        bar(c)
            || c.is_whitespace()
            || "•◦▪☐☑◆❯●".contains(c)
            || c.is_ascii_digit()
            || c == '.'
            || c == ')'
    };
    t.ends_with(' ') && t.chars().all(glyph)
}

fn split_prefix(line: &SLine) -> (SLine, SLine) {
    let mut n = 0;
    for (t, _) in line {
        if !is_prefix_text(t) {
            break;
        }
        n += 1;
    }
    (line[..n].to_vec(), line[n..].to_vec())
}

/// True for a list-marker span (`• `, `2. `) or the turn marker, which
/// continuation rows blank out so wrapped item text aligns under the item text.
fn is_droppable_marker(t: &str) -> bool {
    let body = t.trim();
    if body.is_empty() {
        return false;
    }
    if body.chars().any(|c| "•◦▪☐☑❯●".contains(c)) {
        return true;
    }
    let digits = body.chars().take_while(char::is_ascii_digit).count();
    digits > 0 && matches!(&body[digits..], "." | ")")
}

/// Word-aware wrap of a styled line at display width `w`. Any gutter prefix and
/// the content's leading indent are repeated on continuation rows, so code
/// blocks, bullets and quotes keep their shape; words longer than a row are
/// hard-split.
pub fn wrap_sline(line: &SLine, w: usize) -> Vec<SLine> {
    if w == 0 {
        return vec![Vec::new()];
    }
    let (prefix, content) = split_prefix(line);
    let pw: usize = prefix.iter().map(|(t, _)| span_w(t)).sum();
    let avail = w.saturating_sub(pw).max(1);

    let mut lead = String::new();
    let mut chars: Vec<(char, Style)> = Vec::new();
    for (i, (t, st)) in content.iter().enumerate() {
        let mut s = t.as_str();
        if i == 0 {
            let tr = s.trim_start();
            lead.push_str(&s[..s.len() - tr.len()]);
            s = tr;
        }
        for c in s.chars() {
            chars.push((c, *st));
        }
    }
    if chars.is_empty() {
        let mut row = prefix;
        if !lead.is_empty() {
            row.push((lead, Style::default()));
        }
        return vec![row];
    }
    let lead_w = span_w(&lead);
    // room left for content text on each row
    let room = avail.saturating_sub(lead_w).max(1);

    // greedy word wrap over char indices: (begin, end) ranges per row
    let mut rows: Vec<(usize, usize)> = Vec::new();
    let mut cur: Option<(usize, usize)> = None;
    let (mut cw, mut gap) = (0usize, 0usize);
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i].0 == ' ' {
            gap += char_w(chars[i].0);
            i += 1;
            continue;
        }
        let start = i;
        let mut ww = 0usize;
        while i < chars.len() && chars[i].0 != ' ' {
            ww += char_w(chars[i].0);
            i += 1;
        }
        if let Some((b, e)) = cur {
            if cw + gap + ww > room {
                rows.push((b, e));
                cur = None;
                gap = 0;
            }
        }
        if cur.is_none() {
            cw = 0;
        }
        if ww + cw > room {
            // word too long for the room left: break it by character
            if let Some((b, e)) = cur.take() {
                rows.push((b, e));
            }
            let (mut begin, mut acc) = (start, 0usize);
            for (off, (ch, _)) in chars[start..i].iter().enumerate() {
                let j = start + off;
                let cwid = char_w(*ch);
                if acc + cwid > room && acc > 0 {
                    rows.push((begin, j));
                    begin = j;
                    acc = 0;
                }
                acc += cwid;
            }
            cur = Some((begin, i));
            cw = acc;
            gap = 0;
            continue;
        }
        cur = Some((cur.map_or(start, |(b, _)| b), i));
        cw += gap + ww;
        gap = 0;
    }
    if let Some((b, e)) = cur {
        rows.push((b, e));
    }
    if rows.is_empty() {
        rows.push((0, 0));
    }

    rows.iter()
        .enumerate()
        .map(|(n, (b, e))| {
            let mut row = prefix.clone();
            if n > 0 {
                // markers and list numbers are not repeated on continuation rows
                for (t, st) in row.iter_mut() {
                    if is_droppable_marker(t) {
                        *t = " ".repeat(span_w(t));
                        *st = Style::default();
                    }
                }
            }
            if !lead.is_empty() {
                row.push((lead.clone(), Style::default()));
            }
            let mut buf = String::new();
            let mut st = Style::default();
            let mut init = false;
            for (c, s) in &chars[*b..*e] {
                if init && *s != st {
                    row.push((std::mem::take(&mut buf), st));
                }
                buf.push(*c);
                st = *s;
                init = true;
            }
            if !buf.is_empty() {
                row.push((buf, st));
            }
            row
        })
        .collect()
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
