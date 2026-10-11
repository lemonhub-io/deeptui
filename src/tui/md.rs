//! Lightweight markdown → styled lines for assistant output.
//!
//! Two layers: [`md_inline`] styles spans inside one line (code, bold,
//! italic, strike, links, escapes), and [`md_block`] walks a whole message
//! (fences, headings, lists, quotes, rules, indented code, pipe tables).
//! Delimiters that can't close stay literal — a coding agent's output is
//! full of `*args`, `**kwargs` and `2 * 3` that must survive untouched.

use crate::util::SLine;
use ratatui::style::{Modifier, Style};
use unicode_width::UnicodeWidthChar;

use super::{C_ASSIST, C_CODE, C_DIM, C_GUTTER, C_H2, C_H3, C_HEAD, C_ICODE, C_LINK};

/// Style for one run of inline markdown with the currently open spans.
fn md_span_style(code: bool, bold: bool, italic: bool, strike: bool) -> Style {
    let mut st = if code { C_ICODE } else { Style::default() };
    if bold {
        st = st.add_modifier(Modifier::BOLD);
    }
    if italic {
        st = st.add_modifier(Modifier::ITALIC);
    }
    if strike {
        st = st.add_modifier(Modifier::CROSSED_OUT);
    }
    st
}

fn md_flush(out: &mut SLine, buf: &mut String, code: bool, bold: bool, italic: bool, strike: bool) {
    if !buf.is_empty() {
        out.push((
            std::mem::take(buf),
            md_span_style(code, bold, italic, strike),
        ));
    }
}

/// Parse `[label](url)` at the start of `s` → (label, url, chars consumed).
fn md_link(s: &str) -> Option<(String, String, usize)> {
    let rest = s.strip_prefix('[')?;
    let close = rest.find(']')?;
    let label = &rest[..close];
    if label.is_empty() || label.contains(['[', ']']) {
        return None;
    }
    let url_part = rest[close + 1..].strip_prefix('(')?;
    let end = url_part.find(')')?;
    let url = url_part[..end].trim();
    if url.is_empty() || url.contains(char::is_whitespace) {
        return None;
    }
    // chars, not bytes: find() gives byte offsets, the caller steps chars
    let n = label.chars().count() + url_part[..end].chars().count() + 4;
    Some((label.to_string(), url.to_string(), n))
}

/// The char just before the cursor — the end of `buf`, or of the last flushed
/// span when `buf` is empty — is non-space, so a marker may close here.
fn md_prev_non_ws(buf: &str, out: &SLine) -> bool {
    buf.chars()
        .last()
        .or_else(|| out.last().and_then(|(s, _)| s.chars().last()))
        .is_some_and(|p| !p.is_whitespace())
}

/// `rest` holds `cc` somewhere — a usable `**`/`__`/`~~` closer.
fn md_has_pair(rest: &str, c: char) -> bool {
    rest.chars()
        .zip(rest.chars().skip(1))
        .any(|(a, b)| a == c && b == c)
}

/// `rest` holds a standalone `c` (not the edge of an `cc` run) that can close
/// a single-marker span — so `*args` never pairs with a `**` in `**kwargs`.
fn md_has_single(rest: &str, c: char) -> bool {
    let mut prev = false;
    let mut it = rest.chars().peekable();
    while let Some(ch) = it.next() {
        if ch == c && !prev && it.peek() != Some(&c) {
            return true;
        }
        prev = ch == c;
    }
    false
}

