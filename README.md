# DeepTUI

A lean AI coding agent in Rust. Every design decision optimizes two things:
**API cost** and **speed**. 5.6 MB binary, ~5 ms startup, ~4 MB RSS.

In a controlled benchmark against Codex CLI, Grok CLI, and Pi — same model
(`deepseek-flash`), same tasks, proxy-measured billing data — DeepTUI solved
8/8 tasks at **$0.0011 per successful task**, ~3–62× cheaper and ~2–18× faster
than the others. See [REPORT.md](REPORT.md).

## Install

**Required release: v0.1.1-beta.** All ragent releases from v0.1.0 through
v0.6.0 are retired. Existing ragent and DeepTUI v0.1.0-beta users must reinstall;
see [MIGRATION.md](MIGRATION.md).

```sh
curl -fsSL https://raw.githubusercontent.com/lemonhub-io/deeptui/v0.1.1-beta/install.sh | bash
```

`install.sh` detects the OS/CPU, downloads the matching release asset
(`deeptui-vX.Y.Z-<target>.tar.gz`), verifies its SHA-256 against the release's
`SHA256SUMS.txt`, smoke-tests it, then installs atomically — an interrupted or
failed download never replaces a working binary. It installs to `~/.local/bin`
(`/usr/local/bin` as root, no sudo needed otherwise).

```sh
install.sh --tag v0.1.1-beta          # pin a release
install.sh --from-source         # build with cargo (no prebuilt / exotic CPU)
install.sh --prefix /usr/local   # custom prefix (--bin-dir DIR for the exact dir)
install.sh --modify-path         # append the install dir to your shell rc
install.sh --no-verify           # skip the checksum (not recommended)
install.sh --uninstall           # remove the binary (keeps config + stats)
install.sh --help                # all options
```

Prebuilt targets: Linux x86_64 (static musl) and aarch64, macOS x86_64/arm64,
Windows x86_64 (`.zip`; on Windows the script prints the asset to grab).
Anything else, or an x86 CPU without AVX2, falls back to `--from-source`.
Environment overrides: `DEEPTUI_VERSION`, `DEEPTUI_INSTALL_DIR`.

## Updating

```sh
deeptui --self-update             # install the required supported release
deeptui --check-update            # just report, don't touch anything
deeptui --self-update --update-tag v0.1.1-beta   # pin a tag
```

`--self-update` is the same release path as the installer, done in-process:
the asset for this platform is downloaded, its SHA-256 is checked against the
release's `SHA256SUMS.txt`, the binary is run once (`--version` must match the
tag), and only then is it `rename`d over the running `deeptui` — an interrupted
update can never leave a half-written binary. It needs write access to the
directory holding the binary (not the binary itself), and takes effect on the
next start.

The installer and updater accept only `v0.1.1-beta`, including the complete
prerelease suffix. The `latest` installer alias resolves to this beta explicitly;
GitHub's stable-only `/latest` endpoint is not used. Cached notifications for
retired releases are ignored. A client already on the required beta skips the
background lookup.

Previously downloaded ragent binaries have optional update checks and no remote
kill switch. They cannot be remotely forced to upgrade. The supported migration
is to rerun the installer, which preserves existing DeepTUI settings, copies
legacy config and usage stats when the destination is absent, and replaces a
recognized retired `ragent` in the install directory with a DeepTUI launcher.

## Quickstart

```sh
export DEEPTUI_API_KEY=sk-...        # or OPENAI_API_KEY
deeptui                              # TUI
deeptui "fix the bug in src/api.rs"  # headless
git diff | deeptui "review this"     # piped stdin
deeptui --dump-map                   # print the injected repo map, no API call
deeptui -c "now add tests for it"    # continue this directory's last conversation
deeptui -m deepseek/deepseek-v4-pro  # start on a model added with /model
deeptui --max-rounds 20 --max-cost 0.05 "…"   # bound one task's rounds and spend
```

From a checkout instead: `cargo build --release`, then use
`./target/release/deeptui` in place of `deeptui` above.

