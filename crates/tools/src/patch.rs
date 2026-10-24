//! Where an `edit_file` search string lands — and, when it lands nowhere,
//! what the model most likely meant.
//!
//! A failed edit costs a whole round (re-read the file, retry), so the matcher
//! forgives what models predictably get wrong while staying unambiguous:
//! exact first; then line by line ignoring surrounding whitespace, re-indenting
//! the replacement by the offset it found; and when even that misses, the
//! error carries the closest region, numbered, so the retry needs no read.

use std::ops::Range;

/// A unique place to splice: `range` of the text becomes `with`.
#[derive(Debug, PartialEq)]
pub struct Hit {
    pub range: Range<usize>,
    pub with: String,
    /// Matched only after ignoring whitespace.
    pub loose: bool,
}

/// Find the one region `old` names in `text`, and what replaces it.
pub fn locate(text: &str, old: &str, new: &str) -> Result<Hit, String> {
    let exact: Vec<usize> = text.match_indices(old).map(|(i, _)| i).collect();
    match exact.len() {
        1 => {
            return Ok(Hit {
                range: exact[0]..exact[0] + old.len(),
                with: new.to_string(),
                loose: false,
            })
        }
        0 => {}
        n => {
            let at: Vec<usize> = exact.iter().map(|&i| line_of(text, i)).collect();
            return Err(format!(
                "`old` matches {n} times ({}) — include more surrounding lines",
                lines_list(&at)
            ));
        }
    }

    let lines = spans(text);
    let (lead, tail, want) = trim_blank(old);
    if want.is_empty() {
        return Err("`old` not found (it is only whitespace)".into());
    }
    let starts: Vec<usize> = (0..=lines.len().saturating_sub(want.len()))
        .filter(|&s| {
            s + want.len() <= lines.len()
                && want
                    .iter()
                    .zip(&lines[s..])
                    .all(|(w, r)| w.trim() == text[r.clone()].trim())
        })
        .collect();
    match starts.len() {
        0 => Err(closest(text, &lines, &want)),
        1 => {
            let s = starts[0];
            let found = &lines[s..s + want.len()];
            let got: Vec<&str> = found.iter().map(|r| &text[r.clone()]).collect();
            let shift = reindent(&got, &want).ok_or_else(|| {
                format!(
                    "`old` matches lines {}-{} only up to indentation, which differs \
                     unevenly; copy them exactly:\n{}",
                    s + 1,
                    s + want.len(),
                    numbered(&got, s)
                )
            })?;
            let with = shed(new, lead, tail)
                .iter()
                .map(|l| shift.apply(l))
                .collect::<Vec<String>>()
                .join("\n");
            let mut range = found[0].start..found[found.len() - 1].end;
            // deleting whole lines takes their line break along
            if with.is_empty() {
                if text[range.end..].starts_with('\n') {
                    range.end += 1;
                } else if range.start > 0 {
                    range.start -= 1;
                }
            }
            Ok(Hit {
                range,
                with,
                loose: true,
            })
        }
        n => {
            let at: Vec<usize> = starts.iter().map(|s| s + 1).collect();
            Err(format!(
                "`old` matches {n} times ignoring whitespace ({}) — include more surrounding lines",
                lines_list(&at)
            ))
        }
    }
}

/// `old`/`new` copied from `read_file` output still carry its `  12│` gutter
/// on every line; take it off. Text without a full gutter is left alone.
pub fn ungutter(s: &str) -> Option<String> {
    let mut out = Vec::new();
    for l in s.lines() {
        if l.trim().is_empty() {
            out.push("");
            continue;
        }
        let t = l.trim_start();
        let digits = t.len() - t.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        out.push(t[digits..].strip_prefix('│').filter(|_| digits > 0)?);
    }
    let mut s2 = out.join("\n");
    if s.ends_with('\n') {
        s2.push('\n');
    }
    Some(s2)
}

/// 1-based line of byte offset `at`.
pub fn line_of(text: &str, at: usize) -> usize {
    text[..at].matches('\n').count() + 1
}

/// How much the open/close counts of `()`, `[]`, `{}` move from `old` to
/// `new` — non-zero is how a dropped or doubled brace usually shows.
pub fn imbalance(old: &str, new: &str) -> Vec<(char, i64)> {
    let net = |s: &str, o: char, c: char| {
        s.chars().filter(|&x| x == o).count() as i64 - s.chars().filter(|&x| x == c).count() as i64
    };
    [('(', ')'), ('[', ']'), ('{', '}')]
        .into_iter()
        .map(|(o, c)| (o, net(new, o, c) - net(old, o, c)))
        .filter(|&(_, d)| d != 0)
        .collect()
}

/// Byte ranges of each line, without its `\n`.
fn spans(text: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, b) in text.bytes().enumerate() {
        if b == b'\n' {
            out.push(start..i);
            start = i + 1;
        }
    }
    if start < text.len() {
        out.push(start..text.len());
    }
    out
}

