# ragent — Horizontal Evaluation Report

A controlled benchmark of four terminal coding agents — **ragent**, **Pi 1.1.0**
(`@earendil-works/pi-coding-agent`), **Codex CLI 0.160.1**, and **Grok CLI
1.0.46** — all running the same model, **DeepSeek `deepseek-flash`**, on the
same task suite, on the same machine.

**Result:** ragent and Pi tie on token economy ($0.0011–0.0012 per successful
task); ragent is ~2× faster at identical request counts. Grok is ~3.5× more
expensive. Codex costs ~62× more per successful task and failed 2 of 8 tasks by
timing out in a compaction loop.

| agent | tasks OK | wall clock | API reqs | input tokens | output | wire up/down | spend | **$/✓ task** |
|---|---|---|---|---|---|---|---|---|
| **ragent** | **8/8** | **76.6 s** | 39 | **104,862** | 10,304 | 0.3 / 2.1 MB | $0.0090 | **$0.00112** |
| **pi** | **8/8** | 150.7 s | 39 | 117,632 | 10,621 | 0.4 / 3.0 MB | $0.0096 | $0.00120 |
| grok | 8/8 | 201.8 s | 73 | 1,254,634 | 21,554 | 3.6 / 5.2 MB | $0.0295 | $0.00369 |
| codex | 6/8 | 1,382.4 s | 379 | 53,323,006 | 216,831 | 140 / 439 MB | $0.4199 | $0.0700 |

## 1. Methodology

### 1.1 Protocol

- Every agent received the **identical prompt** per task and a **fresh copy** of
  the task fixture (`cp -r`).
