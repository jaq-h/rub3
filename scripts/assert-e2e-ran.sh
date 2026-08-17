#!/usr/bin/env bash
#
# Fails when an anvil-gated suite reported green without having run.
#
# Two ways that happens, and cargo's exit code shows neither:
#
#   * a test-name filter that selects nothing still exits 0, so renaming a
#     module, changing the cfg that compiles it, or dropping an `#[ignore]`
#     quietly reduces a CI step to a no-op that reports success;
#   * every one of these suites prints `SKIP: ...` and passes when `anvil`,
#     `forge` or `cast` is missing, which is what makes them safe to run on a
#     laptop and unacceptable in a job that requires Foundry on PATH first.
#
# So this reads back what cargo said it ran, rather than trusting the status.
#
# Usage: scripts/assert-e2e-ran.sh <log-file> <expected-passed>

set -euo pipefail

log="${1:?usage: assert-e2e-ran.sh <log-file> <expected-passed>}"
expected="${2:?usage: assert-e2e-ran.sh <log-file> <expected-passed>}"

if [ ! -f "$log" ]; then
    echo "::error::$log does not exist, so nothing is known about the run"
    exit 1
fi

if grep -q 'SKIP:' "$log"; then
    echo "::error::the suite self-skipped, which here means the toolchain broke"
    grep -n 'SKIP:' "$log" >&2
    exit 1
fi

# Summed over every `test result:` line, so the answer describes the whole run
# and not whichever target happened to print last.
passed=$(awk '
    /^test result:/ {
        for (i = 1; i < NF; i++) {
            if ($(i + 1) == "passed;") total += $i
        }
    }
    END { print total + 0 }
' "$log")

if [ "$passed" -ne "$expected" ]; then
    echo "::error::expected $expected passing tests in $log, cargo reported $passed"
    grep '^test result:' "$log" >&2 ||
        echo "there is no 'test result:' line at all - the filter selected nothing" >&2
    exit 1
fi

echo "ok: $passed tests ran and passed"
