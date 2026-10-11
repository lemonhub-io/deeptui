# ragent

A lean AI coding agent in Rust. Every design decision optimizes two things:
**API cost** and **speed**. 5.6 MB binary, ~5 ms startup, ~4 MB RSS.

In a controlled benchmark against Codex CLI, Grok CLI, and Pi — same model
(`deepseek-flash`), same tasks, proxy-measured billing data — ragent solved
8/8 tasks at **$0.0011 per successful task**, ~3–62× cheaper and ~2–18× faster
than the others. See [REPORT.md](REPORT.md).

```
cargo build --release
export RAGENT_API_KEY=sk-...        # or OPENAI_API_KEY
./target/release/ragent             # TUI
./target/release/ragent "fix the bug in src/api.rs"   # headless
git diff | ./target/release/ragent "review this"      # piped stdin
```

**Build note (Linux):** `.cargo/config.toml` links with [mold](https://github.com/rui314/mold)
(`apt install mold`) — ~4.7x faster rebuild-relink locally (12s vs 57s for the
dev profile). On macOS/Windows the config is a no-op; on a mold-less Linux
box, comment out the `[target.*]` sections.

## Cost levers

| Lever | Where |
|---|---|
| ~120-token system prompt | `agent.rs::SYSTEM` |
| Terse tool schemas (every word is billed each round) | `tools.rs::specs` |
| `edit_file` = exact search/replace, never whole-file rewrites | `tools.rs::edit_file_sync` |
| Hard caps on all tool output (lines/bytes/hits; truncation tells model how to page) | `tools.rs` consts |
| Repo map injected once — file tree + symbol outlines, not file contents | `repomap.rs` |
| Auto-compaction: stale tool outputs → `[elided]`, then middle-history drop with tool-call orphan repair | `ctx.rs::compact` |
| Stable message prefix for provider prompt caching; cached tokens shown (`⛁`) | `api.rs` |
| Live $ meter from a price table (overridable in config) | `config.rs`, status bar |
| Cache-hit input priced separately (DeepSeek hits are ~50x cheaper) + DeepSeek peak-window ×2 | `config.rs::cost` |
| **Cost per successful task**: every run recorded to `~/.local/share/ragent/stats.tsv`; TUI shows `task $x` and session `$/✓`; headless prints task cost + lifetime average | `stats.rs` |
| Assistant messages that ended in tool calls stay content-free; final answers capped at 8 lines by prompt rule | `agent.rs` |

## Speed levers

| Lever | Where |
|---|---|
| SSE streaming to the UI per chunk | `api.rs::stream_chat` |
| Batches of read-only tool calls (`read_file`/`grep`/`glob`/`ls`/`repomap`) execute concurrently via `JoinSet` | `agent.rs` |
| Blocking file ops run in `spawn_blocking`; `ignore` crate walks respect `.gitignore` and skip `target`/`node_modules`/etc. | `tools.rs`, `repomap.rs` |
| No tokenizer dependency — a 1-division char heuristic estimates tokens in O(n) with zero load time | `util.rs::est_tokens` |
| Incremental scrollback wrap cache; only new lines rewrap, only visible lines materialize per frame | `tui.rs` |
| tokio multi-thread runtime, `thin` LTO, `codegen-units=1`, stripped | `Cargo.toml` |

## Config

Resolution order: CLI flag → env var (`RAGENT_API_KEY`, `RAGENT_BASE_URL`, `RAGENT_MODEL`, fallback `OPENAI_*`) → `~/.config/ragent/config.toml` → defaults. `ragent --init` writes a commented example.

Any **OpenAI-compatible** endpoint works: OpenAI, DeepSeek, OpenRouter, Ollama (`http://localhost:11434/v1`), vLLM, LM Studio…

```toml
# ~/.config/ragent/config.toml
api_key = "..."
base_url = "https://api.deepseek.com/v1"
model = "deepseek-flash"
context_budget = 60000      # tokens; compaction kicks in above this
max_output_tokens = 8192
max_iters = 40
bash_timeout_ms = 30000
# price_input / price_output / price_cached: $/1M for unknown models
price_input = 0.15
price_output = 0.60
price_cached = 0.003
```

## TUI keys

- `Enter` send (queues while a run is in flight)
- `Alt-Enter` / `Shift-Enter`* newline  (*kitty-protocol terminals)
- `Esc` interrupt run (or clear input)
- `Ctrl-C` / `Ctrl-D` quit · `Ctrl-L` clear scrollback
- `Ctrl-A/E/U/K/W`, `Ctrl-←/→` line editing · `↑/↓` history
- `PgUp/PgDn`, mouse wheel — scroll

## Tools

`read_file` (offset/limit paging) · `edit_file` (atomic multi-edit search/replace) ·
`write_file` · `bash` (timeout, head+tail truncation) · `grep` (regex, gitignore-aware,
glob filter) · `glob` · `ls` · `repomap`. File tools are confined to the project root;
`bash` is your shell.

## Layout

```
src/api.rs      OpenAI-compatible SSE client, retries, usage decode
src/tools.rs    tool schemas + implementations, output caps
src/ctx.rs      token estimation + two-stage compaction
src/repomap.rs  repo skeleton builder
src/agent.rs    run loop, parallel tool dispatch, events
src/tui.rs      ratatui UI: scrollback, input, status bar
src/config.rs   config/env/CLI resolution + price table
src/stats.rs    per-task cost accounting (stats.tsv)
src/main.rs     CLI, headless mode
bench/          benchmark harness, fixtures, and raw data (see REPORT.md)
```

## Metrics

Every run appends `ts model in out cached cost ok|fail` to
`~/.local/share/ragent/stats.tsv`. Headless prints `[task ok] $x · ↑in ↓out
⛁cached · life $avg/✓`; the TUI status bar shows `task $x` and session
`$/✓` live.

## License

[Apache License 2.0](LICENSE).
