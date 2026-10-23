#!/usr/bin/env bash
# Difficulty ladder: L3..L6, all agents in parallel per level, deepseek-flash.
KEY=${DEEPSEEK_API_KEY:?set DEEPSEEK_API_KEY}
BENCH_DIR=$(cd "$(dirname "$0")" && pwd)
DEEPTUI=${DEEPTUI:-$BENCH_DIR/../target/release/deeptui}

declare -A PROMPTS=(
  [L3]="Add POST /api/posts to this blog API: JSON body {title, body}; title required (max 100 chars), body required, else 400 {\"error\": ...}; valid -> 201 with the created post (persist via store.add). Then extend tests.py with POST coverage (valid create, missing title, over-long title) and make 'python3 tests.py' print ALL OK."
  [L4]="Run 'python3 check.py' — it defines the contract for loader.py, transforms.py and report.py. Find and fix every bug until it prints ALL OK. Do not modify check.py or data.csv."
  [L5]="check.py and README.md define the contract. Implement notes.py (stdlib only): subcommands add TEXT / list / search QUERY / delete ID, JSON-list storage in \$NOTES_FILE, ids from 1, 'list' prints 'ID: text' lines, unknown commands exit non-zero. Also write test_notes.py with real unit tests covering all subcommands and run them. Finish when 'python3 check.py' prints ALL OK."
  [L6]="README.md describes this shop CLI plus a TODO wishlist feature, and test_shop.py currently fails. (1) Fix cart merging and coupon compounding, (2) implement the wishlist commands per the README, (3) add a wishlist unit test to test_shop.py. Finish when 'python3 test_shop.py' passes and 'python3 check.py' prints ALL OK."
)
declare -A PORTS=([deeptui]=8931 [codex]=8932 [grok]=8933 [pi]=8934)

lines_of() { wc -l < "$1" 2>/dev/null || echo 0; }

run() {
  local agent=$1 task=$2
  local dir=/tmp/bench/$agent/$task
  rm -rf "$dir"; mkdir -p /tmp/bench/$agent
  cp -r "$BENCH_DIR/fixtures/$task" "$dir"
  local log=/tmp/proxylog_$agent.jsonl
  local before=$(lines_of "$log")
  local start end rc=0
  start=$(date +%s.%N)
  case $agent in
    deeptui)
      (cd "$dir" && PORT=${PORTS[$agent]} DEEPTUI_BASE_URL=http://127.0.0.1:9101/v1 timeout 600 "$DEEPTUI" "${PROMPTS[$task]}") \
        > /tmp/benchout_${agent}_${task}.log 2>&1 ;;
    codex)
      (cd "$dir" && PORT=${PORTS[$agent]} DEEPTUI_API_KEY=$KEY timeout 600 codex exec -m deepseek-flash \
        -c model_provider='"deepseek"' \
        -c 'model_providers.deepseek.name="DeepSeek"' \
        -c 'model_providers.deepseek.base_url="http://127.0.0.1:9102/v1"' \
        -c 'model_providers.deepseek.env_key="DEEPTUI_API_KEY"' \
        --skip-git-repo-check --dangerously-bypass-approvals-and-sandbox \
        "${PROMPTS[$task]}") > /tmp/benchout_${agent}_${task}.log 2>&1 ;;
    grok)
      (cd "$dir" && PORT=${PORTS[$agent]} XAI_API_KEY=$KEY timeout 600 grok -m deepseek-flash \
        --always-approve -p "${PROMPTS[$task]}") \
        > /tmp/benchout_${agent}_${task}.log 2>&1 ;;
    pi)
      (cd "$dir" && PORT=${PORTS[$agent]} timeout 600 pi -p --no-session \
        --provider deepseek --model deepseek-flash \
        "${PROMPTS[$task]}") \
        > /tmp/benchout_${agent}_${task}.log 2>&1 ;;
  esac
  rc=$?
  end=$(date +%s.%N)
  local after=$(lines_of "$log")
  echo "$agent $task $(echo "$end - $start" | bc) $((after - before)) $before $after rc=$rc" >> /tmp/ladder_times.txt
  echo "done: $agent $task ($(echo "$end - $start" | bc | cut -d. -f1)s, $((after - before)) reqs, rc=$rc)"
}

check() {
  local agent=$1 task=$2 dir=/tmp/bench/$agent/$task ok=FAIL
  case $task in
    L3) (cd "$dir" && PORT=${PORTS[$agent]} python3 tests.py 2>/dev/null | grep -q "ALL OK") \
        && grep -q "do_POST" "$dir/server.py" && ok=OK ;;
    L4) (cd "$dir" && python3 check.py 2>/dev/null | grep -q "ALL OK") \
        && ! grep -q "int(sum" "$dir/report.py" && ok=OK ;;
    L5) (cd "$dir" && python3 check.py 2>/dev/null | grep -q "ALL OK") \
        && test -f "$dir/test_notes.py" && ok=OK ;;
    L6) (cd "$dir" && python3 test_shop.py >/dev/null 2>&1 \
        && python3 check.py 2>/dev/null | grep -q "ALL OK") && ok=OK ;;
  esac
  echo "$agent $task $ok" >> /tmp/ladder_ok.txt
  echo "check: $agent $task $ok"
}

task=$1; shift
agents=$([ $# -gt 0 ] && echo "$@" || echo "deeptui codex grok pi")
for agent in $agents; do run "$agent" "$task" & done
wait
for agent in $agents; do check "$agent" "$task"; done