**Build note (Linux):** `.cargo/config.toml` links with [mold](https://github.com/rui314/mold)
(`apt install mold`) — ~5x faster rebuild-relink locally (11s vs 57s for the
dev profile). On macOS/Windows the config is a no-op; on a mold-less Linux
box, comment out the `[target.*]` sections.

x86_64 Linux builds target `x86-64-v3` (AVX2/BMI2/FMA — Intel Haswell 2013+ /
AMD Excavator 2015+). For older CPUs lower it in `.cargo/config.toml`; for a
fully local-tuned binary:

```sh
RUSTFLAGS="-C target-cpu=native -C link-arg=-fuse-ld=mold" cargo build --release
```

Other speed levers: release = thin LTO + `codegen-units=1` + `panic=abort` +
stripped; dev = `line-tables-only` debug info; single-thread tokio runtime
(tool calls are IO-bound concurrency, not CPU parallelism).

## Cost levers

| Lever | Where |
|---|---|
| ~120-token system prompt | `engine/src/prompt.rs::RULES` |
| Terse tool schemas (every word is billed each round) | each `Tool` const in `tools/src` |
| `edit_file` = exact search/replace, never whole-file rewrites | `tools/src/fs.rs::edit_file` |
| Hard caps on all tool output (lines/bytes/hits; truncation tells model how to page) | `tools/src/{fs,search,shell}.rs` consts |
| Repo map injected once — file tree + symbol outlines, not file contents | `deeptui-repomap` |
| Map budget (bytes) spent by file rank: entry points first, `...` for what is cut; every code file keeps a symbol floor | `repomap/src/lib.rs::build` |
| Auto-compaction: stale tool outputs → `[elided · gist]` (`exit 1`, `error: …`, `120 lines`), then middle-history drop with tool-call orphan repair | `core/src/history.rs::compact` |
| The dropped middle leaves a **work log** — one line per step (`bash \`cargo test\` → exit 101 (×3)`), built from the messages, zero API cost — so a compacted model doesn't redo or re-verify work it can no longer see (the loop that sank Codex in REPORT.md) | `core/src/history.rs::work_log` |
| **Runaway guards**: a call that returns the same result 3× in a turn is told so in that result; a round cap (default 60) whose last round asks for a status report instead of cutting mid-work; an optional $ cap per task | `core/src/turn.rs` (`Limits`) |
| Fewer wasted rounds: near-JSON tool arguments are repaired, `edit_file` forgives whitespace/indent slips, CRLF and copied `read_file` gutters, and a miss returns the closest region numbered — retry without a re-read | `tools/src/{args,patch}.rs` |
| `bash` output cleaned of ANSI escapes, progress-bar redraws and repeated-line runs; head+tail kept per stream, so the verdict at the end survives | `tools/src/shell.rs::tidy` |
| Stable message prefix for provider prompt caching; cached tokens shown (`⛁`) | `deeptui-provider` |
| Live $ meter: usage is priced at the source, every view just sums | `core/src/tariff.rs`, `engine` |
| Cache-hit input priced separately (DeepSeek hits are ~50x cheaper) + DeepSeek peak-window ×2 | `core/src/tariff.rs::cost` |
| **Cost per successful task**: every run recorded to `~/.local/share/deeptui/stats.tsv`; TUI shows `task $x` and session `$/✓`; headless prints task cost + lifetime average | `core/src/ledger.rs`, `src/journal.rs` |
| Assistant messages that ended in tool calls stay content-free; final answers capped at 8 lines by prompt rule | `core/src/turn.rs` |

## Speed levers

| Lever | Where |
|---|---|
| SSE streaming to the UI per chunk | `provider/src/lib.rs::Completion` |
| Tool calls run in waves derived from their declared effect: consecutive reads (`read_file`/`grep`/`glob`/`ls`/`repomap`) run concurrently, every write is a barrier | `core/src/schedule.rs`, `engine` |
| Blocking file ops run in `spawn_blocking`; `ignore` crate walks respect `.gitignore` and skip `target`/`node_modules`/etc. | `deeptui-tools`, `deeptui-repomap` |
| No tokenizer dependency — a 1-division char heuristic estimates tokens in O(n) with zero load time | `core/src/history.rs::est_tokens` |
| Incremental scrollback wrap cache; only new lines rewrap, only visible lines materialize per frame | `tui/src/feed.rs` |
| tokio `current_thread` runtime, `thin` LTO, `codegen-units=1`, stripped | `Cargo.toml` |

## Config

Resolution order: CLI flag → env var (`DEEPTUI_API_KEY`, `DEEPTUI_BASE_URL`, `DEEPTUI_MODEL`, fallback `OPENAI_*`) → `~/.config/deeptui/config.toml` → defaults. `deeptui --init` writes a commented example.

Any **OpenAI-compatible** endpoint works: OpenAI, DeepSeek, OpenRouter, Ollama (`http://localhost:11434/v1`), vLLM, LM Studio…

```toml
# ~/.config/deeptui/config.toml
api_key = "..."
base_url = "https://api.deepseek.com/v1"
model = "deepseek-flash"
context_budget = 60000      # tokens; compaction kicks in above this
max_output_tokens = 8192
bash_timeout_ms = 30000
# price_input / price_output / price_cached: $/1M for unknown models
price_input = 0.15
price_output = 0.60
price_cached = 0.003
use_map = true               # inject the repo map into the system prompt
map_bytes = 5000             # repo-map budget in bytes (env DEEPTUI_MAP_BYTES)
max_rounds = 60              # model rounds per task; the last reports status (0 = no cap)
max_task_cost = 0.50         # $ per task before it is stopped (0 = no cap, the default)
save_session = true          # keep conversations per project for /resume and --continue
```

`--continue` (`-c`) resumes the last conversation held for the current
directory, with a freshly built system prompt; `/resume` in the TUI lists the
others. Each project keeps its 50 newest conversations, one file each, under
`$XDG_DATA_HOME/deeptui/sessions/` (owner-only; `DEEPTUI_SESSION_DIR` moves
them, `save_session = false` stops writing them).

### Models

No key is needed to start: a first `deeptui` opens on a provider picker
(DeepSeek, OpenAI, Anthropic, Google, OpenRouter, Kimi, GLM, Qwen, … and all
the others behind "All providers…"). Choose one with the arrows — typing
filters — and the composer turns into a masked key field: paste the key and
press Enter. It is checked against the provider's free `/models` endpoint (a
refused key is asked for again), kept owner-only in `models.toml`, and the
provider's models are offered next. One key serves all of a provider's models.
The key is never echoed, never put in history, never written to a transcript.