/// Style inline spans inside one markdown line: `code`, **bold**, *italic*,
/// ~~strike~~, [text](url) and backslash escapes.
fn md_inline(t: &str) -> SLine {
    let mut out: SLine = Vec::new();
    let mut buf = String::new();
    let (mut bold, mut italic, mut code, mut strike) = (false, false, false, false);
    let mut chars = t.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if !code => {
                if let Some(n) = chars.next() {
                    buf.push(n);
                }
            }
            '`' => {
                // an unmatched backtick stays literal instead of styling the
                // rest of the line as code
                if !code && !chars.clone().any(|x| x == '`') {
                    buf.push('`');
                } else {
                    md_flush(&mut out, &mut buf, code, bold, italic, strike);
                    code = !code;
                }
            }
            '*' | '_' if !code => {
                // flanking rules, simplified: a marker opens only next to a
                // non-space and closes only after a non-space — `2 * 3 * 4`,
                // `a *b` and `*args` keep their asterisks
                let prev_non_ws = md_prev_non_ws(&buf, &out);
                let rest: String = chars.clone().collect();
                if rest.starts_with(c) {
                    // `**` / `__`
                    let after = &rest[c.len_utf8()..];
                    let next_non_ws = after.chars().next().is_some_and(|x| !x.is_whitespace());
                    let closes =
                        (bold && prev_non_ws) || (!bold && next_non_ws && md_has_pair(after, c));
                    if closes {
                        chars.next();
                        md_flush(&mut out, &mut buf, code, bold, italic, strike);
                        bold = !bold;
                    } else {
                        buf.push(c);
                        buf.push(c);
                        chars.next();
                    }
                } else if c == '*' {
                    let next_non_ws = rest.chars().next().is_some_and(|x| !x.is_whitespace());
                    let closes = (italic && prev_non_ws)
                        || (!italic && next_non_ws && md_has_single(&rest, '*'));
                    if closes {
                        md_flush(&mut out, &mut buf, code, bold, italic, strike);
                        italic = !italic;
                    } else {
                        buf.push('*');
                    }
                } else {
                    buf.push(c); // a lone `_` (snake_case) stays literal
                }
            }
            '~' if !code && chars.peek() == Some(&'~') => {
                let rest: String = chars.clone().collect();
                let after = &rest[1..];
                let next_non_ws = after.chars().next().is_some_and(|x| !x.is_whitespace());
                let closes = (strike && md_prev_non_ws(&buf, &out))
                    || (!strike && next_non_ws && md_has_pair(after, '~'));
                if closes {
                    chars.next();
                    md_flush(&mut out, &mut buf, code, bold, italic, strike);
                    strike = !strike;
                } else {
                    buf.push('~');
                    buf.push('~');
                    chars.next();
                }
            }
            '[' if !code => {
                let rest: String = std::iter::once('[').chain(chars.clone()).collect();
                match md_link(&rest) {
                    Some((label, url, n)) => {
                        for _ in 1..n {
                            chars.next();
                        }
                        md_flush(&mut out, &mut buf, code, bold, italic, strike);
                        out.push((label.clone(), C_LINK));
                        if url != label {
                            out.push((format!(" ({url})"), C_DIM));
                        }
                    }
                    None => buf.push(c),
                }
            }
            _ => buf.push(c),
        }
    }
    md_flush(&mut out, &mut buf, code, bold, italic, strike);
    out
}

/// Place the ● turn marker on a line's leading indent.
pub(super) fn mark_line(l: &mut SLine) {
    if l.first().is_some_and(|(s, _)| s == "  ") {
        l[0] = ("● ".into(), C_ASSIST);
    } else {
        l.insert(0, ("● ".into(), C_ASSIST));
    }
}

/// ATX heading level (1..=6) and its text, if `t` is one.
fn md_heading(t: &str) -> Option<(usize, &str)> {
    let n = t.chars().take_while(|c| *c == '#').count();
    if n == 0 || n > 6 {
        return None;
    }
    let rest = &t[n..];
    match rest.chars().next() {
        None => Some((n, "")),
        Some(' ') => Some((n, rest.trim_start())),
        _ => None,
    }
}

/// `(prefix, rest)` for a bullet, task or ordered list item.
fn md_item(t: &str, indent: usize) -> Option<(String, &str)> {
    let depth = (indent / 2).min(2);
    for m in ["- ", "* ", "+ "] {
        if let Some(rest) = t.strip_prefix(m) {
            let bullet = ['•', '◦', '▪'][depth];
            let pfx = format!("  {}{bullet} ", "  ".repeat(depth));
            let (mark, rest) = if let Some(r) = rest.strip_prefix("[ ] ") {
                ("☐ ", r)
            } else if let Some(r) = rest
                .strip_prefix("[x] ")
                .or_else(|| rest.strip_prefix("[X] "))
            {
                ("☑ ", r)
            } else {
                ("", rest)
            };
            return Some((format!("{pfx}{mark}"), rest));
        }
    }
    let digits = t.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0 && digits <= 3 {
        let (num, after) = t.split_at(digits);
        let rest = after
            .strip_prefix('.')
            .or_else(|| after.strip_prefix(')'))
            .and_then(|r| r.strip_prefix(' '));
        if let Some(rest) = rest {
            return Some((format!("  {}{num}. ", "  ".repeat(depth)), rest));
        }
    }
    None
}

