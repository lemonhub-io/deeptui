#!/usr/bin/env bash
# Benchmark: ragent vs codex vs grok on deepseek-flash, via logging proxies.
KEY=${DEEPSEEK_API_KEY:?set DEEPSEEK_API_KEY}
BENCH_DIR=$(cd "$(dirname "$0")" && pwd)
RAGENT=${RAGENT:-$BENCH_DIR/../target/release/ragent}

declare -A PROMPTS=(
  [T1]="fix the bug in calc.py and verify by running it"
  [T3]="rename greet() to welcome() everywhere (definition and all call sites), then run check.py to confirm"
  [T4]="what does the \`handle\` function in miniapp/server.py do? answer in one line"
  [T5]="write csv2json.py: read CSV from stdin (header row first), print a JSON array of objects to stdout. Handle quoted fields containing commas and escaped quotes. Then verify it works on sample.csv"
)

lines_of() { wc -l < "$1" 2>/dev/null || echo 0; }

run() {
  local agent=$1 task=$2
  local dir=/tmp/bench/$agent/$task
  rm -rf "$dir"; mkdir -p /tmp/bench/$agent
  cp -r "$BENCH_DIR/fixtures/$task" "$dir"
  local log=/tmp/proxylog_$agent.jsonl
  local before=$(lines_of "$log")
  local start end
  start=$(date +%s.%N)
  case $agent in
    ragent)
      (cd "$dir" && RAGENT_BASE_URL=http://127.0.0.1:9101/v1 timeout 180 "$RAGENT" "${PROMPTS[$task]}") \
        > /tmp/benchout_${agent}_${task}.log 2>&1 ;;
    codex)
      (cd "$dir" && RAGENT_API_KEY=$KEY timeout 180 codex exec -m deepseek-flash \
        -c model_provider='"deepseek"' \
        -c 'model_providers.deepseek.name="DeepSeek"' \
        -c 'model_providers.deepseek.base_url="http://127.0.0.1:9102/v1"' \
        -c 'model_providers.deepseek.env_key="RAGENT_API_KEY"' \
        --skip-git-repo-check --dangerously-bypass-approvals-and-sandbox \
        "${PROMPTS[$task]}") > /tmp/benchout_${agent}_${task}.log 2>&1 ;;
    grok)
      (cd "$dir" && XAI_API_KEY=$KEY timeout 180 grok -m deepseek-flash \
        --always-approve -p "${PROMPTS[$task]}") \
        > /tmp/benchout_${agent}_${task}.log 2>&1 ;;
    pi)
      (cd "$dir" && timeout 180 pi -p --no-session \
        --provider deepseek --model deepseek-flash \
        "${PROMPTS[$task]}") \
        > /tmp/benchout_${agent}_${task}.log 2>&1 ;;
  esac
  end=$(date +%s.%N)
  local after=$(lines_of "$log")
  echo "$agent $task $(echo "$end - $start" | bc) $((after - before))" >> /tmp/bench_times.txt
  echo "done: $agent $task ($(echo "$end - $start" | bc | cut -d. -f1)s, $((after - before)) reqs)"
}

# success checks ---------------------------------------------------------------
check() {
  local agent=$1 task=$2 dir=/tmp/bench/$agent/$task ok=FAIL
  case $task in
    T1) (cd "$dir" && python3 calc.py 2>/dev/null | grep -q "all ok") && ok=OK ;;
    T3) (cd "$dir" && python3 check.py 2>/dev/null | grep -q "check ok") \
        && grep -q "welcome" "$dir/main.py" && ok=OK ;;
    T4) grep -qiE "dispatch|route" /tmp/benchout_${agent}_${task}.log && ok=OK ;;
    T5) (cd "$dir" && python3 csv2json.py < sample.csv 2>/dev/null \
        | python3 -c "import json,sys; d=json.load(sys.stdin); exit(0 if len(d)==3 and d[0]['name']=='Smith, John' else 1)") && ok=OK ;;
  esac
  echo "$agent $task $ok" >> /tmp/bench_ok.txt
}

> /tmp/bench_times.txt; > /tmp/bench_ok.txt
for agent in "$@"; do
  for task in T1 T3 T4 T5; do
    run "$agent" "$task"
  done
done
for agent in "$@"; do for task in T1 T3 T4 T5; do check "$agent" "$task"; done; done
echo "=== times ==="; cat /tmp/bench_times.txt
echo "=== ok ==="; cat /tmp/bench_ok.txt
