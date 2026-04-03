#!/bin/bash
# Wrapper for cwltest conformance tests.
# cwltest calls: runner --outdir=DIR [--quiet] <cwl-file> [job-file]
set -e

OUTDIR=""
CWL_FILE=""
JOB_FILE=""

# Parse args — cwltest passes --outdir and --quiet before positional args
for arg in "$@"; do
    case "$arg" in
        --outdir=*) OUTDIR="${arg#--outdir=}" ;;
        --quiet) ;;  # ignore
        *)
            if [ -z "$CWL_FILE" ]; then
                CWL_FILE="$arg"
            elif [ -z "$JOB_FILE" ]; then
                JOB_FILE="$arg"
            fi
            ;;
    esac
done

if [ -z "$OUTDIR" ]; then
    OUTDIR=$(mktemp -d)
fi

if [ -z "$JOB_FILE" ]; then
    JOB_FILE="$OUTDIR/empty-input.yml"
    mkdir -p "$OUTDIR"
    echo "{}" > "$JOB_FILE"
fi

exec "$(dirname "$0")/target/debug/cwl-zen" run "$CWL_FILE" "$JOB_FILE" --outdir "$OUTDIR" --no-crate --copy-inputs 2>/dev/null
