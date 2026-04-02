#!/bin/bash
# Wrapper for cwltest conformance tests.
# cwltest calls: cwl-zen-runner.sh <cwl-file> [job-file]
set -e
CWL_FILE="$1"
JOB_FILE="${2:---}"
OUTDIR=$(mktemp -d)

if [ "$JOB_FILE" = "--" ] || [ -z "$JOB_FILE" ]; then
    # No input file — create empty one
    JOB_FILE="$OUTDIR/empty-input.yml"
    echo "{}" > "$JOB_FILE"
fi

exec "$(dirname "$0")/target/debug/cwl-zen" run "$CWL_FILE" "$JOB_FILE" --outdir "$OUTDIR" --no-crate 2>/dev/null