/// One raw assistant-text line → styled spans (with left indent).
/// Returns the line and whether it holds visible content (for the ● marker).
fn md_line(raw: &str, in_fence: &mut bool) -> (SLine, bool) {
    let t = raw.trim_start();
    if t.starts_with("```") || t.starts_with("~~~") {
        *in_fence = !*in_fence;
        let lang = t.trim_start_matches(['`', '~']).trim();
        let mut l: SLine = vec![("  ▎ ".into(), C_GUTTER)];
        if !lang.is_empty() {
            l.push((lang.to_string(), C_DIM));
        }
        return (l, true);
    }
    if *in_fence {
        return (
            vec![("  ▎ ".into(), C_GUTTER), (raw.to_string(), C_CODE)],
            !t.is_empty(),
        );
    }
    if t.is_empty() {
        return (Vec::new(), false);
    }
    if let Some((lvl, txt)) = md_heading(t) {
        let st = match lvl {
            1 => C_HEAD,
            2 => C_H2,
            _ => C_H3,
        };
        let mut l: SLine = vec![("  ".into(), Style::default())];
        for (s, x) in md_inline(txt) {
            l.push((s, if x == Style::default() { st } else { x }));
        }
        return (l, true);
    }
    if t.starts_with('>') {
        let (mut rest, mut depth) = (t, 0);
        while let Some(r) = rest.strip_prefix('>') {
            depth += 1;
            rest = r.trim_start();
        }
        let mut l: SLine = vec![(format!("  {}", "▎ ".repeat(depth.min(4))), C_GUTTER)];
        for (s, x) in md_inline(rest) {
            l.push((s, if x == Style::default() { C_DIM } else { x }));
        }
        return (l, true);
    }
    if let Some((pfx, rest)) = md_item(t, raw.len() - t.len()) {
        let mut l: SLine = vec![(pfx, C_DIM)];
        l.extend(md_inline(rest));
        return (l, true);
    }
    let mut l: SLine = vec![("  ".into(), Style::default())];
    l.extend(md_inline(t));
    (l, true)
}

fn md_is_table_row(t: &str) -> bool {
    t.starts_with('|') && t.matches('|').count() >= 2
}

fn md_is_table_sep(t: &str) -> bool {
    md_is_table_row(t)
        && md_cells(t).iter().all(|c| {
            !c.is_empty()
                && c.chars().any(|ch| ch == '-')
                && c.chars().all(|ch| ch == '-' || ch == ':')
        })
}

/// A `---`, `***` or `___` thematic break.
fn md_is_rule(t: &str) -> bool {
    let c = match t.chars().next() {
        Some(c @ ('-' | '*' | '_')) => c,
        _ => return false,
    };
    t.chars().filter(|ch| !ch.is_whitespace()).count() >= 3
        && t.chars().all(|ch| ch.is_whitespace() || ch == c)
}

fn md_cells(row: &str) -> Vec<String> {
    let r = row.replace("\\|", "\u{1}");
    let r = r.trim().trim_start_matches('|').trim_end_matches('|');
    r.split('|')
        .map(|c| c.replace('\u{1}', "|").trim().to_string())
        .collect()
}

#[derive(Clone, Copy, PartialEq)]
enum MdAlign {
    Left,
    Center,
    Right,
}

fn md_spans_w(l: &SLine) -> usize {
    l.iter().map(|(t, _)| md_text_w(t)).sum()
}

fn md_text_w(t: &str) -> usize {
    t.chars()
        .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
        .sum()
}

fn md_fit(l: &SLine, w: usize) -> SLine {
    if md_spans_w(l) <= w {
        return l.clone();
    }
    let mut out: SLine = Vec::new();
    let mut used = 0usize;
    for (t, st) in l {
        let mut buf = String::new();
        for ch in t.chars() {
            let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
            if used + cw > w.saturating_sub(1) {
                if !buf.is_empty() {
                    out.push((buf, *st));
                }
                out.push(("…".into(), C_DIM));
                return out;
            }
            buf.push(ch);
            used += cw;
        }
        if !buf.is_empty() {
            out.push((buf, *st));
        }
    }
    out
}

