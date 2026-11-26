#!/bin/sh
# tsoa_bench.sh — 完全列化（True SoA）の計測: 改修前と改修後の Fascon と手書きの C（AoS）を、6 万タスクと 4,194,304 タスクで
# GCC / clang × 基本（SSE2）/ -march=native のビルドを交互に走らせて測る
# the True-SoA benchmark: Fascon before and after the change and hand-written C (AoS), at 60,000 and 4,194,304 tasks,
# with GCC / clang × baseline (SSE2) / -march=native builds run alternately
#
#   sh bench/tsoa_bench.sh <改修前の fascon / fascon before> <改修後の fascon / fascon after> [回数 / rounds] [出力 / out dir]
#   python3 bench/tsoa_summary.py <出力 / out dir>/results.tsv
set -e
BEFORE=$1
AFTER=$2
ROUNDS=${3:-5}
OUT=${4:-bench/tsoa-build}
HERE=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$OUT"
: > "$OUT/results.tsv"
BINS=""
for cap in 60000 4194304; do
    d="$OUT/$cap"
    mkdir -p "$d"
    sed "s/@CAP@/$cap/g" "$HERE/tsoa_bench.fsc.in" > "$d/tsoa_bench.fsc"
    "$BEFORE" build "$d/tsoa_bench.fsc" --out-dir "$d/before" > /dev/null
    "$AFTER" build "$d/tsoa_bench.fsc" --out-dir "$d/after" > /dev/null
    "$AFTER" regions "$d/tsoa_bench.fsc" --lang en > "$d/regions-after.txt"
    "$BEFORE" regions "$d/tsoa_bench.fsc" --lang en > "$d/regions-before.txt"
    for cc in gcc clang; do
        for m in base native; do
            fl=""
            [ "$m" = native ] && fl="-march=native"
            for v in before after; do
                "$cc" -O2 -std=c11 $fl -DNT=$cap -I"$d/$v" "$HERE/tsoa_harness.c" -o "$d/h-$v-$cc-$m"
                BINS="$BINS $d/h-$v-$cc-$m"
            done
        done
    done
done
r=1
while [ $r -le "$ROUNDS" ]; do
    for b in $BINS; do
        name=$(basename "$b")
        cap=$(basename "$(dirname "$b")")
        "$b" "$cap ${name#h-}" | sed "s/^/$r\t/" >> "$OUT/results.tsv"
    done
    r=$((r + 1))
done
echo "results: $OUT/results.tsv"
