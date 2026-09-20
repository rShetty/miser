#!/usr/bin/env bash
# Classifier benchmark: run miser-evals across classifier modes against the
# same corpus and print a per-mode accuracy/latency comparison table.
#
# Usage:
#   scripts/classifier_benchmark.sh [corpus] [modes...]
#
# Environment:
#   OPENROUTER_API_KEY  enables cloud_llm (and hybrid) classifier runs
#   JEV_API_KEY         enables jev classifier runs (TypeSafe direct or Gateway)
#   MISER_CONFIG        config file (default: config/miser.toml)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CORPUS="${1:-$ROOT/evals/classifier_cases.jsonl}"
shift || true
MODES=("$@")
if [ ${#MODES[@]} -eq 0 ]; then
  MODES=(heuristic local_llm cloud_llm hybrid jev)
fi
CONFIG="${MISER_CONFIG:-$ROOT/config/miser.toml}"

TMP_CONFIG=""
cleanup() { [ -n "$TMP_CONFIG" ] && rm -f "$TMP_CONFIG"; }
trap cleanup EXIT

if [ -n "${JEV_API_KEY:-}" ]; then
  # Flip the shipped config's jev endpoint on for this run.
  TMP_CONFIG="$(mktemp)"
  sed 's/^\[classifier.jev\]/[classifier.jev]/; s/^enabled = false/enabled = true/' "$CONFIG" > "$TMP_CONFIG"
  CONFIG="$TMP_CONFIG"
fi

printf '%-12s %-8s %-9s %-8s %-7s %-7s %-7s %-6s %s\n' \
  MODE EXACT ADJACENT MAE UNDER OVER P50_MS AVG_MS NOTE
for mode in "${MODES[@]}"; do
  if [ "$mode" = "jev" ] && [ -z "${JEV_API_KEY:-}" ]; then
    printf '%-12s %-8s %-9s %-8s %-7s %-7s %-7s %-6s %s\n' "$mode" "skipped" "" "" "" "" "" "" "(no JEV_API_KEY)"
    continue
  fi
  if { [ "$mode" = "cloud_llm" ] || [ "$mode" = "hybrid" ]; } && [ -z "${OPENROUTER_API_KEY:-}" ]; then
    printf '%-12s %-8s %-9s %-8s %-7s %-7s %-7s %-6s %s\n' "$mode" "skipped" "" "" "" "" "" "" "(no OPENROUTER_API_KEY)"
    continue
  fi
  summary="$(cargo run --quiet --manifest-path "$ROOT/Cargo.toml" -p miser-evals -- \
    --corpus "$CORPUS" --mode "$mode" --config "$CONFIG" 2>/dev/null \
    | grep -E 'exact_accuracy=|latency_ms_avg=' || true)"
  exact="$(echo "$summary" | grep -o 'exact_accuracy=[0-9.]*' | cut -d= -f2)"
  adjacent="$(echo "$summary" | grep -o 'adjacent_accuracy=[0-9.]*' | cut -d= -f2)"
  mae="$(echo "$summary" | grep -o 'mean_tier_distance=[0-9.]*' | cut -d= -f2)"
  under="$(echo "$summary" | grep -o 'under_routing=[0-9.]*' | cut -d= -f2)"
  over="$(echo "$summary" | grep -o 'over_routing=[0-9.]*' | cut -d= -f2)"
  failures="$(echo "$summary" | grep -o ' failures=[0-9]*' | cut -d= -f2)"
  p50="$(echo "$summary" | grep -o 'latency_ms_p50=[0-9]*' | cut -d= -f2)"
  avg="$(echo "$summary" | grep -o 'latency_ms_avg=[0-9.]*' | cut -d= -f2)"
  note=""
  [ "${failures:-0}" -gt 0 ] 2>/dev/null && note="fallback×$failures"
  printf '%-12s %-8s %-9s %-8s %-7s %-7s %-7s %-6s %s\n' \
    "$mode" "$exact" "$adjacent" "$mae" "$under" "$over" "${p50:-n/a}" "${avg:-n/a}" "$note"
done
