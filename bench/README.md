# bench/

Reproducible harness for the evaluation in [../REPORT.md](../REPORT.md).

- `dsproxy.py` — logging forward-proxy to `api.deepseek.com`; records billed
  usage, payload bytes, and latency per request to `/tmp/proxylog_<tag>.jsonl`
- `run_bench.sh` — easy tier (T1/T3/T4/T5), sequential per agent
- `run_ladder.sh` — difficulty ladder (L3–L6), agents in parallel per level
- `fixtures/` — the eight task fixtures (unmodified copies)
- `data/` — normalized per-task measurements used in the report
- `agg_ladder.py`, `agg_tier.py` — proxy-log → per-task table aggregators

All agent invocations require a `DEEPSEEK_API_KEY` env var (the proxy injects
it, so agents can also run with dummy keys). L3 binds a fixture HTTP server on
per-agent ports (deeptui 8931, codex 8932, grok 8933, pi 8934).
