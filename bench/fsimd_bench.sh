#!/bin/sh
# fsimd_bench.sh — 浮動小数点の完全列化（#[fast_math]）と条件付き一括更新（Masked Column RMW）の計測: Fascon と手書きの C
# （AoS。素直な -O2 と -ffast-math）を、6 万粒子と 4,194,304 粒子で、GCC / clang × 基本（SSE2）/ -march=native のビルドを
# 交互に走らせて測る
# the float True-SoA (#[fast_math]) and masked column RMW benchmark: Fascon and hand-written C (AoS, plain -O2 and
# -ffast-math), at 60,000 and 4,194,304 particles, with GCC / clang × baseline (SSE2) / -march=native builds run
# alternately
#
#   sh bench/fsimd_bench.sh <fascon> [回数 / rounds] [出力 / out dir]
#   python3 bench/tsoa_summary.py --float <出力 / out dir>/results.tsv
set -e
FASCON=$1
ROUNDS=${2:-5}
OUT=${3:-bench/fsimd-build}
HERE=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$OUT"
: > "$OUT/results.tsv"
BINS=""
for cap in 60000 4194304; do
    d="$OUT/$cap"
    mkdir -p "$d"
    sed "s/@CAP@/$cap/g" "$HERE/fsimd_bench.fsc.in" > "$d/fsimd_bench.fsc"
    "$FASCON" build "$d/fsimd_bench.fsc" --out-dir "$d/gen" > /dev/null
    "$FASCON" regions "$d/fsimd_bench.fsc" --lang en > "$d/regions.txt"
    for cc in gcc clang; do
        for m in base native; do
            fl=""
            [ "$m" = native ] && fl="-march=native"
            "$cc" -O2 -std=c11 $fl -DNT=$cap -DFAST=0 -I"$HERE" -c "$HERE/fsimd_c.c" -o "$d/c-$cc-$m.o"
            "$cc" -O2 -std=c11 $fl -ffast-math -DNT=$cap -DFAST=1 -I"$HERE" -c "$HERE/fsimd_c.c" -o "$d/cf-$cc-$m.o"
            "$cc" -O2 -std=c11 $fl -DNT=$cap -I"$d/gen" -I"$HERE" "$HERE/fsimd_harness.c" "$d/c-$cc-$m.o" "$d/cf-$cc-$m.o" -o "$d/h-$cc-$m"
            BINS="$BINS $d/h-$cc-$m"
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