- All traffic went through a per-agent **logging proxy** (`bench/dsproxy.py`)
  that forwards to `api.deepseek.com`, injects the API key, and records a
  normalized usage record per request: billed input/output tokens, cached and
  reasoning token details, request/response bytes, and server latency.
  Measurements come from the provider's `usage` field — not the agents'
  self-reports (Codex's self-report undercounted its own usage by ~150×).
- Wall-clock measured by the harness (`date +%s.%N` around the CLI process).
- Success is **scripted**, not judged: each task has an executable contract
  (e.g., `check.py` must print `ALL OK`, `test_shop.py` must pass).
- Model: `deepseek-flash` for every agent. No other model was used anywhere in
  this evaluation. Claude Code, though installed on the machine, was excluded
  per the evaluation's constraints.
- Pricing: DeepSeek off-peak rates — input-miss $0.15/1M, input-cache-hit
  $0.003/1M, output $0.60/1M (runs happened on a weekend, off-peak).

### 1.2 Agent invocation

| agent | command | wire API |
|---|---|---|
| ragent | `ragent "<prompt>"` | chat completions, streaming |
| pi | `pi -p --no-session --provider deepseek --model deepseek-flash` | chat completions, streaming |
| grok | `grok -m deepseek-flash --always-approve -p "<prompt>"` | chat completions |
| codex | `codex exec -m deepseek-flash --dangerously-bypass-approvals-and-sandbox` | **Responses API** |

Notes: Codex 0.160 requires the Responses wire API (its bubblewrap sandbox was
non-functional in this container and had to be bypassed). Grok and Pi needed a
custom-provider config (`~/.grok/config.toml`, `~/.pi/agent/models.json`).

### 1.3 Task suite — difficulty ladder

| level | fixture | task | contract |
|---|---|---|---|
| T1 | 1 file | fix a subtraction bug in `calc.py` | `python3 calc.py` → `all ok` |
| T3 | 3 files | rename `greet()` → `welcome()` everywhere | `check.py` → `check ok` |
| T4 | 8-file miniapp | answer what `handle()` does (one line) | grep transcript for route/dispatch |
| T5 | data file | write `csv2json.py` with quoted-field support | JSON output validates |
| L3 | 3-file blog API | add `POST /api/posts` w/ validation + extend tests | `tests.py` → `ALL OK` (needs ≥4 checks) |
| L4 | 3 modules + data | find 4 planted bugs (no-trailing-newline CSV, int mean, even median, percentile, sliding window) | `check.py` → `ALL OK` |
| L5 | spec only | build `notes.py` CLI + `test_notes.py` from contract | `check.py` → `ALL OK` |
| L6 | 8-file shop | fix 2 bugs (cart merge, coupon compounding) + implement wishlist feature + a test | `test_shop.py` + `check.py` → `ALL OK` |

T-tier ran **twice** for ragent/codex/grok (separate fixture copies each run).
The L-tier ran once per agent with a 600 s timeout.

## 2. Results

### 2.1 Easy tier (T1/T3/T4/T5), run 2 — cache-warm

```
agent  task    sec  reqs     in    out  cache%    cost
ragent T1     5.2    4   5,154   210    84%  $0.00026
       T3     4.7    4   5,633   464    89%  $0.00039
       T4     3.4    3   3,802   191    81%  $0.00023
       T5    14.7    5  10,799 2,001    91%  $0.00137
codex  T1    17.7    5 1,201,795  571  100%  $0.00412
       T3    15.6    5 1,202,071  643  100%  $0.00416
       T4     9.8    3   720,053  201  100%  $0.00238
       T5    17.7    5 1,220,160 1,261  100%  $0.00469
grok   T1    14.8    7  102,423 1,030   91%  $0.00234
       T3    11.1    5   84,081   724   89%  $0.00204
       T4     7.5    4   33,755   427   74%  $0.00167
       T5    16.4    7  103,275 1,316   90%  $0.00255
pi     T1    14.2    4    7,484   241   75%  $0.00044
       T3    15.4    5   10,385   524   83%  $0.00061
       T4    11.0    3    5,261   181   71%  $0.00035
       T5    14.1    4    7,715   584   80%  $0.00060
```

### 2.2 Difficulty ladder (L3–L6)

```
lvl agent  ok    sec  reqs        in     out  cache%      up     down   api_s    cost
L3  ragent OK    7.6    4     11,242  1,261    73%     37K    356K     7.0  $0.0012
    pi     OK   26.0    7     22,074  1,471    86%     83K    404K    ~18    $0.0014
    grok   OK   33.9   10    175,537  3,803    93%    666K   1,086K   23.6  $0.0046
    codex  FAIL 600    173 22,437,821 97,972    98%  62.9MB  196.4MB  569.7  $0.1756
L4  ragent OK    9.0    5     13,030  1,179    78%     38K    299K     8.3  $0.0012
    codex  OK   16.6    4    963,939  1,139   100%   2.6MB    8.0MB   12.8  $0.0039
    pi     OK   21.0    5     15,653  1,847    82%     53K    503K    ~15    $0.0016
    grok   OK   34.3   15    270,204  3,624    95%    998K    977K    27.7  $0.0048
L5  ragent OK   17.9    7     25,657  2,841    88%     87K    839K    15.8  $0.0022
    pi     OK   21.0    4     14,366  2,637    82%     54K    798K    ~15    $0.0020
    grok   OK   35.1   10    175,902  4,458    93%    669K   1,333K    25.2  $0.0049
    codex  FAIL 600    160 20,691,134 102,835  98%  58.1MB  183.3MB  570.7  $0.1751
L6  ragent OK   14.1    7     29,545  2,157    84%     97K    569K    12.4  $0.0021
    pi     OK   28.0    7     34,694  3,136    87%    125K    868K    ~21    $0.0026
    grok   OK   48.7   15    309,457  6,172    96%   1.2MB    1.7MB    37.2  $0.0066
    codex  OK  104.7   24  4,886,033 12,209    96%  13.4MB   41.4MB    88.0  $0.0500
```

### 2.3 Per-request scaffold (the smoking gun)

Measured request body → billed input tokens, first request of a fresh session:

| agent | per-req body | per-req input | per-req cached (steady state) |
|---|---|---|---|
| ragent | ~4 KB | ~1.3k | ~70–90% |
| pi | ~6 KB | ~1.6k | ~75–90% |
| grok | ~63 KB | ~16k | ~93% |
| codex | **~650 KB** | **~240k** | ~98–100% |

Codex's *first* request of a session already carries ~243k input tokens (its
full tool/environment scaffold), and its Responses-API payloads re-ship
`encrypted_content` reasoning blobs every round — hence ~650 KB up and **~2 MB
down per call** (DeepSeek echoes the items back). On L3 it moved **259 MB of
wire traffic** to add a 22-line endpoint.

## 3. Failure analysis: Codex's compaction loop

Codex timed out (600 s, rc=124) on L3 and L5 — the two *medium*-difficulty
tasks — while passing the hardest one (L6). Failure mode, from its transcript:

1. The model works; ~240k-token rounds accumulate tool output.
2. Codex's own context compaction fires ("context compacted" appears in the
   log, plus an ~11k-token summarization request between each pair of 244k
   requests).
3. Post-compaction the model loses track of applied edits and re-verifies
   everything: on L3 it re-printed all three files dozens of times and ran
   `tests.py` 86 times, oscillating between "I'll verify before patching" and
   the next compaction — without ever updating `tests.py`.
4. Each loop iteration costs ~240k input tokens; 173 iterations = 22.4M tokens
   = $0.176 — **more than ragent spent on the entire 8-task evaluation**.

The code it produced was correct both times (its `do_POST` handler and
`notes.py` pass their checks when run afterward). This is an **orchestration
failure**, not a capability failure — amplified ~100× by per-request state
size. A stray test-server process it leaked on its L3 port compounded the
confusion. Codex's cost is bimodal: when it converges it is merely expensive
(L4 $0.0039, L6 $0.050); when it doesn't, it is catastrophic.

## 4. Findings

1. **Token economy is a design invariant.** Across all 8 tasks and all
   difficulty levels, per-request input held ~1.3–3k tokens (ragent, pi),
   ~16k (grok), ~240k (codex). Difficulty scaled request *count*, not request
   *size* — for the lean agents, cost grew smoothly (ragent: $0.0004 → $0.0026
   from easiest to hardest).
2. **ragent ≈ pi on economy; ragent ~2× faster.** Identical request counts
   (39) and near-identical token totals (105k vs 118k input). The speed gap
   comes from ragent's parallel read-only tool dispatch (pi serializes them),
   ~5 ms native startup vs Node.js startup, and slightly smaller rounds.
3. **Cache-hit pricing decides grok/codex viability.** 90–100% of their input
   is served from DeepSeek's prompt cache at $0.003/1M. Codex's total swung
   3× between cold ($0.012/✓) and warm ($0.004/✓) runs on identical tasks —
   at 240k tokens/round, cache *is* the bill.
4. **More context ≠ more reliability.** Codex ships the most context by two
   orders of magnitude and was the only agent to fail — twice — because its
   compaction policy interacts badly with giant state.
5. **Cost visibility matters.** ragent records every task's spend to
   `stats.tsv` and shows `$ / ✓` live; it was the only agent in the benchmark
   whose self-reported cost matched the proxy-measured cost exactly.

## 5. Caveats

- n=1 run for the L-tier, n=2 for the T-tier (ragent/codex/grok), n=1 (pi).
  Directionally consistent; not a universal ranking.
- Codex's numbers are partly an adapter artifact: DeepSeek's Responses API
  echoes `encrypted_content` reasoning blocks that inflate every subsequent
  request. On OpenAI's own backend the accounting differs.
- Codex ran unsandboxed (`--dangerously-bypass-approvals-and-sandbox`) because
  its bwrap sandbox fails in this container; a working approval gate would
  slow it further.
- All agents share one DeepSeek API key; interleaved parallel L-tier runs may
  have mild rate-limit noise, well below the 10–100× effects measured.
- These are small synthetic tasks. On multi-hour sessions the rankings could
  shift — especially for codex, whose design trades token economy for
  ambient context.

## 6. Reproduce

```bash
# terminals 1-4: one proxy per agent
python3 bench/dsproxy.py 9101 ragent "$DEEPSEEK_API_KEY" &
python3 bench/dsproxy.py 9102 codex  "$DEEPSEEK_API_KEY" &
python3 bench/dsproxy.py 9103 grok   "$DEEPSEEK_API_KEY" &
python3 bench/dsproxy.py 9104 pi     "$DEEPSEEK_API_KEY" &

# easy tier (all four agents):   bench/run_bench.sh ragent codex grok pi
# difficulty ladder (per level): bench/run_ladder.sh L3 ragent codex grok pi
# aggregate:                     bench/agg_ladder.py  (reads /tmp/proxylog_*.jsonl)
```

Requires: ragent release build, `codex` ≥0.160 configured for a DeepSeek
Responses-API provider on :9102, `grok` ≥1.0.46 with a custom `deepseek-flash`
model on :9103, `pi` ≥1.1 with a `deepseek` provider on :9104.

Raw per-request logs are recorded to `/tmp/proxylog_<agent>.jsonl` at runtime;
normalized per-task data used in this report is in `bench/data/`
(`t_tier.csv`, `ladder.csv`).