`/model` in the TUI manages a short list of models on top of the configured
one, with [models.dev](https://models.dev) as the source of endpoints, prices
and context limits:

```
/model                             a picker over your models; + add a model
/model setup                       the provider picker again
/model search qwen coder           pick from models.dev (✓ = a key is at hand)
/model add openrouter/qwen/qwen3-coder
/model 2                           switch — by number, id, or any unique piece of it
/model key openrouter              paste a new key for a provider
/model list · /model rm 2 · /model refresh
```

Switching to a model whose provider has no key asks for the key first and
switches once it is in.

Only models that call tools and sit behind an OpenAI-compatible API are
offered (about 6,000 of models.dev's ~9,000). A switch applies from the next
turn, keeps the conversation, and is remembered for the next start (`-m` or
`DEEPTUI_MODEL` override it). Each model is fitted to its own window: the
compaction budget never exceeds 90% of its context minus one reply.

Thinking models that stream `reasoning_content` (DeepSeek V4, Kimi, GLM,
Qwen…) get it sent back on their assistant messages while the turn lasts, as
interleaved thinking between tool calls requires; it is dropped when the turn
ends, so later requests and saved transcripts never pay for old thinking. It
is never shown.

The list lives in `~/.config/deeptui/models.toml` (`DEEPTUI_MODELS_FILE` moves
it), a snapshot taken when each model was added — startup never touches the
network, and every field can be edited by hand. Keys come from the env vars
models.dev names (`DEEPSEEK_API_KEY`, `OPENROUTER_API_KEY`, …), else an
`api_key = "…"` written under the entry, else the provider's key pasted into
the TUI (`[keys]`), else the configured key when the model is on the same
host. Headless runs need a key up front and say how to get one. The catalog is cached for a day in
`$XDG_CACHE_HOME/deeptui/` and revalidated by ETag, so a refresh that finds
nothing new costs one empty reply; offline, the last copy is used.

See the map without spending a token: `deeptui --dump-map [--map-bytes N]`
prints exactly what gets injected (`--no-map` / `use_map = false` disables it).

## TUI

Deep navy surfaces with ice-blue accents · persistent session header
(project/model/context/state) · welcome view with lifetime stats · padded,
multiline prompt composer · responsive cost and token counters · markdown rendering in assistant output
(ATX headings, fenced and indented code with a `▎` gutter,
inline `` `code` `` / `**bold**` / `*italic*` / `~~strike~~` / links, bullets and
ordered lists with hanging indents, blockquotes, `---` rules and boxed pipe
tables) · `❯`/`●`/`◆` turn markers · braille activity line
while running · breathing composer border and moving activity rail · compact
status bar with a `▲ scrolled` indicator. Idle sessions do not redraw on a timer.
Set `DEEPTUI_REDUCED_MOTION=1` for a static activity indicator and border.

### Slash commands

Type `/` in the composer: matching commands float above it, and Tab completes
(any unambiguous prefix also works — `/comp` is `/compact`). Commands queue
behind a running turn just like prompts, so they always apply in the order
typed. None of them calls the model, except `/retry` re-running a prompt.

| Command | Does |
|---|---|
| `/help` | commands and keys |
| `/model […]` | pick, add and switch models; paste a provider's key (see [Models](#models)) |
| `/clear` (`/new`) | start a new conversation; the old one stays resumable |
| `/resume [n]` | list this directory's earlier conversations, or continue the `n`th |
| `/compact` | shrink the context now (elide old tool output, fold the middle into a work log) |
| `/undo` | forget the last turn — up to 20 back; files are not touched |
| `/retry` | forget the last turn and run its prompt again |
| `/context` | context size against the budget: system / tool output / conversation |
| `/cost` | session spend, tasks, tokens and cache hit rate |
| `/copy` | copy the last answer to the clipboard (OSC 52, works over ssh) |
| `/exit` (`/quit`) | quit |

A prompt that really starts with a slash is sent as `//like this`; paths such
as `/etc/hosts …` go to the model unchanged.

The landing page uses the same palette, a scripted interactive terminal preview,
keyboard-accessible tabs, installation commands, and a motion toggle. It also
respects the system's reduced-motion setting. Preview it locally with
`python3 -m http.server 8765`, then open `http://localhost:8765`.

### TUI keys

- `Enter` send (queues while a run is in flight)
- `Alt-Enter` / `Shift-Enter`* newline  (*kitty-protocol terminals)
- `Esc` interrupt run (or clear input)
- `Ctrl-C` / `Ctrl-D` quit · `Ctrl-L` clear scrollback
- `Ctrl-A/E/U/K/W`, `Ctrl-←/→` line editing · `↑/↓` history
- `PgUp/PgDn`, mouse wheel — scroll

## Tools

`read_file` (offset/limit paging) · `edit_file` (atomic multi-edit search/replace) ·
`write_file` · `bash` (timeout, head+tail truncation) · `grep` (regex, gitignore-aware,
glob filter) · `glob` · `ls` · `repomap` (`path` scopes to a subtree, `budget` sizes the
output). File tools are confined to the project root; `bash` is your shell.

How they hold up when the model is sloppy:

- **`edit_file`** matches exactly first; failing that, line by line ignoring
  surrounding whitespace, re-indenting `new` by the offset it found (and saying
  so). An ambiguous `old` names every matching line; a missing one returns the
  most similar region, numbered like `read_file`. CRLF files stay CRLF. The
  result reports where each edit landed (`at L12, L80`) and warns when an edit
  shifts the bracket balance or breaks a file that was valid JSON.
- **`bash`** runs in its own process group with stdin closed and pagers,
  colours and prompts off. The whole group is killed when the call returns,
  times out or the turn is cancelled — a leftover test server can neither leak
  nor hold the pipes open and stall the call (`setsid` to keep one running). A
  timeout returns the output so far.
- **Arguments** that are near-JSON (raw newlines in strings, a markdown fence,
  double encoding, trailing prose) are repaired; anything else is an error that
  says why, instead of a misleading `missing <field>`.

## Layout

A Cargo workspace whose dependency graph is the architecture — every arrow
points down, and the compiler enforces it:

```
crates/core      deeptui-core: the pure domain — no IO, no async runtime, no clock.
                 Transcript + compaction, the turn state machine, events,
                 tariffs, the cost ledger, the effect scheduler
crates/provider  OpenAI-compatible SSE client: retries, stall timeout, chunk decode
crates/repomap   the project walk + ranked, byte-budgeted repo skeleton
crates/tools     tools as data: name, schema, effect and implementation in one value
crates/update    self-update: verify, smoke-test, atomic swap
crates/models    the models.dev catalog (cached, ETag-revalidated) + models.toml
crates/engine    interprets the turn machine; the session actor; the system prompt
crates/tui       ratatui frontend — feed, editor, meter, keymap, markdown
crates/testkit   dev-only fixtures: temp roots, loopback HTTP stub
src/             the composition root: settings, /model, transcripts, stats
                 journal, headless frontend
bench/           benchmark harness, fixtures, and raw data (see REPORT.md)
```

How a turn flows: `deeptui_core::Turn` is a sans-IO state machine — it says
what it needs next (`Next::Ask` a completion, `Next::Run` tool calls) and is
fed what happened. The engine performs exactly that IO, prices usage, and pours
the turn's `Event`s into the session's outlet. Every consumer is a fold over
that one stream: the TUI, the headless console, the session ledger, the
`stats.tsv` journal. Each turn ends with exactly one `Event::Finished(Receipt)`.

## Metrics

Every run appends `ts model in out cached cost ok|fail` to
`~/.local/share/deeptui/stats.tsv` (override the path with `DEEPTUI_STATS_FILE`;
never written by the unit tests). Headless prints `[task ok] $x · ↑in ↓out
⛁cached · life $avg/✓`; the TUI status bar shows `task $x` and session
`$/✓` live.

## License

[Apache License 2.0](LICENSE).