/// Render a pipe table as a boxed, column-aligned table.
fn md_table(rows: &[String], sep: &str, width: usize) -> Vec<(SLine, bool)> {
    let ncol = md_cells(sep)
        .len()
        .max(rows.first().map_or(0, |r| md_cells(r).len()));
    let raw_rows: Vec<Vec<SLine>> = rows
        .iter()
        .map(|r| {
            let cells = md_cells(r);
            (0..ncol)
                .map(|i| md_inline(cells.get(i).map_or("", |c| c.as_str())))
                .collect()
        })
        .collect();
    let plain = |r: &str| {
        let mut l: SLine = vec![("  ".into(), Style::default())];
        l.extend(md_inline(r.trim()));
        (l, true)
    };
    let overhead = 3 * ncol + 3;
    if width <= overhead + ncol {
        return rows.iter().map(|r| plain(r)).collect();
    }
    let aligns: Vec<MdAlign> = md_cells(sep)
        .iter()
        .map(|c| match (c.starts_with(':'), c.ends_with(':')) {
            (true, true) => MdAlign::Center,
            (false, true) => MdAlign::Right,
            _ => MdAlign::Left,
        })
        .collect();
    let mut colw = vec![1usize; ncol];
    for row in &raw_rows {
        for (i, cell) in row.iter().enumerate() {
            colw[i] = colw[i].max(md_spans_w(cell));
        }
    }
    let avail = width - overhead;
    while colw.iter().sum::<usize>() > avail {
        let idx = (0..ncol).max_by_key(|i| colw[*i]).unwrap_or(0);
        if colw[idx] <= 1 {
            break;
        }
        colw[idx] -= 1;
    }
    let rule = |left: &str, mid: &str, right: &str| -> SLine {
        let mut l: SLine = vec![("  ".into(), Style::default()), (left.into(), C_GUTTER)];
        for (i, cw) in colw.iter().enumerate() {
            l.push(("─".repeat(cw + 2), C_GUTTER));
            l.push(((if i + 1 == ncol { right } else { mid }).into(), C_GUTTER));
        }
        l
    };
    let cells = |row: &Vec<SLine>, header: bool| -> SLine {
        let mut l: SLine = vec![("  ".into(), Style::default()), ("│".into(), C_GUTTER)];
        for (i, cell) in row.iter().enumerate() {
            let mut spans = md_fit(cell, colw[i]);
            if header {
                for s in spans.iter_mut() {
                    if s.1 == Style::default() {
                        s.1 = C_H3;
                    }
                }
            }
            let pad = colw[i].saturating_sub(md_spans_w(&spans));
            let (lft, rgt) = match aligns.get(i).copied().unwrap_or(MdAlign::Left) {
                MdAlign::Left => (0, pad),
                MdAlign::Right => (pad, 0),
                MdAlign::Center => (pad / 2, pad - pad / 2),
            };
            if lft > 0 {
                l.push((" ".repeat(lft + 1), Style::default()));
            } else {
                l.push((" ".into(), Style::default()));
            }
            l.extend(spans);
            l.push((" ".repeat(rgt + 1), Style::default()));
            l.push(("│".into(), C_GUTTER));
        }
        l
    };
    let mut out: Vec<(SLine, bool)> = vec![(rule("┌", "┬", "┐"), false)];
    for (i, row) in raw_rows.iter().enumerate() {
        out.push((cells(row, i == 0), true));
        if i == 0 {
            out.push((rule("├", "┼", "┤"), false));
        }
    }
    out.push((rule("└", "┴", "┘"), false));
    out
}

