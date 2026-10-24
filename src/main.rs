//! DeepTUI — the composition root, and the only code that knows every crate.
//!
//! It resolves settings, builds an engine from its parts, taps the stats
//! journal into the session, and hands the session to a frontend. No crate
//! below it knows the others exist beyond what its manifest allows.

mod headless;
mod journal;
mod models;
mod settings;
mod transcript;

use anyhow::{bail, Result};
use clap::Parser;
use deeptui_core::{Event, Msg};
use deeptui_engine::{system_prompt, Command, Engine, Remote, Session};
use deeptui_provider::Provider;
use deeptui_tools::{Toolbox, Workspace};
use journal::Journal;
use settings::{Flags, Settings};
use std::io::{IsTerminal, Read};
use std::path::Path;
use tokio::sync::mpsc;
use transcript::Transcript;

/// DeepTUI — a lean coding agent: extreme cost & speed optimization.
#[derive(Parser)]
#[command(name = "deeptui", version, about)]
struct Cli {
    /// Prompt. If given (or stdin is piped), runs headless and exits.
    #[arg(trailing_var_arg = true)]
    prompt: Vec<String>,
    /// Model name, or one added with /model (e.g. deepseek/deepseek-v4-pro).
    #[arg(short, long)]
    model: Option<String>,
    /// OpenAI-compatible base URL (e.g. https://api.deepseek.com/v1).
    #[arg(long)]
    base_url: Option<String>,
    /// API key.
    #[arg(long)]
    api_key: Option<String>,
    /// Context budget in tokens before compaction.
    #[arg(long)]
    budget: Option<usize>,
    /// Force headless mode.
    #[arg(short = 'p', long)]
    print: bool,
    /// Don't inject the repo map into the system prompt.
    #[arg(long)]
    no_map: bool,
    /// Repo-map budget in bytes (default 5000, max 60000).
    #[arg(long, value_name = "BYTES")]
    map_bytes: Option<usize>,
    /// Print the repo map for this directory and exit.
    #[arg(long)]
    dump_map: bool,
    /// Write an example config file and exit.
    #[arg(long)]
    init: bool,
    /// Update to the latest release and exit.
    #[arg(long)]
    self_update: bool,
    /// Report whether a newer release exists, then exit.
    #[arg(long)]
    check_update: bool,
    /// Release tag for --self-update (default: the latest release).
    #[arg(long, value_name = "TAG")]
    update_tag: Option<String>,
    /// Don't check for a newer release on startup.
    #[arg(long)]
    no_update_check: bool,
    /// Continue this directory's last conversation.
    #[arg(short = 'c', long = "continue")]
    resume: bool,
    /// Model rounds per task; the last one must report status (0 = no cap, default 60).
    #[arg(long, value_name = "N")]
    max_rounds: Option<usize>,
    /// Dollars a task may spend before it is stopped (0 = no cap).
    #[arg(long, value_name = "USD")]
    max_cost: Option<f64>,
}

impl Cli {
    fn flags(&self) -> Flags<'_> {
        Flags {
            model: self.model.as_deref(),
            base_url: self.base_url.as_deref(),
            api_key: self.api_key.as_deref(),
            budget: self.budget,
            no_map: self.no_map,
            map_bytes: self.map_bytes,
            no_update_check: self.no_update_check,
            max_rounds: self.max_rounds,
            max_cost: self.max_cost,
        }
    }

    /// The prompt words, plus whatever was piped in after them.
    fn prompt(&self) -> String {
        let mut prompt = self.prompt.join(" ");
        if !std::io::stdin().is_terminal() {
            let mut piped = String::new();
            std::io::stdin().read_to_string(&mut piped).ok();
            let piped = piped.trim_end();
            if !piped.is_empty() {
                prompt = if prompt.is_empty() {
                    piped.to_string()
                } else {
                    format!("{prompt}\n\n{piped}")
                };
            }
        }
        prompt
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    // Self-update runs before any config is read: updating needs no API key.
    if cli.self_update || cli.check_update {
        let http = deeptui_update::client(std::time::Duration::from_secs(120))?;
        return if cli.check_update {
            deeptui_update::report(&http).await
        } else {
            deeptui_update::run(&http, cli.update_tag.as_deref()).await
        };
    }
    if cli.init {
        return settings::write_example();
    }
    let root = std::env::current_dir()?;
    if cli.dump_map {
        print!(
            "{}",
            deeptui_repomap::build(&root, settings::map_bytes(cli.map_bytes))
        );
        return Ok(());
    }
    // Sweep leftovers of an earlier swap (Windows keeps the old image aside).
    deeptui_update::cleanup_stale();
    let s = Settings::load(&cli.flags())?;
    let prompt = cli.prompt();
    let headless = cli.print || !prompt.is_empty();
    if headless && !s.ready() {
        bail!(
            "no API key for {}: set DEEPTUI_API_KEY (or the provider's own variable) \
             or pass --api-key — or run `deeptui` once to choose a provider and paste its key",
            s.model.id
        );
    }
    let journal = Journal::open(&s.model.endpoint.model);
    let lifetime = journal.load();
    let (remote, events) = session(&s, &root, journal, cli.resume)?;

    if headless {
        if prompt.is_empty() {
            bail!("empty prompt");
        }
        // One cached lookup, concurrently with the run: never on the critical path.
        if s.update_check {
            tokio::spawn(async {
                if let Some(tag) = deeptui_update::check(true).await {
                    eprintln!(
                        "\x1b[2m↑ deeptui {tag} available — deeptui --self-update (you have {})\x1b[0m",
                        deeptui_update::current_version()
                    );
                }
            });
        }
        let code = headless::run(remote, events, prompt, lifetime).await;
        std::process::exit(code);
    }
    let intro = deeptui_tui::Intro {
        model: s.model.endpoint.model.clone(),
        root,
        context_budget: s.model.context_budget,
        lifetime,
        priced: s.model.tariff.is_known(),
        update: s.update_check.then(deeptui_update::cached_update).flatten(),
        ready: s.ready(),
    };
    // no key yet: ask for the remembered model's, else open the provider picker
    if !s.ready() {
        let first = if s.model.id == s.configured.id {
            "setup".to_string()
        } else {
            s.model.id.clone()
        };
        remote.send(Command::Model(first));
    }
    deeptui_tui::run(remote, events, intro, deeptui_update::check(s.update_check)).await
}

/// Assemble the engine from its parts and start a session around it —
/// fresh, or picking up where this directory's last conversation stopped.
fn session(
    s: &Settings,
    root: &Path,
    journal: Journal,
    resume: bool,
) -> Result<(Remote, mpsc::Receiver<Event>)> {
    let m = &s.model;
    let provider = Provider::new(m.endpoint.clone())?;
    let toolbox = Toolbox::new(Workspace::new(root, s.bash_timeout_ms));
    let engine = Engine::new(provider, toolbox, m.tariff, m.context_budget).limits(s.limits);
    let history = vec![Msg::system(system_prompt(root, s.map))];
    let mut session = Session::new(engine, history)
        .tap(journal)
        .switchboard(models::Board::new(s));
    if s.save_session {
        session = session.shelf(Transcript::for_root(root));
    }
    let (remote, events) = session.spawn();
    // ahead of any prompt, so the turn starts from the old conversation
    if resume {
        remote.send(Command::Resume(Some(1)));
    }
    Ok((remote, events))
}
