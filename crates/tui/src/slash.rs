//! Slash commands: their table, parsing and completion. What a command does
//! is the app's call; this only knows the words.

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Slash {
    Help,
    /// `/model` and whatever follows it.
    Model(String),
    Clear,
    Resume(Option<usize>),
    Compact,
    Undo,
    Retry,
    Context,
    Cost,
    /// The `SKILL.md` files the agent may load.
    Skills,
    Copy,
    Exit,
}

/// One row of `/help`.
pub(crate) struct Spec {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub args: &'static str,
    pub help: &'static str,
    slash: Slash,
}

const fn spec(name: &'static str, args: &'static str, help: &'static str, slash: Slash) -> Spec {
    Spec {
        name,
        aliases: &[],
        args,
        help,
        slash,
    }
}

pub(crate) const COMMANDS: &[Spec] = &[
    spec("help", "", "commands and keys", Slash::Help),
    spec(
        "model",
        "[…]",
        "pick or add a model, paste a provider key",
        Slash::Model(String::new()),
    ),
    Spec {
        name: "clear",
        aliases: &["new"],
        args: "",
        help: "start a new conversation",
        slash: Slash::Clear,
    },
    spec(
        "resume",
        "[n]",
        "list earlier conversations here, or continue one",
        Slash::Resume(None),
    ),
    spec(
        "compact",
        "",
        "shrink the context now, without an API call",
        Slash::Compact,
    ),
    spec(
        "undo",
        "",
        "forget the last turn (files stay as they are)",
        Slash::Undo,
    ),
    spec(
        "retry",
        "",
        "forget the last turn and run its prompt again",
        Slash::Retry,
    ),
    spec("context", "", "what fills the context", Slash::Context),
    spec("cost", "", "this session's tokens and spend", Slash::Cost),
    spec(
        "skills",
        "",
        "the skills the agent can load (SKILL.md)",
        Slash::Skills,
    ),
    spec(
        "copy",
        "",
        "copy the last answer to the clipboard",
        Slash::Copy,
    ),
    Spec {
        name: "exit",
        aliases: &["quit"],
        args: "",
        help: "quit",
        slash: Slash::Exit,
    },
];

/// What a submitted line is.
#[derive(Debug, PartialEq)]
pub(crate) enum Line {
    Prompt(String),
    Command(Slash),
    /// Meant as a command but not one; why.
    Bad(String),
}

pub(crate) fn parse(text: &str) -> Line {
    let Some(rest) = text.strip_prefix('/') else {
        return Line::Prompt(text.into());
    };
    // `//…` sends a prompt that starts with a slash
    if rest.starts_with('/') {
        return Line::Prompt(rest.into());
    }
    let (word, arg) = rest
        .split_once(char::is_whitespace)
        .map_or((rest, ""), |(w, a)| (w, a.trim()));
    if word.is_empty() {
        return Line::Command(Slash::Help);
    }
    // a path such as /etc/hosts is a prompt, not a command
    if !word.chars().all(|c| c.is_ascii_alphabetic()) {
        return Line::Prompt(text.into());
    }
    let s = match lookup(word) {
        Ok(s) => s,
        Err(why) => return Line::Bad(why),
    };
    match (s.slash.clone(), arg) {
        (c, "") => Line::Command(c),
        (Slash::Model(_), rest) => Line::Command(Slash::Model(rest.into())),
        (Slash::Resume(_), n) => match n.parse() {
            Ok(n) if n > 0 => Line::Command(Slash::Resume(Some(n))),
            _ => Line::Bad("usage: /resume [n] — n as /resume lists them".into()),
        },
        _ => Line::Bad(format!("/{} takes no arguments", s.name)),
    }
}

/// A name, an alias, or a prefix only one command has.
fn lookup(word: &str) -> Result<&'static Spec, String> {
    let w = word.to_ascii_lowercase();
    if let Some(s) = COMMANDS
        .iter()
        .find(|s| s.name == w || s.aliases.contains(&w.as_str()))
    {
        return Ok(s);
    }
    match matches(&w).as_slice() {
        [one] => Ok(one),
        [] => Err(format!(
            "unknown command /{word} — /help lists them; //{word} sends it as a prompt"
        )),
        many => Err(format!(
            "/{word} could be {}",
            many.iter()
                .map(|s| format!("/{}", s.name))
                .collect::<Vec<_>>()
                .join(" ")
        )),
    }
}

