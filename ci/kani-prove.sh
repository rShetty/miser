#!/usr/bin/env bash
# Run one crate's Kani harnesses, then prove that proofs actually ran.
#
# This exists because a verification layer reporting success -- or nothing --
# when it should be loud has now happened four times in this project:
#
#   1. TLC explored 0 states and printed "no error".
#   2. A `cargo build -- -D warnings` typo made four nightly jobs *skipped*,
#      which in a run list is indistinguishable from passed.
#   3. Kani's internal compiler ICEd mid-codegen, so the job died before a
#      single proof ran.
#   4. `cargo kani -p miser-policy` passed on a crate with zero harnesses.
#
# Cases 3 and 4 both exit non-zero or zero respectively for reasons that have
# nothing to do with the properties. So the exit code is not the signal; the
# only signal is whether the log says proofs were attempted and completed.
set -euo pipefail

crate="${1:?usage: kani-prove.sh <crate>}"
log="kani-${crate}.log"

echo "== kani-${crate} =="

# Kani's normal verification failure and its internal-compiler panic both land
# here. tee the whole thing so the log is reviewable as an artifact.
if cargo kani -p "$crate" --output-format regular 2>&1 | tee "$log"; then
  status=0
else
  status=${PIPESTATUS[0]}
fi

summary=$(grep -E '[0-9]+ verification harnesses' "$log" | tail -1 || true)

if [ -z "$summary" ]; then
  # Distinguish the two causes, because they need different fixes and the log
  # is the only place either is visible.
  # Match on what Kani actually prints rather than one exact phrasing: an ICE
  # surfaces as a rustc panic, as "internal compiler error", or as "unexpectedly
  # panicked", and which one shows up varies with the version and the output
  # format. Requiring two exact strings meant the first version of this script
  # mislabelled a real ICE as "proofs may not have run" and sent the reader
  # looking for a harness bug that was not there.
  if grep -qE 'internal compiler error|unexpectedly panicked|kani-compiler/src/.*panicked|assertion failed' "$log"; then
    echo "::error title=Kani ICE::$crate: kani-compiler panicked during codegen, so 0 proofs ran. This is an upstream Kani bug, not a harness defect. See $log."
  elif grep -qiE 'no verification harnesses|0 harnesses' "$log"; then
    echo "::error title=No harnesses::$crate has no #[kani::proof] functions, so this proves nothing. Add harnesses or delete the step -- a green row here is a lie."
  else
    echo "::error title=No verification summary::$crate produced no 'N verification harnesses' line. Proofs may not have run. See $log."
  fi
  exit 1
fi

# A summary line that reports zero successes is Kani's way of saying a
# harness found a counterexample. `cargo kani` already exits non-zero for that,
# so this is belt-and-braces against a future Kani that does not.
if printf '%s' "$summary" | grep -qE '\(0 successful'; then
  echo "::error title=Counterexample::$crate found a counterexample: $summary"
  exit 1
fi

echo "$crate: $summary"
exit "$status"
