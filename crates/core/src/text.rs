//! Small string helpers shared by every layer that shows text to a human.

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

pub fn fmt_tokens(n: u64) -> String {
    if n >= 10_000 {
        format!("{:.0}k", n as f64 / 1000.0)
    } else if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_tokens_boundaries() {
        assert_eq!(fmt_tokens(0), "0");
        assert_eq!(fmt_tokens(999), "999");
        assert_eq!(fmt_tokens(1_000), "1.0k");
        assert_eq!(fmt_tokens(1_500), "1.5k");
        // 9999 rounds up to 10.0k in the sub-10k branch, not the k branch
        assert_eq!(fmt_tokens(9_999), "10.0k");
        assert_eq!(fmt_tokens(10_000), "10k");
        assert_eq!(fmt_tokens(123_456), "123k");
    }

    #[test]
    fn one_line_flattens_newlines_and_marks_truncation() {
        assert_eq!(one_line("a\nb", 10), "a⏎b");
        assert_eq!(one_line("abc", 3), "abc");
        assert_eq!(one_line("abcdef", 3), "abc…");
        // max counts chars, not bytes: wide chars must not blow the budget
        assert_eq!(one_line("文档字", 2), "文档…");
    }
}