/// Block-level markdown: a whole assistant message (or the live, still growing
/// stream) into styled scrollback lines. `width` sizes rules and tables.
pub(super) fn md_block(text: &str, in_fence: &mut bool, width: usize) -> Vec<(SLine, bool)> {
    if text.is_empty() {
        return Vec::new();
    }
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out: Vec<(SLine, bool)> = Vec::new();
    let mut i = 0usize;
    while i < lines.len() {
        let raw = lines[i];
        let t = raw.trim_start();
        if !*in_fence {
            if md_is_table_row(t)
                && i + 1 < lines.len()
                && md_is_table_sep(lines[i + 1].trim_start())
            {
                let sep = lines[i + 1].trim_start().to_string();
                let mut rows = vec![t.to_string()];
                i += 2;
                while i < lines.len() && md_is_table_row(lines[i].trim_start()) {
                    rows.push(lines[i].trim_start().to_string());
                    i += 1;
                }
                out.extend(md_table(&rows, &sep, width));
                continue;
            }
            if md_is_rule(t) {
                out.push((
                    vec![
                        ("  ".into(), Style::default()),
                        ("─".repeat(width.saturating_sub(4).max(3)), C_GUTTER),
                    ],
                    true,
                ));
                i += 1;
                continue;
            }
            // a 4-space/tab indented block after a blank line is code, unless it
            // starts with a list marker (that is a nested list instead)
            let ind = raw.len() - t.len();
            let prev_blank = i == 0 || lines[i - 1].trim().is_empty();
            if prev_blank
                && (raw.starts_with('\t') || ind >= 4)
                && !t.is_empty()
                && md_item(t, ind).is_none()
            {
                while i < lines.len() {
                    let r = lines[i];
                    let indent = r.len() - r.trim_start().len();
                    if r.trim().is_empty() {
                        // a blank line only stays inside the block when more
                        // indented code follows it
                        let more = lines.get(i + 1).is_some_and(|n| {
                            !n.trim().is_empty()
                                && (n.starts_with('\t') || n.len() - n.trim_start().len() >= 4)
                        });
                        if !more {
                            break;
                        }
                        out.push((vec![("  ▎ ".into(), C_GUTTER)], false));
                        i += 1;
                        continue;
                    }
                    if indent == 0 {
                        break;
                    }
                    let body = r
                        .strip_prefix('\t')
                        .or_else(|| r.strip_prefix("    "))
                        .unwrap_or(r);
                    out.push((
                        vec![("  ▎ ".into(), C_GUTTER), (body.to_string(), C_CODE)],
                        true,
                    ));
                    i += 1;
                }
                continue;
            }
        }
        out.push(md_line(raw, in_fence));
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_code_and_bold() {
        let l = md_inline("a `x` b **y** c");
        let texts: Vec<&str> = l.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(texts, ["a ", "x", " b ", "y", " c"]);
        assert_eq!(l[1].1, C_ICODE);
        assert!(l[3].1.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn line_fence_heading_bullet() {
        let mut fence = false;
        let (l, c) = md_line("```rust", &mut fence);
        assert!(fence && c && l[0].0 == "  ▎ ");
        let (l, _) = md_line("fn main() {}", &mut fence);
        assert_eq!(l[0].0, "  ▎ ");
        assert_eq!(l[1].1, C_CODE);
        md_line("```", &mut fence);
        assert!(!fence);
        let (l, c) = md_line("# Title", &mut fence);
        assert!(c && l[1].1 == C_HEAD && l[1].0 == "Title");
        let (l, _) = md_line("- item", &mut fence);
        assert_eq!(l[0].0, "  • ");
    }

    #[test]
    fn marker_swaps_leading_indent() {
        let mut fence = false;
        let (mut l, _) = md_line("hello", &mut fence);
        mark_line(&mut l);
        assert_eq!(l[0].0, "● ");
        let (mut l, _) = md_line("```python", &mut fence);
        mark_line(&mut l);
        assert_eq!(l[0].0, "● ");
        assert_eq!(l[1].0, "  ▎ ");
    }

    #[test]
    fn block_renders_tables_rules_lists_and_headings() {
        let md = "# One\n## Two\n### Three\n\n1. first\n2. second\n\n- [x] done\n- [ ] todo\n\n---\n\n| name | qty |\n|:-----|----:|\n| apples | 12 |\n| pears | 3 |\n";
        let mut fence = false;
        let out = md_block(md, &mut fence, 60);
        let text: Vec<String> = out
            .iter()
            .map(|(l, _)| l.iter().map(|(t, _)| t.as_str()).collect::<String>())
            .collect();
        assert_eq!(text[0], "  One");
        assert_eq!(text[1], "  Two");
        assert_eq!(text[2], "  Three");
        assert_eq!(out[0].0[1].1, C_HEAD);
        assert_eq!(out[1].0[1].1, C_H2);
        assert_eq!(out[2].0[1].1, C_H3);
        assert!(text.contains(&"  1. first".to_string()), "{text:?}");
        assert!(text.contains(&"  • ☑ done".to_string()), "{text:?}");
        assert!(text.contains(&"  • ☐ todo".to_string()), "{text:?}");
        assert!(text.iter().any(|t| t.starts_with("  ────")), "{text:?}");
        // the boxed table: borders, header and aligned right column
        assert!(text.iter().any(|t| t.starts_with("  ┌─")), "{text:?}");
        assert!(text.iter().any(|t| t == "  │ name   │ qty │"), "{text:?}");
        assert!(text.iter().any(|t| t == "  │ apples │  12 │"), "{text:?}");
        assert!(text.iter().any(|t| t == "  │ pears  │   3 │"), "{text:?}");
    }

    #[test]
    fn indented_code_block_gets_the_gutter_bar() {
        let mut fence = false;
        let out = md_block("text\n\n    let x = 1;\n    x + 1\n\nafter", &mut fence, 40);
        let text: Vec<String> = out
            .iter()
            .map(|(l, _)| l.iter().map(|(t, _)| t.as_str()).collect::<String>())
            .collect();
        assert_eq!(
            text,
            vec!["  text", "", "  ▎ let x = 1;", "  ▎ x + 1", "", "  after",]
        );
        assert_eq!(out[2].0[1].1, C_CODE);
    }

    #[test]
    fn inline_styles_italics_strike_and_links() {
        let l = md_inline("an *italic* and ~~gone~~ and [docs](http://x/y) end");
        let text: String = l.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(text, "an italic and gone and docs (http://x/y) end");
        assert!(l[1].1.add_modifier.contains(Modifier::ITALIC), "{l:?}");
        assert!(l[3].1.add_modifier.contains(Modifier::CROSSED_OUT), "{l:?}");
        assert_eq!(l[5].1, C_LINK);
        // underscores inside words stay literal, escaped markers are kept
        let l = md_inline("snake_case_name and \\*literal\\*");
        let text: String = l.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(text, "snake_case_name and *literal*");
    }

    #[test]
    fn wrapped_lines_keep_gutter_and_word_boundaries() {
        use crate::util::wrap_sline;
        let l = vec![
            ("  ▎ ".to_string(), C_GUTTER),
            ("    if a { b }".to_string(), C_CODE),
        ];
        let rows = wrap_sline(&l, 14);
        assert_eq!(rows.len(), 2);
        for r in &rows {
            assert_eq!(r[0].0, "  ▎ ");
            assert_eq!(r[1].0, "    ");
        }
        // paragraphs wrap on spaces, not mid-word
        let p = vec![
            ("  ".to_string(), Style::default()),
            ("alpha beta gamma".to_string(), Style::default()),
        ];
        let rows = wrap_sline(&p, 12);
        let text: Vec<String> = rows
            .iter()
            .map(|r| r.iter().map(|(t, _)| t.as_str()).collect())
            .collect();
        assert_eq!(text, vec!["  alpha beta", "  gamma"]);
        // list numbers/bullets blank out on the wrapped row
        let li = vec![
            ("  2. ".to_string(), C_DIM),
            ("alpha beta gamma".to_string(), Style::default()),
        ];
        let rows = wrap_sline(&li, 12);
        let text: Vec<String> = rows
            .iter()
            .map(|r| r.iter().map(|(t, _)| t.as_str()).collect())
            .collect();
        assert_eq!(text, vec!["  2. alpha", "     beta", "     gamma"]);
    }

    fn text_of(l: &SLine) -> String {
        l.iter().map(|(t, _)| t.as_str()).collect()
    }

    #[test]
    fn unmatched_markers_stay_literal() {
        // emphasis/code delimiters with no closer must not be eaten
        for (raw, want) in [
            ("a*b", "a*b"),
            ("use *args and **kwargs", "use *args and **kwargs"),
            ("2 * 3 * 4", "2 * 3 * 4"), // pairs DO emphasise
            ("x ~~ y", "x ~~ y"),
            ("a `b", "a `b"),
            ("cost a**b", "cost a**b"),
            ("f(*a, **k)", "f(*a, **k)"),
            ("100% sure ~done", "100% sure ~done"),
        ] {
            assert_eq!(text_of(&md_inline(raw)), want, "{raw}");
        }
        // and matched pairs still style
        let l = md_inline("a*b*c");
        assert_eq!(text_of(&l), "abc");
        assert!(l[1].1.add_modifier.contains(Modifier::ITALIC), "{l:?}");
        let l = md_inline("**b**");
        assert_eq!(text_of(&l), "b");
        assert!(l[0].1.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn link_with_wide_label_keeps_tail() {
        // consumed count is chars, not bytes — CJK labels must not eat text
        let l = md_inline("[文档](http://x) tail");
        assert_eq!(text_of(&l), "文档 (http://x) tail");
    }

    #[test]
    fn fence_delimiter_shows_lang_not_backticks() {
        let mut fence = false;
        let (l, _) = md_line("```rust", &mut fence);
        let t = text_of(&l);
        assert_eq!(t, "  ▎ rust", "{t}");
        let (l, _) = md_line("```", &mut fence);
        assert_eq!(text_of(&l), "  ▎ ");
        assert!(!fence);
    }
}