/// Commands a name or alias of which starts with `prefix`, in table order.
fn matches(prefix: &str) -> Vec<&'static Spec> {
    COMMANDS
        .iter()
        .filter(|s| s.name.starts_with(prefix) || s.aliases.iter().any(|a| a.starts_with(prefix)))
        .collect()
}

/// What a half-typed `/word` could become — nothing once the line is
/// anything else.
pub(crate) fn hints(text: &str) -> Vec<&'static Spec> {
    match text.strip_prefix('/') {
        Some(w) if w.chars().all(|c| c.is_ascii_alphabetic()) => matches(&w.to_ascii_lowercase()),
        _ => Vec::new(),
    }
}

/// Tab on a half-typed `/word`: the whole command when only one fits, else
/// as far as every candidate agrees. `None` when that adds nothing.
pub(crate) fn complete(text: &str) -> Option<String> {
    let hits = hints(text);
    let out = match hits.as_slice() {
        [] => return None,
        [one] => {
            let pad = if one.args.is_empty() { "" } else { " " };
            format!("/{}{pad}", one.name)
        }
        many => {
            // the word each candidate was matched by
            let typed = text[1..].to_ascii_lowercase();
            let words: Vec<&str> = many
                .iter()
                .map(|s| {
                    std::iter::once(s.name)
                        .chain(s.aliases.iter().copied())
                        .find(|w| w.starts_with(&typed))
                        .unwrap_or(s.name)
                })
                .collect();
            let common = words[1..].iter().fold(words[0], |acc, w| {
                let n = acc
                    .bytes()
                    .zip(w.bytes())
                    .take_while(|(a, b)| a == b)
                    .count();
                &acc[..n]
            });
            format!("/{common}")
        }
    };
    (out != text).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_parse_into_prompts_commands_or_reasons() {
        use Line::*;
        assert_eq!(parse("fix it"), Prompt("fix it".into()));
        assert_eq!(parse("/clear"), Command(Slash::Clear));
        assert_eq!(parse("/NEW"), Command(Slash::Clear));
        assert_eq!(parse("/quit"), Command(Slash::Exit));
        assert_eq!(parse("/"), Command(Slash::Help));
        // a prefix only one command has is enough
        assert_eq!(parse("/comp"), Command(Slash::Compact));
        assert_eq!(parse("/resume"), Command(Slash::Resume(None)));
        assert_eq!(parse("/resume  3 "), Command(Slash::Resume(Some(3))));
        assert_eq!(parse("/skills"), Command(Slash::Skills));
        assert_eq!(parse("/m"), Command(Slash::Model(String::new())));
        assert_eq!(
            parse("/model add  openrouter/qwen/qwen3-coder"),
            Command(Slash::Model("add  openrouter/qwen/qwen3-coder".into()))
        );
        // paths and escaped slashes go to the model
        assert_eq!(
            parse("/etc/hosts is odd"),
            Prompt("/etc/hosts is odd".into())
        );
        assert_eq!(parse("//fix the bug"), Prompt("/fix the bug".into()));

        let Bad(why) = parse("/fix the bug") else {
            panic!()
        };
        assert!(why.contains("//fix sends it as a prompt"), "{why}");
        assert_eq!(parse("/re"), Bad("/re could be /resume /retry".into()));
        assert_eq!(parse("/undo 2"), Bad("/undo takes no arguments".into()));
        for n in ["0", "x"] {
            assert!(matches!(parse(&format!("/resume {n}")), Bad(_)), "{n}");
        }
    }

    #[test]
    fn hints_follow_the_half_typed_word_and_tab_completes_it() {
        let names = |t: &str| hints(t).iter().map(|s| s.name).collect::<Vec<_>>();
        assert_eq!(names("/").len(), COMMANDS.len());
        assert_eq!(names("/co"), ["compact", "context", "cost", "copy"]);
        assert_eq!(names("/q"), ["exit"]);
        assert!(names("/clear now").is_empty() && names("hi").is_empty());

        assert_eq!(complete("/comp").as_deref(), Some("/compact"));
        assert_eq!(complete("/res").as_deref(), Some("/resume "));
        assert_eq!(complete("/mo").as_deref(), Some("/model "));
        assert_eq!(complete("/q").as_deref(), Some("/exit"));
        assert_eq!(complete("/sk").as_deref(), Some("/skills"));
        assert_eq!(complete("/con").as_deref(), Some("/context"));
        assert_eq!(complete("/r").as_deref(), Some("/re"));
        // nothing to add, or nothing to complete
        assert_eq!(complete("/co"), None);
        assert_eq!(complete("/zz"), None);
        assert_eq!(complete("plain"), None);
        assert_eq!(complete(""), None);
    }
}