/// The lines of `s` without blank lines at either end, and how many were
/// dropped at the start and at the end.
fn trim_blank(s: &str) -> (usize, usize, Vec<&str>) {
    let all: Vec<&str> = s.lines().collect();
    let lead = all.iter().take_while(|l| l.trim().is_empty()).count();
    let tail = all[lead..]
        .iter()
        .rev()
        .take_while(|l| l.trim().is_empty())
        .count();
    (lead, tail, all[lead..all.len() - tail].to_vec())
}

/// The lines of `new`, minus as many blank lines at each end as `old` had —
/// the blank framing was part of the search, not of the change.
fn shed(new: &str, lead: usize, tail: usize) -> Vec<&str> {
    let all: Vec<&str> = new.lines().collect();
    let l = all
        .iter()
        .take(lead)
        .take_while(|l| l.trim().is_empty())
        .count();
    let t = all[l..]
        .iter()
        .rev()
        .take(tail)
        .take_while(|l| l.trim().is_empty())
        .count();
    all[l..all.len() - t].to_vec()
}

/// How the replacement is re-indented to sit where `old` matched.
enum Shift {
    Same,
    Add(String),
    Drop(String),
}

impl Shift {
    fn apply(&self, l: &str) -> String {
        if l.trim().is_empty() {
            return l.to_string();
        }
        match self {
            Shift::Same => l.to_string(),
            Shift::Add(p) => format!("{p}{l}"),
            // a line indented less than the offset sits at the block's margin
            Shift::Drop(p) => l
                .strip_prefix(p.as_str())
                .unwrap_or(l.trim_start())
                .to_string(),
        }
    }
}

fn indent(l: &str) -> &str {
    &l[..l.len() - l.trim_start().len()]
}

/// The single indentation offset between the file's lines and the model's,
/// or `None` when they differ unevenly (tabs for spaces, a mangled block).
fn reindent(got: &[&str], want: &[&str]) -> Option<Shift> {
    let pairs: Vec<(&str, &str)> = got
        .iter()
        .zip(want)
        .filter(|(_, w)| !w.trim().is_empty())
        .map(|(g, w)| (indent(g), indent(w)))
        .collect();
    if pairs.iter().all(|(g, w)| g == w) {
        return Some(Shift::Same);
    }
    let offset = |long: &str, short: &str| long.strip_suffix(short).map(str::to_string);
    let same = |v: Vec<Option<String>>| {
        let first = v.first()?.clone()?;
        v.iter()
            .all(|p| p.as_deref() == Some(first.as_str()))
            .then_some(first)
    };
    if let Some(p) = same(pairs.iter().map(|(g, w)| offset(g, w)).collect()) {
        return Some(Shift::Add(p));
    }
    same(pairs.iter().map(|(g, w)| offset(w, g)).collect()).map(Shift::Drop)
}

/// The error for a search that matched nowhere: the most similar region of
/// the file, numbered like `read_file`, so the retry can copy it.
fn closest(text: &str, lines: &[Range<usize>], want: &[&str]) -> String {
    // scoring sees at most the first 8 lines of the search: enough to place it
    let k = want.len().min(8);
    let probe: Vec<Vec<(char, char)>> = want[..k].iter().map(|l| bigrams(l)).collect();
    let file: Vec<Vec<(char, char)>> = lines.iter().map(|r| bigrams(&text[r.clone()])).collect();
    let best = (0..lines.len())
        .map(|s| {
            let score = (0..k)
                .map(|j| file.get(s + j).map_or(0.0, |f| dice(f, &probe[j])))
                .sum::<f64>()
                / k as f64;
            (s, score)
        })
        .fold(None, |b: Option<(usize, f64)>, x| match b {
            Some(b) if b.1 >= x.1 => Some(b),
            _ => Some(x),
        });
    match best {
        Some((s, score)) if score >= 0.5 => {
            let end = (s + want.len()).min(lines.len()).min(s + 12);
            let got: Vec<&str> = lines[s..end].iter().map(|r| &text[r.clone()]).collect();
            format!(
                "`old` not found; closest is lines {}-{} ({:.0}% similar):\n{}",
                s + 1,
                end,
                score * 100.0,
                numbered(&got, s)
            )
        }
        _ => "`old` not found (no similar region; read the file first)".into(),
    }
}

/// Sorted character bigrams of a trimmed line.
fn bigrams(s: &str) -> Vec<(char, char)> {
    let c: Vec<char> = s.trim().chars().collect();
    let mut v: Vec<(char, char)> = c.windows(2).map(|w| (w[0], w[1])).collect();
    v.sort_unstable();
    v
}

/// Sørensen–Dice similarity of two sorted bigram multisets.
fn dice(a: &[(char, char)], b: &[(char, char)]) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let (mut i, mut j, mut hit) = (0, 0, 0usize);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                hit += 1;
                i += 1;
                j += 1;
            }
        }
    }
    2.0 * hit as f64 / (a.len() + b.len()) as f64
}

