#!/usr/bin/env bash
# Ecosystem verification harness for the AI governance suite.
# Runs build/lint/test matrices against each project's CURRENT working tree.
# Usage: ./ecosystem_verify.sh [repo-dir ...]   (default: all six)
set -u
export CARGO_TERM_COLOR=never

HOME_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
RESULTS=()
OVERALL=0

record() { # repo check status detail
  RESULTS+=("$1|$2|$3|$4")
  if [ "$3" = "FAIL" ]; then OVERALL=1; fi
}

run_step() { # repo check cmd...
  local repo="$1" check="$2"; shift 2
  local out
  if out=$("$@" 2>&1); then
    record "$repo" "$check" "PASS" "-"
    return 0
  else
    local tail_log="${TMPDIR:-/tmp}/verify_${repo}_${check//\//_}.log"
    printf '%s\n' "$out" | tail -80 > "$tail_log"
    record "$repo" "$check" "FAIL" "$tail_log"
    return 1
  fi
}

verify_rust() {
  local repo="$1"; local d="$HOME_DIR/$1"
  echo "── $repo (rust) ──────────────────────────────────────"
  [ -d "$d" ] || { record "$repo" "present" "FAIL" "dir missing"; return; }
  echo "   branch: $(git -C "$d" branch --show-current)"
  pushd "$d" >/dev/null || return
  run_step "$repo" "fmt"    cargo fmt --all -- --check
  run_step "$repo" "clippy" cargo clippy --workspace --all-targets -- -D warnings
  run_step "$repo" "test"   cargo test --workspace --quiet
  popd >/dev/null
}

verify_hive() {
  local repo="hive"; local d="$HOME_DIR/hive"
  echo "── $repo (python) ────────────────────────────────────"
  local py="$d/.venv-enterprise/bin/python"
  [ -x "$py" ] || { record "$repo" "venv" "FAIL" ".venv-enterprise missing"; return; }
  pushd "$d" >/dev/null || return
  run_step "$repo" "compile" "$py" -m compileall -q backend
  run_step "$repo" "test"    "$py" -m pytest -q
  popd >/dev/null
}

verify_relay() {
  local repo="relay-enterprise"; local d="$HOME_DIR/relay-enterprise"
  echo "── $repo (python) ────────────────────────────────────"
  local py="$d/.venv-enterprise/bin/python"
  [ -x "$py" ] || { record "$repo" "venv" "FAIL" ".venv-enterprise missing"; return; }
  pushd "$d" >/dev/null || return
  run_step "$repo" "compile" "$py" -m compileall -q gateway auth security backends connectors config patroclus observability
  run_step "$repo" "noexec"  bash -c '! grep -rn "exec(" gateway/ --include="*.py"'
  run_step "$repo" "test"    "$py" -m pytest tests -q
  popd >/dev/null
}

echo "════════ ECOSYSTEM VERIFICATION HARNESS ════════"
if [ $# -gt 0 ]; then
  for r in "$@"; do
    case "$r" in
      hive) verify_hive ;;
      relay-enterprise) verify_relay ;;
      *) verify_rust "$r" ;;
    esac
  done
else
  for r in miser patroclus sentiel Aegis; do verify_rust "$r"; done
  verify_hive
  verify_relay
fi

echo
echo "════════ RESULTS ════════"
printf '%-18s %-10s %-6s %s\n' REPO CHECK STATUS DETAIL
for row in "${RESULTS[@]}"; do
  IFS='|' read -r repo check status detail <<< "$row"
  printf '%-18s %-10s %-6s %s\n' "$repo" "$check" "$status" "$detail"
done
echo
if [ $OVERALL -eq 0 ]; then echo "ALL GREEN ✔"; else echo "FAILURES PRESENT ✘ (see logs above)"; fi
exit $OVERALL
