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

# The shipped config already ships [classifier.jev] enabled = true; point
# MISER_CONFIG at a custom config to override anything else.

printf '%-12s %-8s %-9s %-8s %-7s %-7s %-7s %-6s %-8s %s\n' \
  MODE EXACT ADJACENT MAE UNDER OVER P50_MS AVG_MS COST_USD NOTE
for mode in "${MODES[@]}"; do
  if [ "$mode" = "jev" ] && [ -z "${JEV_API_KEY:-}" ]; then
    printf '%-12s %-8s %-9s %-8s %-7s %-7s %-7s %-6s %-8s %s\n' "$mode" "skipped" "" "" "" "" "" "" "" "(no JEV_API_KEY)"
    continue
  fi
  if { [ "$mode" = "cloud_llm" ] || [ "$mode" = "hybrid" ]; } && [ -z "${OPENROUTER_API_KEY:-}" ]; then
    printf '%-12s %-8s %-9s %-8s %-7s %-7s %-7s %-6s %-8s %s\n' "$mode" "skipped" "" "" "" "" "" "" "" "(no OPENROUTER_API_KEY)"
    continue
  fi
  summary="$(cargo run --quiet --manifest-path "$ROOT/Cargo.toml" -p miser-evals -- \
    --corpus "$CORPUS" --mode "$mode" --config "$CONFIG" 2>/dev/null \
    | grep -E 'exact_accuracy=|latency_ms_avg=' || true)"
  pick() { echo "$summary" | grep -o "$1" | cut -d= -f2 || true; }
  exact="$(pick 'exact_accuracy=[0-9.]*')"
  adjacent="$(pick 'adjacent_accuracy=[0-9.]*')"
  mae="$(pick 'mean_tier_distance=[0-9.]*')"
  under="$(pick 'under_routing=[0-9.]*')"
  over="$(pick 'over_routing=[0-9.]*')"
  failures="$(pick ' failures=[0-9]*')"
  p50="$(pick 'latency_ms_p50=[0-9]*')"
  avg="$(pick 'latency_ms_avg=[0-9.]*')"
  cost="$(pick 'classification_cost_usd=[0-9.]*')"
  note=""
  [ "${failures:-0}" -gt 0 ] 2>/dev/null && note="fallback×$failures"
  printf '%-12s %-8s %-9s %-8s %-7s %-7s %-7s %-6s %-8s %s\n' \
    "$mode" "$exact" "$adjacent" "$mae" "$under" "$over" "${p50:-n/a}" "${avg:-n/a}" "${cost:-\$0}" "$note"
done