/// Lines in `read_file`'s gutter format, starting at 0-based line `from`.
fn numbered(lines: &[&str], from: usize) -> String {
    lines
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let l: String = l.chars().take(200).collect();
            format!("{:>4}│{l}", from + i + 1)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn lines_list(at: &[usize]) -> String {
    let mut s = at
        .iter()
        .take(6)
        .map(|l| format!("L{l}"))
        .collect::<Vec<_>>()
        .join(", ");
    if at.len() > 6 {
        s.push_str(", …");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn splice(text: &str, h: &Hit) -> String {
        let mut t = text.to_string();
        t.replace_range(h.range.clone(), &h.with);
        t
    }

    fn edit(text: &str, old: &str, new: &str) -> Result<String, String> {
        locate(text, old, new).map(|h| splice(text, &h))
    }

    #[test]
    fn exact_matches_win_and_ambiguity_names_the_lines() {
        let h = locate("a\nfoo\nb\n", "foo", "bar").unwrap();
        assert_eq!((h.range.clone(), h.loose), (2..5, false));
        let err = locate("x\nfoo\nfoo\n", "foo", "bar").unwrap_err();
        assert_eq!(
            err,
            "`old` matches 2 times (L2, L3) — include more surrounding lines"
        );
    }

    #[test]
    fn whitespace_slips_still_land_once() {
        // trailing spaces in the file, and a search that lost them
        let text = "fn a() {   \n    x();\n}\n";
        assert_eq!(
            edit(text, "fn a() {\n    x();", "fn a() {\n    y();").unwrap(),
            "fn a() {\n    y();\n}\n"
        );
        // blank framing around the search is not part of the change
        assert_eq!(edit("a\nb\nc\n", "\n b \n", "\nB\n").unwrap(), "a\nB\nc\n");
        // and a loose match that is ambiguous is still refused
        let err = locate("  x\ny\n x \n", "x  ", "z").unwrap_err();
        assert!(
            err.contains("2 times ignoring whitespace (L1, L3)"),
            "{err}"
        );
    }

    #[test]
    fn the_replacement_follows_the_indentation_it_found() {
        let text = "class A:\n        def f(self):\n            return 1\n";
        // the model dropped one indent level from both old and new
        let out = edit(
            text,
            "    def f(self):\n        return 1",
            "    def f(self):\n        return 2\n\n    def g(self):\n        pass",
        )
        .unwrap();
        assert_eq!(
            out,
            "class A:\n        def f(self):\n            return 2\n\n        def g(self):\n            pass\n"
        );
        // ...or added one
        let out = edit(
            "if x:\n  y()\n",
            "    if x:\n      y()",
            "    if x:\n      z()",
        )
        .unwrap();
        assert_eq!(out, "if x:\n  z()\n");
        // tabs for spaces cannot be reconciled: the real lines come back
        let err = locate(
            "func f() {\n\treturn 1\n}\n",
            "func f() {\n    return 1",
            "x",
        )
        .unwrap_err();
        assert!(err.contains("unevenly"), "{err}");
        assert!(err.contains("   2│\treturn 1"), "{err}");
    }

    #[test]
    fn deleting_lines_takes_their_break() {
        // an exact deletion is literal
        assert_eq!(edit("a\n  b \nc\n", "b", "").unwrap(), "a\n   \nc\n");
        // a loose one removes whole lines
        assert_eq!(edit("a\n  b\nc\n", "b  ", "").unwrap(), "a\nc\n");
        assert_eq!(edit("a\n  b \nc", "  b\nc", "").unwrap(), "a");
    }

    #[test]
    fn a_miss_shows_the_closest_region() {
        let text = "fn alpha() {\n    let total = items.len();\n    total\n}\n";
        let err = locate(text, "let totl = item.len();\ntotal", "x").unwrap_err();
        assert!(
            err.starts_with("`old` not found; closest is lines 2-3"),
            "{err}"
        );
        assert!(err.contains("   2│    let total = items.len();"), "{err}");
        let err = locate(text, "completely unrelated words here", "x").unwrap_err();
        assert!(err.contains("no similar region"), "{err}");
        assert!(locate(text, " \n\t\n", "x")
            .unwrap_err()
            .contains("only whitespace"));
    }

    #[test]
    fn gutters_come_off_only_when_every_line_has_one() {
        assert_eq!(
            ungutter("  12│    let a = 1;\n  13│\n\n  14│}\n").as_deref(),
            Some("    let a = 1;\n\n\n}\n")
        );
        assert_eq!(ungutter("a│b"), None);
        assert_eq!(ungutter("12│x\nplain"), None);
        assert_eq!(ungutter("│x"), None);
    }

    #[test]
    fn imbalance_reports_net_bracket_drift() {
        assert!(imbalance("f(a)", "f(a, b)").is_empty());
        assert_eq!(imbalance("if x {\n}", "if x {"), [('{', 1)]);
        assert_eq!(imbalance("(a))", "(a)"), [('(', 1)]);
        assert_eq!(imbalance("[x]", "x]"), [('[', -1)]);
    }
}
