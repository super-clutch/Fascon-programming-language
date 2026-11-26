# Traversal benchmark / 走査ベンチマーク

Summing `vruntime` over 60,000 task_struct-like Tasks. The Fascon side is the generated C of `sched_bench.fsc`, called unchanged from `harness.c`; the baseline is ordinary C with the same Task allocated by malloc and linked by raw pointers.
task_struct 風の Task 6 万個の `vruntime` を合計します。Fascon 側は `sched_bench.fsc` の生成 C を `harness.c` からそのまま呼び、比較相手は同じ Task を malloc して生ポインタでつないだ普通の C です。

```text
fascon build bench/sched_bench.fsc --out-dir bench/build
cc -O2 -std=c11 -Ibench/build bench/harness.c -o bench/build/harness
bench/build/harness                          # 2 MiB pages for the arena (madvise, Linux)
FASCON_NO_HUGEPAGE=1 bench/build/harness     # 4 KiB pages
```

## November 2026, masked column RMW and float columns (version unchanged) / 2026-11 の改修: 条件付き一括更新と浮動小数点の列（版は 1.0 のまま）

Two measurements on one host, after the additions of docs/SPEC-1.0-HPC.md: the task benchmark of the True-SoA section again, now Fascon phase 1 (True SoA) against Fascon now (masked column RMW), and a new float benchmark (`bench/fsimd_bench.fsc.in`: a 32-byte Particle with `x: f32, vx: f32, mass: f64, energy: f64, kind: u32, tag: u32`) against hand-written C (AoS) at plain `-O2` and at `-ffast-math` (the C loops compiled in a separate unit, linked without `-ffast-math` so the MXCSR of the program is unchanged). The float harness checks after every write that Fascon and both C arrays hold the same bits (integer sums of the bit patterns). See docs/PERF-1.0.md, phase 6. / docs/SPEC-1.0-HPC.ja.md の追加のあと、同じホストで 2 つ測りました。完全列化の節のタスクのベンチマークを、フェーズ 1（完全列化）の Fascon と今の Fascon（条件付き一括更新）でもう一度。新しい浮動小数点のベンチマーク（`bench/fsimd_bench.fsc.in`: 32 バイトの Particle `x: f32, vx: f32, mass: f64, energy: f64, kind: u32, tag: u32`）を、手書きの C（AoS）の素直な `-O2` と `-ffast-math`（C のループは別の翻訳単位でコンパイルし、`-ffast-math` なしでリンク: プログラムの MXCSR は変えない）と比べます。浮動小数点のハーネスは書き込みのたびに、Fascon と 2 つの C の配列が同じビットを持つか（ビットの並みの整数の和）を確かめます。docs/PERF-1.0.ja.md のフェーズ 6 を参照。

```text
sh bench/tsoa_bench.sh <fascon phase 1> <fascon now> 9 bench/tsoa-build
python3 bench/tsoa_summary.py bench/tsoa-build/results.tsv --names "Fascon phase 1 (True SoA),Fascon now (masked RMW)"
sh bench/fsimd_bench.sh <fascon> 9 bench/fsimd-build
python3 bench/tsoa_summary.py --float bench/fsimd-build/results.tsv
```

Machine / 機械: Intel Xeon @ 2.10 GHz, KVM virtual machine, 2 vCPUs, 2 MiB L2 per core, 260 MiB L3, Linux with transparent huge pages in `madvise` mode (2 MiB pages for the Fascon tables and the C arrays); GCC 13.3 and clang 18.1 at `-O2 -std=c11`, baseline (SSE2) and `-march=native` (AVX-512). Each benchmark ran its binaries alternately, 9 rounds (16 binaries for tasks, 8 for particles); medians of the 9 rounds, ns per task / particle, cycles in parentheses at each round's own clock. / それぞれの実行ファイルを交互に 9 回（タスクは 16 本、粒子は 8 本）。9 回の中央値で、1 タスク / 1 粒子あたりの ns、かっこはその回のクロックで換算したクロック数です。

### Tasks: phase 1 against now / タスク: フェーズ 1 と今

core clock: 2.66-3.85 GHz over the runs (IMUL chain, median 3.33); ns per task (cycles per task, each run at its own clock); medians over the rounds

**60000 tasks, gcc, baseline (SSE2)**

| operation | C (AoS) | Fascon phase 1 (True SoA) | Fascon now (masked RMW) | after / C |
|---|---:|---:|---:|---:|
| read: sum of vruntime (vruntime_scan) | 0.478 (1.58) | 0.117 (0.39) | 0.117 (0.39) | 0.25 |
| read: vruntime sum of runnable tasks | 0.882 (2.91) | 0.217 (0.73) | 0.200 (0.67) | 0.23 |
| read: count of runnable tasks | 0.602 (2.01) | 0.024 (0.08) | 0.022 (0.07) | 0.04 |
| read: pick_next (min vruntime, runnable) | 0.616 (2.05) | 0.223 (0.79) | 0.220 (0.74) | 0.36 |
| write: vruntime += 1, runnable tasks | 0.814 (2.72) | 0.891 (3.02) | 0.659 (2.22) | 0.82 |
| write: vruntime += 1, every task | 0.718 (2.45) | 0.630 (2.12) | 0.611 (2.04) | 0.83 |
| write: vruntime = x, every task | 0.790 (2.63) | 0.347 (1.17) | 0.326 (1.09) | 0.41 |

**60000 tasks, gcc, -march=native**

| operation | C (AoS) | Fascon phase 1 (True SoA) | Fascon now (masked RMW) | after / C |
|---|---:|---:|---:|---:|
| read: sum of vruntime (vruntime_scan) | 0.480 (1.61) | 0.052 (0.17) | 0.052 (0.17) | 0.11 |
| read: vruntime sum of runnable tasks | 0.891 (2.99) | 0.128 (0.44) | 0.127 (0.43) | 0.14 |
| read: count of runnable tasks | 0.602 (2.07) | 0.005 (0.02) | 0.005 (0.02) | 0.01 |
| read: pick_next (min vruntime, runnable) | 0.849 (2.83) | 0.166 (0.56) | 0.169 (0.56) | 0.20 |
| write: vruntime += 1, runnable tasks | 0.885 (2.97) | 0.861 (2.90) | 0.210 (0.70) | 0.24 |
| write: vruntime += 1, every task | 0.699 (2.36) | 0.604 (2.05) | 0.604 (2.04) | 0.86 |
| write: vruntime = x, every task | 0.759 (2.57) | 0.340 (1.16) | 0.340 (1.16) | 0.45 |

**60000 tasks, clang, baseline (SSE2)**

| operation | C (AoS) | Fascon phase 1 (True SoA) | Fascon now (masked RMW) | after / C |
|---|---:|---:|---:|---:|
| read: sum of vruntime (vruntime_scan) | 0.459 (1.60) | 0.098 (0.33) | 0.101 (0.34) | 0.21 |
| read: vruntime sum of runnable tasks | 0.581 (1.97) | 0.193 (0.67) | 0.192 (0.66) | 0.34 |
| read: count of runnable tasks | 0.471 (1.64) | 0.011 (0.04) | 0.011 (0.04) | 0.02 |
| read: pick_next (min vruntime, runnable) | 0.603 (2.08) | 0.149 (0.51) | 0.150 (0.51) | 0.25 |
| write: vruntime += 1, runnable tasks | 0.613 (2.10) | 0.833 (2.84) | 0.638 (2.20) | 1.05 |
| write: vruntime += 1, every task | 0.714 (2.40) | 0.605 (2.05) | 0.627 (2.09) | 0.87 |
| write: vruntime = x, every task | 0.796 (2.68) | 0.601 (2.05) | 0.320 (1.08) | 0.40 |

**60000 tasks, clang, -march=native**

| operation | C (AoS) | Fascon phase 1 (True SoA) | Fascon now (masked RMW) | after / C |
|---|---:|---:|---:|---:|
| read: sum of vruntime (vruntime_scan) | 0.495 (1.64) | 0.076 (0.25) | 0.070 (0.23) | 0.14 |
| read: vruntime sum of runnable tasks | 0.560 (1.85) | 0.127 (0.42) | 0.116 (0.39) | 0.21 |
| read: count of runnable tasks | 0.499 (1.61) | 0.002 (0.01) | 0.002 (0.01) | 0.00 |
| read: pick_next (min vruntime, runnable) | 0.635 (2.10) | 0.216 (0.66) | 0.178 (0.59) | 0.28 |
| write: vruntime += 1, runnable tasks | 0.471 (1.58) | 0.921 (3.03) | 0.201 (0.68) | 0.43 |
| write: vruntime += 1, every task | 0.693 (2.30) | 0.590 (1.93) | 0.448 (1.50) | 0.65 |
| write: vruntime = x, every task | 0.764 (2.57) | 0.357 (1.20) | 0.373 (1.24) | 0.48 |

**4194304 tasks, gcc, baseline (SSE2)**

| operation | C (AoS) | Fascon phase 1 (True SoA) | Fascon now (masked RMW) | after / C |
|---|---:|---:|---:|---:|
| read: sum of vruntime (vruntime_scan) | 1.139 (3.84) | 0.251 (0.85) | 0.262 (0.87) | 0.23 |
| read: vruntime sum of runnable tasks | 1.174 (3.93) | 0.283 (0.98) | 0.296 (1.04) | 0.27 |
| read: count of runnable tasks | 1.012 (3.38) | 0.029 (0.10) | 0.029 (0.10) | 0.03 |
| read: pick_next (min vruntime, runnable) | 1.073 (3.57) | 0.294 (0.97) | 0.294 (0.98) | 0.28 |
| write: vruntime += 1, runnable tasks | 1.192 (3.98) | 1.173 (4.02) | 0.673 (2.27) | 0.57 |
| write: vruntime += 1, every task | 1.236 (4.13) | 0.626 (2.11) | 0.633 (2.09) | 0.51 |
| write: vruntime = x, every task | 1.232 (4.06) | 0.445 (1.50) | 0.446 (1.49) | 0.37 |

**4194304 tasks, gcc, -march=native**

| operation | C (AoS) | Fascon phase 1 (True SoA) | Fascon now (masked RMW) | after / C |
|---|---:|---:|---:|---:|
| read: sum of vruntime (vruntime_scan) | 1.094 (3.81) | 0.264 (0.88) | 0.281 (0.94) | 0.25 |
| read: vruntime sum of runnable tasks | 1.159 (3.89) | 0.253 (0.84) | 0.256 (0.87) | 0.22 |
| read: count of runnable tasks | 1.039 (3.46) | 0.005 (0.02) | 0.005 (0.02) | 0.00 |
| read: pick_next (min vruntime, runnable) | 1.085 (3.62) | 0.269 (0.89) | 0.271 (0.91) | 0.25 |
| write: vruntime += 1, runnable tasks | 1.156 (3.98) | 1.118 (3.70) | 0.284 (0.95) | 0.24 |
| write: vruntime += 1, every task | 1.185 (4.02) | 0.628 (2.08) | 0.648 (2.22) | 0.55 |
| write: vruntime = x, every task | 1.225 (4.11) | 0.449 (1.50) | 0.447 (1.54) | 0.37 |

**4194304 tasks, clang, baseline (SSE2)**

| operation | C (AoS) | Fascon phase 1 (True SoA) | Fascon now (masked RMW) | after / C |
|---|---:|---:|---:|---:|
| read: sum of vruntime (vruntime_scan) | 1.112 (3.67) | 0.255 (0.87) | 0.244 (0.81) | 0.22 |
| read: vruntime sum of runnable tasks | 1.094 (3.66) | 0.306 (0.99) | 0.302 (1.00) | 0.27 |
| read: count of runnable tasks | 1.059 (3.53) | 0.011 (0.04) | 0.011 (0.04) | 0.01 |
| read: pick_next (min vruntime, runnable) | 1.086 (3.64) | 0.262 (0.88) | 0.281 (0.92) | 0.25 |
| write: vruntime += 1, runnable tasks | 1.170 (3.89) | 1.150 (3.81) | 0.652 (2.16) | 0.56 |
| write: vruntime += 1, every task | 1.209 (4.02) | 0.518 (1.72) | 0.497 (1.69) | 0.42 |
| write: vruntime = x, every task | 1.281 (4.23) | 0.443 (1.48) | 0.439 (1.46) | 0.34 |

**4194304 tasks, clang, -march=native**

| operation | C (AoS) | Fascon phase 1 (True SoA) | Fascon now (masked RMW) | after / C |
|---|---:|---:|---:|---:|
| read: sum of vruntime (vruntime_scan) | 1.069 (3.59) | 0.269 (0.91) | 0.269 (0.89) | 0.25 |
| read: vruntime sum of runnable tasks | 1.065 (3.54) | 0.270 (0.91) | 0.265 (0.88) | 0.25 |
| read: count of runnable tasks | 1.066 (3.56) | 0.002 (0.01) | 0.002 (0.01) | 0.00 |
| read: pick_next (min vruntime, runnable) | 1.079 (3.64) | 0.256 (0.87) | 0.248 (0.81) | 0.22 |
| write: vruntime += 1, runnable tasks | 1.147 (3.85) | 1.124 (3.92) | 0.285 (0.96) | 0.25 |
| write: vruntime += 1, every task | 1.191 (3.96) | 0.492 (1.65) | 0.570 (1.90) | 0.48 |
| write: vruntime = x, every task | 1.212 (4.08) | 0.449 (1.51) | 0.471 (1.56) | 0.38 |

### Particles: float columns and masked updates / 粒子: 浮動小数点の列と条件付きの更新

core clock: 2.68-3.63 GHz over the runs (IMUL chain, median 3.29); ns per particle (cycles per particle, each run at its own clock); medians over the rounds

**60000 particles, gcc, baseline (SSE2)**

| operation | C (AoS, -O2) | C (AoS, -ffast-math) | Fascon | Fascon / C | Fascon / C fast-math |
|---|---:|---:|---:|---:|---:|
| read: sum of mass (f64) | 0.689 (2.24) | 0.660 (2.15) | 0.116 (0.37) | 0.17 | 0.17 |
| read: sum of x (f32) | 0.641 (2.12) | 0.678 (2.26) | 0.065 (0.21) | 0.10 | 0.09 |
| read: energy sum of HOT (f64, filtered) | 1.010 (3.36) | 0.896 (2.98) | 0.299 (1.00) | 0.30 | 0.33 |
| write: energy += 0.5, HOT (f64 masked add) | 0.863 (2.86) | 0.891 (2.85) | 0.543 (1.80) | 0.63 | 0.63 |
| write: x += 1.0, HOT (f32 masked add) | 0.873 (2.87) | 0.826 (2.76) | 0.279 (0.93) | 0.32 | 0.34 |
| write: energy = e, COLD (f64 masked set) | 1.043 (3.48) | 0.821 (2.72) | 0.506 (1.68) | 0.48 | 0.62 |
| write: mass = m, every particle (f64) | 0.778 (2.59) | 0.777 (2.56) | 0.329 (1.09) | 0.42 | 0.43 |

**60000 particles, gcc, -march=native**

| operation | C (AoS, -O2) | C (AoS, -ffast-math) | Fascon | Fascon / C | Fascon / C fast-math |
|---|---:|---:|---:|---:|---:|
| read: sum of mass (f64) | 0.644 (2.12) | 0.632 (2.10) | 0.050 (0.17) | 0.08 | 0.08 |
| read: sum of x (f32) | 0.635 (2.05) | 0.628 (2.09) | 0.026 (0.09) | 0.04 | 0.04 |
| read: energy sum of HOT (f64, filtered) | 0.975 (3.21) | 1.081 (3.60) | 0.114 (0.36) | 0.11 | 0.10 |
| write: energy += 0.5, HOT (f64 masked add) | 3.375 (10.70) | 3.265 (10.70) | 0.158 (0.52) | 0.05 | 0.05 |
| write: x += 1.0, HOT (f32 masked add) | 3.420 (11.25) | 3.369 (10.92) | 0.077 (0.26) | 0.02 | 0.02 |
| write: energy = e, COLD (f64 masked set) | 0.940 (3.00) | 1.111 (3.66) | 0.154 (0.51) | 0.17 | 0.14 |
| write: mass = m, every particle (f64) | 0.780 (2.50) | 0.780 (2.46) | 0.597 (1.95) | 0.78 | 0.79 |

**60000 particles, clang, baseline (SSE2)**

| operation | C (AoS, -O2) | C (AoS, -ffast-math) | Fascon | Fascon / C | Fascon / C fast-math |
|---|---:|---:|---:|---:|---:|
| read: sum of mass (f64) | 0.668 (2.21) | 0.426 (1.38) | 0.092 (0.31) | 0.14 | 0.23 |
| read: sum of x (f32) | 0.640 (2.15) | 0.421 (1.44) | 0.053 (0.19) | 0.09 | 0.13 |
| read: energy sum of HOT (f64, filtered) | 0.611 (2.06) | 0.632 (2.15) | 0.344 (1.14) | 0.55 | 0.53 |
| write: energy += 0.5, HOT (f64 masked add) | 0.580 (1.92) | 0.561 (1.83) | 0.472 (1.57) | 0.82 | 0.86 |
| write: x += 1.0, HOT (f32 masked add) | 0.623 (2.13) | 0.550 (1.88) | 0.267 (0.88) | 0.41 | 0.47 |
| write: energy = e, COLD (f64 masked set) | 0.655 (2.17) | 0.652 (2.21) | 0.444 (1.51) | 0.70 | 0.68 |
| write: mass = m, every particle (f64) | 0.757 (2.52) | 0.805 (2.63) | 0.322 (1.08) | 0.43 | 0.41 |

**60000 particles, clang, -march=native**

| operation | C (AoS, -O2) | C (AoS, -ffast-math) | Fascon | Fascon / C | Fascon / C fast-math |
|---|---:|---:|---:|---:|---:|
| read: sum of mass (f64) | 0.619 (2.09) | 0.471 (1.62) | 0.060 (0.20) | 0.10 | 0.13 |
| read: sum of x (f32) | 0.619 (2.08) | 0.415 (1.41) | 0.032 (0.11) | 0.05 | 0.08 |
| read: energy sum of HOT (f64, filtered) | 0.449 (1.53) | 0.554 (1.86) | 0.104 (0.35) | 0.23 | 0.19 |
| write: energy += 0.5, HOT (f64 masked add) | 0.465 (1.59) | 0.460 (1.54) | 0.159 (0.53) | 0.33 | 0.34 |
| write: x += 1.0, HOT (f32 masked add) | 0.462 (1.58) | 0.455 (1.55) | 0.078 (0.26) | 0.17 | 0.17 |
| write: energy = e, COLD (f64 masked set) | 0.465 (1.55) | 0.457 (1.57) | 0.150 (0.51) | 0.33 | 0.32 |
| write: mass = m, every particle (f64) | 0.769 (2.63) | 0.751 (2.58) | 0.593 (2.03) | 0.77 | 0.79 |

**4194304 particles, gcc, baseline (SSE2)**

| operation | C (AoS, -O2) | C (AoS, -ffast-math) | Fascon | Fascon / C | Fascon / C fast-math |
|---|---:|---:|---:|---:|---:|
| read: sum of mass (f64) | 1.146 (3.74) | 1.176 (3.91) | 0.245 (0.81) | 0.22 | 0.21 |
| read: sum of x (f32) | 1.200 (3.94) | 1.153 (3.93) | 0.124 (0.42) | 0.11 | 0.11 |
| read: energy sum of HOT (f64, filtered) | 1.472 (4.80) | 1.644 (5.47) | 0.346 (1.12) | 0.23 | 0.21 |
| write: energy += 0.5, HOT (f64 masked add) | 1.437 (4.68) | 1.453 (4.84) | 0.584 (2.00) | 0.43 | 0.41 |
| write: x += 1.0, HOT (f32 masked add) | 2.122 (6.96) | 2.596 (8.59) | 0.284 (0.96) | 0.14 | 0.11 |
| write: energy = e, COLD (f64 masked set) | 1.673 (5.54) | 1.702 (5.91) | 0.529 (1.76) | 0.32 | 0.30 |
| write: mass = m, every particle (f64) | 1.482 (4.94) | 1.915 (6.24) | 0.451 (1.49) | 0.30 | 0.24 |

**4194304 particles, gcc, -march=native**

| operation | C (AoS, -O2) | C (AoS, -ffast-math) | Fascon | Fascon / C | Fascon / C fast-math |
|---|---:|---:|---:|---:|---:|
| read: sum of mass (f64) | 1.195 (3.93) | 1.327 (4.19) | 0.284 (0.90) | 0.23 | 0.21 |
| read: sum of x (f32) | 1.309 (4.27) | 1.439 (4.36) | 0.135 (0.45) | 0.10 | 0.10 |
| read: energy sum of HOT (f64, filtered) | 1.645 (5.48) | 2.477 (6.76) | 0.277 (0.88) | 0.16 | 0.13 |
| write: energy += 0.5, HOT (f64 masked add) | 4.921 (15.58) | 4.748 (13.88) | 0.291 (0.95) | 0.06 | 0.07 |
| write: x += 1.0, HOT (f32 masked add) | 4.605 (13.89) | 4.722 (13.65) | 0.146 (0.49) | 0.04 | 0.04 |
| write: energy = e, COLD (f64 masked set) | 1.717 (4.72) | 2.012 (6.68) | 0.296 (0.96) | 0.20 | 0.14 |
| write: mass = m, every particle (f64) | 3.527 (9.45) | 3.832 (11.89) | 0.578 (1.89) | 0.20 | 0.16 |

**4194304 particles, clang, baseline (SSE2)**

| operation | C (AoS, -O2) | C (AoS, -ffast-math) | Fascon | Fascon / C | Fascon / C fast-math |
|---|---:|---:|---:|---:|---:|
| read: sum of mass (f64) | 1.588 (5.19) | 1.234 (3.72) | 0.277 (0.88) | 0.17 | 0.24 |
| read: sum of x (f32) | 1.401 (4.07) | 1.398 (3.89) | 0.137 (0.42) | 0.10 | 0.11 |
| read: energy sum of HOT (f64, filtered) | 1.513 (4.21) | 1.390 (3.86) | 0.388 (1.19) | 0.28 | 0.31 |
| write: energy += 0.5, HOT (f64 masked add) | 1.545 (4.47) | 1.696 (5.41) | 0.517 (1.61) | 0.36 | 0.30 |
| write: x += 1.0, HOT (f32 masked add) | 1.293 (4.08) | 1.475 (4.58) | 0.265 (0.88) | 0.21 | 0.19 |
| write: energy = e, COLD (f64 masked set) | 1.307 (4.34) | 1.422 (4.62) | 0.504 (1.51) | 0.35 | 0.33 |
| write: mass = m, every particle (f64) | 3.390 (10.04) | 2.266 (7.36) | 0.457 (1.46) | 0.15 | 0.20 |

**4194304 particles, clang, -march=native**

| operation | C (AoS, -O2) | C (AoS, -ffast-math) | Fascon | Fascon / C | Fascon / C fast-math |
|---|---:|---:|---:|---:|---:|
| read: sum of mass (f64) | 1.224 (4.01) | 1.190 (3.77) | 0.273 (0.89) | 0.22 | 0.24 |
| read: sum of x (f32) | 1.202 (3.91) | 1.118 (3.69) | 0.134 (0.44) | 0.11 | 0.12 |
| read: energy sum of HOT (f64, filtered) | 1.189 (3.77) | 1.108 (3.85) | 0.273 (0.89) | 0.23 | 0.23 |
| write: energy += 0.5, HOT (f64 masked add) | 1.227 (4.00) | 1.240 (4.04) | 0.299 (0.98) | 0.24 | 0.24 |
| write: x += 1.0, HOT (f32 masked add) | 1.293 (4.32) | 1.216 (3.96) | 0.155 (0.51) | 0.12 | 0.13 |
| write: energy = e, COLD (f64 masked set) | 1.251 (4.16) | 1.249 (4.22) | 0.297 (0.98) | 0.24 | 0.23 |
| write: mass = m, every particle (f64) | 1.567 (4.94) | 2.075 (6.81) | 0.396 (1.32) | 0.27 | 0.19 |

- **The write to runnable tasks only (`vruntime += 1`, about a quarter of the tasks, so nearly every 64-slot word is mixed) is now a masked u64 add**: 0.82× / 0.24× / 1.05× / 0.43× C with 60,000 tasks (GCC baseline / GCC native / clang baseline / clang native) and 0.24–0.57× with 4 million, against 0.97–1.91× and 0.93–1.02× for phase 1 in the same run. clang baseline (SSE2, two u64 lanes, carry checks in the top bit) stays on par with the C loop. / 実行可能なタスクだけの書き込み（`vruntime += 1`。4 分の 1 ほどのタスクなので、ほぼすべての 64 スロットの語がばらばら）は u64 の条件付き加算になり、6 万タスクで C の 0.82 / 0.24 / 1.05 / 0.43 倍（GCC 基本・GCC native・clang 基本・clang native）、400 万タスクで 0.24〜0.57 倍。同じ実行でのフェーズ 1 は 0.97〜1.91 倍・0.93〜1.02 倍でした。clang の基本（SSE2: u64 のレーン 2 本、あふれは最上位ビットで検査）は C のループと同じくらいです。
- **Float reads under `#[fast_math]` take 0.09–1.19 cycles per particle**, 0.04–0.55× of plain C and 0.04–0.53× of C at `-ffast-math` (the AoS loops load 4 or 8 bytes out of every 32). / `#[fast_math]` の浮動小数点の読み出しは 1 粒子 0.09〜1.19 クロック、素直な C の 0.04〜0.55 倍、`-ffast-math` の C の 0.04〜0.53 倍です（AoS のループは 32 バイトごとに 4 か 8 バイトを読む）。
- **Masked float writes take 0.26–2.00 cycles, 0.02–0.82× plain C and 0.02–0.86× C at `-ffast-math`; the bulk overwrite of every particle takes 0.15–0.78× C.** GCC `-march=native` compiles the conditional C adds into AVX-512 masked operations that take 10.7–15.6 cycles here, so those ratios (0.02–0.06) say more about that C than about Fascon; the clang native ratios of the masked writes (0.12–0.33) are the fairer reference. / 条件付きの浮動小数点の書き込みは 0.26〜2.00 クロック、素直な C の 0.02〜0.82 倍・`-ffast-math` の C の 0.02〜0.86 倍、全粒子の一括上書きは C の 0.15〜0.78 倍。GCC の `-march=native` は C の条件付きの加算を AVX-512 のマスク付きの演算にし、ここでは 10.7〜15.6 クロックかかるので、その比（0.02〜0.06）は Fascon よりもその C を語っています。条件付きの書き込みの clang native の比（0.12〜0.33）のほうが公平な目安です。
- C (AoS) with 4 million tasks took 3.4–4.3 cycles in this run, against 5.6–13.6 in the True-SoA run below on the same kind of host: the 128 MB array fits in the 260 MiB L3, and how much of it a run gets depends on the other guests. Compare within one run. / 400 万タスクの C（AoS）はこの実行で 3.4〜4.3 クロック、下の完全列化の実行（同じ種類のホスト）では 5.6〜13.6 でした。128 MB の配列は 260 MiB の L3 に収まり、そのうちどれだけを使えるかはほかの仮想機械しだいです。比べるのは同じ実行の中だけにしてください。

## November 2026, True SoA (version unchanged) / 2026-11 の改修: 完全列化（版は 1.0 のまま）

Reads and writes of `vruntime` over a task_struct-like Task (32 bytes) with 60,000 and 4,194,304 tasks, in three versions: hand-written C (AoS: an array of the same struct, plain loops, no overflow checks), Fascon before True SoA (the mirrored columns of PERF-1.0.md phase 4) and Fascon after it (`--columns on --nontemporal auto`, the defaults). See docs/PERF-1.0.md, phase 5. / task_struct 風の Task（32 バイト）の `vruntime` の読み出しと書き込みを、6 万タスクと 4,194,304 タスクで、手書きの C（AoS: 同じ構造体の配列を素直なループで。あふれの検査なし）・完全列化の前の Fascon（PERF-1.0.ja.md フェーズ 4 の写しの列）・完全列化のあとの Fascon（既定の `--columns on --nontemporal auto`）の 3 つで測ります。docs/PERF-1.0.ja.md のフェーズ 5 を参照。

```text
sh bench/tsoa_bench.sh <fascon before> <fascon after> 9 bench/tsoa-build
python3 bench/tsoa_summary.py bench/tsoa-build/results.tsv
```

Machine / 機械: Intel Xeon, KVM virtual machine, 2 vCPUs, 2 MiB L2 per core, 260 MiB L3, Linux with transparent huge pages in `madvise` mode (the harness asks for 2 MiB pages for both the Fascon table and the C array); GCC 13.3 and clang 18.1 at `-O2 -std=c11`, baseline (SSE2) and `-march=native` (AVX-512). 16 binaries (2 sizes × GCC / clang × baseline / native × before / after) were run alternately, 9 rounds; each number is the median of the 9 rounds (each round is the harness's median of 20 × 15 timings for 60,000 tasks and 3 × 9 for 4 million). ns per task, and in parentheses cycles per task at each round's own clock (a chain of dependent IMULs at the start of the round). / 16 本を交互に 9 回。数字は 9 回の中央値（各回はハーネスの中央値）で、1 タスクあたりの ns、かっこの中はその回の最初に IMUL の鎖で測ったクロックで換算した 1 タスクあたりのクロック数です。

core clock: 2.87-3.27 GHz over the runs (IMUL chain, median 3.06); ns per task (cycles per task, each run at its own clock); medians over the rounds

**60000 tasks, gcc, baseline (SSE2)**

| operation | C (AoS) | Fascon before (mirrored) | Fascon after (True SoA) | after / C |
|---|---:|---:|---:|---:|
| read: sum of vruntime (vruntime_scan) | 0.538 (1.62) | 0.135 (0.41) | 0.133 (0.40) | 0.25 |
| read: vruntime sum of runnable tasks | 1.000 (3.04) | 0.133 (0.41) | 0.227 (0.69) | 0.23 |
| read: count of runnable tasks | 0.645 (1.93) | 0.025 (0.08) | 0.025 (0.08) | 0.04 |
| read: pick_next (min vruntime, runnable) | 0.677 (2.08) | 0.252 (0.76) | 0.245 (0.75) | 0.36 |
| write: vruntime += 1, runnable tasks | 0.948 (2.89) | 1.799 (5.48) | 0.994 (2.98) | 1.03 |
| write: vruntime += 1, every task | 0.796 (2.47) | 2.626 (8.11) | 0.684 (2.10) | 0.85 |
| write: vruntime = x, every task | 0.847 (2.61) | 2.744 (8.48) | 0.365 (1.14) | 0.44 |

**60000 tasks, gcc, -march=native**

| operation | C (AoS) | Fascon before (mirrored) | Fascon after (True SoA) | after / C |
|---|---:|---:|---:|---:|
| read: sum of vruntime (vruntime_scan) | 0.502 (1.56) | 0.057 (0.18) | 0.058 (0.18) | 0.11 |
| read: vruntime sum of runnable tasks | 0.990 (3.09) | 0.058 (0.18) | 0.144 (0.45) | 0.15 |
| read: count of runnable tasks | 0.634 (1.97) | 0.005 (0.02) | 0.005 (0.02) | 0.01 |
| read: pick_next (min vruntime, runnable) | 0.793 (2.52) | 0.120 (0.36) | 0.188 (0.58) | 0.23 |
| write: vruntime += 1, runnable tasks | 0.980 (3.09) | 1.798 (5.68) | 0.956 (2.99) | 0.97 |
| write: vruntime += 1, every task | 0.764 (2.38) | 2.648 (8.35) | 0.666 (2.05) | 0.86 |
| write: vruntime = x, every task | 0.829 (2.60) | 2.635 (8.44) | 0.375 (1.17) | 0.45 |

**60000 tasks, clang, baseline (SSE2)**

| operation | C (AoS) | Fascon before (mirrored) | Fascon after (True SoA) | after / C |
|---|---:|---:|---:|---:|
| read: sum of vruntime (vruntime_scan) | 0.491 (1.54) | 0.112 (0.35) | 0.108 (0.34) | 0.22 |
| read: vruntime sum of runnable tasks | 0.649 (1.99) | 0.111 (0.35) | 0.215 (0.67) | 0.34 |
| read: count of runnable tasks | 0.508 (1.60) | 0.012 (0.04) | 0.012 (0.04) | 0.02 |
| read: pick_next (min vruntime, runnable) | 0.639 (1.97) | 0.228 (0.71) | 0.168 (0.52) | 0.26 |
| write: vruntime += 1, runnable tasks | 0.603 (1.86) | 1.777 (5.53) | 0.896 (2.73) | 1.47 |
| write: vruntime += 1, every task | 0.783 (2.44) | 2.616 (8.12) | 0.673 (2.04) | 0.84 |
| write: vruntime = x, every task | 0.833 (2.55) | 2.591 (7.85) | 0.658 (1.98) | 0.78 |

**60000 tasks, clang, -march=native**

| operation | C (AoS) | Fascon before (mirrored) | Fascon after (True SoA) | after / C |
|---|---:|---:|---:|---:|
| read: sum of vruntime (vruntime_scan) | 0.521 (1.60) | 0.076 (0.23) | 0.077 (0.24) | 0.15 |
| read: vruntime sum of runnable tasks | 0.581 (1.81) | 0.079 (0.24) | 0.132 (0.40) | 0.22 |
| read: count of runnable tasks | 0.512 (1.55) | 0.003 (0.01) | 0.003 (0.01) | 0.01 |
| read: pick_next (min vruntime, runnable) | 0.597 (1.81) | 0.091 (0.28) | 0.201 (0.62) | 0.34 |
| write: vruntime += 1, runnable tasks | 0.506 (1.52) | 1.778 (5.51) | 0.960 (2.89) | 1.90 |
| write: vruntime += 1, every task | 0.784 (2.35) | 2.724 (8.20) | 0.605 (1.85) | 0.79 |
| write: vruntime = x, every task | 0.847 (2.56) | 2.571 (7.92) | 0.406 (1.22) | 0.48 |

**4194304 tasks, gcc, baseline (SSE2)**

| operation | C (AoS) | Fascon before (mirrored) | Fascon after (True SoA) | after / C |
|---|---:|---:|---:|---:|
| read: sum of vruntime (vruntime_scan) | 2.635 (8.15) | 0.285 (0.89) | 0.303 (0.91) | 0.11 |
| read: vruntime sum of runnable tasks | 3.226 (9.91) | 0.292 (0.90) | 0.322 (0.97) | 0.10 |
| read: count of runnable tasks | 3.439 (10.49) | 0.033 (0.10) | 0.034 (0.10) | 0.01 |
| read: pick_next (min vruntime, runnable) | 3.311 (10.10) | 0.350 (1.08) | 0.338 (1.03) | 0.10 |
| write: vruntime += 1, runnable tasks | 3.288 (9.94) | 5.425 (16.61) | 3.617 (11.00) | 1.11 |
| write: vruntime += 1, every task | 2.911 (8.79) | 6.786 (20.98) | 0.699 (2.12) | 0.24 |
| write: vruntime = x, every task | 4.361 (13.30) | 6.405 (19.60) | 0.448 (1.36) | 0.10 |

**4194304 tasks, gcc, -march=native**

| operation | C (AoS) | Fascon before (mirrored) | Fascon after (True SoA) | after / C |
|---|---:|---:|---:|---:|
| read: sum of vruntime (vruntime_scan) | 2.662 (8.25) | 0.283 (0.91) | 0.281 (0.87) | 0.11 |
| read: vruntime sum of runnable tasks | 3.243 (10.06) | 0.291 (0.89) | 0.278 (0.85) | 0.08 |
| read: count of runnable tasks | 3.412 (10.45) | 0.005 (0.02) | 0.005 (0.02) | 0.00 |
| read: pick_next (min vruntime, runnable) | 3.323 (10.27) | 0.276 (0.87) | 0.280 (0.86) | 0.08 |
| write: vruntime += 1, runnable tasks | 3.210 (9.96) | 5.444 (17.04) | 3.569 (11.01) | 1.11 |
| write: vruntime += 1, every task | 3.029 (9.39) | 6.389 (19.68) | 0.698 (2.16) | 0.23 |
| write: vruntime = x, every task | 4.236 (12.96) | 6.283 (19.10) | 0.445 (1.42) | 0.11 |

**4194304 tasks, clang, baseline (SSE2)**

| operation | C (AoS) | Fascon before (mirrored) | Fascon after (True SoA) | after / C |
|---|---:|---:|---:|---:|
| read: sum of vruntime (vruntime_scan) | 2.597 (8.05) | 0.272 (0.85) | 0.270 (0.83) | 0.10 |
| read: vruntime sum of runnable tasks | 2.838 (8.63) | 0.272 (0.84) | 0.322 (0.99) | 0.11 |
| read: count of runnable tasks | 2.805 (8.58) | 0.012 (0.04) | 0.012 (0.04) | 0.00 |
| read: pick_next (min vruntime, runnable) | 2.940 (8.95) | 0.348 (1.03) | 0.290 (0.93) | 0.10 |
| write: vruntime += 1, runnable tasks | 2.920 (8.93) | 5.111 (15.57) | 3.572 (10.92) | 1.22 |
| write: vruntime += 1, every task | 2.939 (9.00) | 6.147 (19.12) | 0.552 (1.74) | 0.19 |
| write: vruntime = x, every task | 4.359 (13.33) | 6.261 (18.99) | 0.447 (1.38) | 0.10 |

**4194304 tasks, clang, -march=native**

| operation | C (AoS) | Fascon before (mirrored) | Fascon after (True SoA) | after / C |
|---|---:|---:|---:|---:|
| read: sum of vruntime (vruntime_scan) | 1.890 (5.64) | 0.285 (0.90) | 0.280 (0.85) | 0.15 |
| read: vruntime sum of runnable tasks | 2.467 (7.67) | 0.285 (0.88) | 0.275 (0.86) | 0.11 |
| read: count of runnable tasks | 1.868 (5.83) | 0.003 (0.01) | 0.003 (0.01) | 0.00 |
| read: pick_next (min vruntime, runnable) | 2.697 (8.28) | 0.284 (0.88) | 0.264 (0.84) | 0.10 |
| write: vruntime += 1, runnable tasks | 2.802 (8.53) | 4.993 (15.39) | 3.557 (10.81) | 1.27 |
| write: vruntime += 1, every task | 2.969 (9.06) | 5.992 (18.74) | 0.521 (1.61) | 0.18 |
| write: vruntime = x, every task | 4.444 (13.61) | 6.277 (19.70) | 0.456 (1.38) | 0.10 |

- Reads stay under one cycle per task with 60,000 tasks in every build; with 4 million tasks the u64 column streams at about 27–30 GB/s (0.83–1.03 cycles) while C (AoS) takes 5.6–10.5 cycles. Bulk updates and overwrites are 0.44–0.86× C (60,000) and 0.10–0.24× C (4 million); before True SoA they were 3.1–3.5× and 1.4–2.3×. The write to runnable tasks only is 0.97–1.11× C with GCC and 1.22–1.90× with clang (whose C loop is unrolled and runs in 0.5–0.6× the GCC time). / 読み出しは 6 万タスクでどのビルドも 1 クロック未満、400 万タスクで約 27〜30 GB/s（0.83〜1.03 クロック、C は 5.6〜10.5）。一括更新・上書きは C の 0.44〜0.86 倍（6 万）・0.10〜0.24 倍（400 万）で、完全列化の前は 3.1〜3.5 倍・1.4〜2.3 倍でした。実行可能なタスクだけの書き込みは GCC で C の 0.97〜1.11 倍、clang で 1.22〜1.90 倍です（clang の C のループは展開され、GCC の 0.5〜0.6 倍の時間）。
- Non-temporal stores on the overwrite `vruntime = x` (same host, GCC unless noted): 480 KB column 0.445 ns with them vs 0.337 ns without (clang 0.664 / 0.680); 32 MiB column (fits in the 260 MiB L3) 0.445–0.450 vs 0.384–0.394 ns; 4 × 128 MiB columns (beyond the L3) 0.49 vs 1.13–1.14 ns. Software prefetch 256–2048 B ahead in the 4 million-task column scans gained nothing (plain sum 0.273 ns without it). / 上書き `vruntime = x` の非時間的ストア: 480 KB の列で 0.445 ns（なしは 0.337 ns、clang は 0.664 / 0.680）、32 MiB の列（260 MiB の L3 に収まる）で 0.445〜0.450 と 0.384〜0.394 ns、128 MiB の列 4 本（L3 を超える）で 0.49 と 1.13〜1.14 ns。400 万タスクの列の走査に 256〜2048 B 先のソフトウェアのプリフェッチを入れても速くなりませんでした（素の総和はなしで 0.273 ns）。
- This shared host has rounds where everything, C included, runs about 1.6× slower; medians over 9 rounds are given. / この共用のホストでは、C も含めてすべてが 1.6 倍ほど遅くなる回があります。9 回の中央値です。

## November 2026, columns (version unchanged) / 2026-11 の改修: 列（版は 1.0.0 のまま）

Phase 4 of [docs/PERF-1.0.md](../docs/PERF-1.0.md): every `for id in @a` reduction reads a column of just the values it reads. `--columns off` builds the same program with the tables and scans of phases 1–3. / [docs/PERF-1.0.ja.md](../docs/PERF-1.0.ja.md) のフェーズ 4: `for id in @a` の集計は、その集計が読む値だけの列を読みます。`--columns off` は同じプログラムをフェーズ 1〜3 の表と走査でビルドします。

```text
fascon build bench/sched_bench.fsc --out-dir bench/on                    # columns (default)
fascon build bench/sched_bench.fsc --out-dir bench/off --columns off     # phases 1-3 only
cc -O2 -std=c11 [-march=native] -Ibench/on  bench/harness.c -o bench/on/harness
cc -O2 -std=c11 [-march=native] -Ibench/off bench/harness.c -o bench/off/harness
```

Machine / 機械: Intel Xeon, KVM virtual machine, 2 vCPUs, Linux with transparent huge pages in `madvise` mode (the harness asks for 2 MiB pages); GCC 13.3 and clang 18.1 at `-O2`. The core clock, measured during the session with a chain of dependent 64-bit multiplies, was 3.85 GHz (3.80–3.94), so one cycle is 0.260 ns. Eight builds (columns off / on × GCC / clang × baseline SSE2 / `-march=native`, which is AVX-512 on this host) were run alternately, nine rounds; every number is the median of the nine rounds (each round is the harness's own median of 15 timings). ns per task, shorter is faster. "b" = baseline, "n" = `-march=native`. / 同じ回に依存のある 64 ビット乗算の鎖で測ったコアのクロックは 3.85 GHz（3.80〜3.94）で、1 クロックは 0.260 ns です。8 つのビルド（列 off / on × GCC / clang × 基本の SSE2 / `-march=native`。このホストでは AVX-512）を交互に 9 回走らせ、数字は 9 回の中央値（各回はハーネス自身の 15 回の計測の中央値）です。1 タスクあたりの ns、短いほど速い。「b」は基本、「n」は `-march=native`。

| Scenario / 状況 | off GCC b | on GCC b | off clang b | on clang b | off GCC n | on GCC n | off clang n | on clang n |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| C raw pointers, packed malloc, creation order / 生ポインタ・連続確保・作成順 | 1.96 | 2.34 | 1.90 | 1.78 | 1.88 | 1.83 | 1.78 | 2.24 |
| Fascon `get` chain, creation order / `get` の連鎖・作成順 | 2.50 | 2.61 | 3.01 | 2.98 | 2.52 | 2.50 | 2.78 | 2.85 |
| Fascon `@task[id]` chain, creation order / `@task[id]` の連鎖・作成順 | 2.34 | 2.42 | 3.04 | 3.01 | 2.41 | 2.31 | 2.74 | 2.67 |
| Fascon `for id in @task` scan (sum) / 表の順走査（総和） | 0.54 | 0.21 | 0.45 | 0.19 | 0.51 | 0.10 | 0.45 | 0.11 |
| Fascon count of runnable tasks (filtered count) / 実行可能なタスクの数（条件付きの個数） | 0.56 | 0.02 | 0.56 | 0.01 | 0.60 | 0.00 | 0.28 | 0.00 |
| Fascon vruntime of runnable tasks (filtered sum) / 実行可能なタスクの vruntime の和（条件付き総和） | 0.79 | 0.10 | 0.52 | 0.09 | 0.82 | 0.05 | 0.48 | 0.06 |
| Fascon pick_next (least vruntime among runnable) / pick_next（実行可能で vruntime 最小） | 0.60 | 0.21 | 0.63 | 0.18 | 0.61 | 0.10 | 0.49 | 0.07 |
| Fascon tick of runnable tasks (writes `files`, no column reads it) / tick（`files` を書く。どの列も読まない） | 0.87 | 0.89 | 0.84 | 0.77 | 0.94 | 0.90 | 0.76 | 0.75 |
| Fascon charge of runnable tasks (writes `vruntime`, three columns read it) / charge（`vruntime` を書く。3 本の列が読む） | 0.94 | 2.13 | 0.81 | 1.79 | 0.93 | 1.78 | 0.78 | 1.82 |
| Fascon scan, sparse table (10 % of the 64-slot blocks live) / まばらな表の走査 | 0.56 | 0.03 | 0.45 | 0.02 | 0.51 | 0.02 | 0.43 | 0.02 |
| Fascon count of runnable tasks, sparse table / まばらな表の個数 | 0.05 | 0.02 | 0.05 | 0.01 | 0.06 | 0.00 | 0.03 | 0.00 |
| Fascon pick_next, sparse table / まばらな表の pick_next | 0.06 | 0.03 | 0.07 | 0.02 | 0.06 | 0.02 | 0.05 | 0.01 |
| C raw pointers, packed malloc, scattered order / 生ポインタ・連続確保・ばらばらの順 | 31.40 | 45.72 | 29.65 | 28.99 | 30.00 | 30.06 | 28.69 | 67.45 |
| C raw pointers, fragmented malloc, scattered order / 生ポインタ・断片化した確保・ばらばらの順 | 41.90 | 43.93 | 42.42 | 41.92 | 44.00 | 43.71 | 41.36 | 88.13 |
| Fascon `get` chain, scattered order / `get` の連鎖・ばらばらの順 | 14.31 | 23.29 | 14.35 | 14.14 | 12.98 | 11.43 | 13.52 | 37.62 |
| Fascon `@task[id]` chain, scattered order / `@task[id]` の連鎖・ばらばらの順 | 21.41 | 12.53 | 13.17 | 14.14 | 12.89 | 11.10 | 13.41 | 28.22 |

What the numbers say / 数字から言えること:

- Scans with columns are under one cycle per 32-byte task in every build: the plain sum 0.10–0.21 ns (0.39–0.81 cycles), the filtered sum 0.05–0.10 ns, pick_next 0.07–0.21 ns, the filtered count 0.00–0.02 ns. Without columns the same scans take 0.28–0.82 ns (1.1–3.2 cycles) including the filtered count; the plain sum, the filtered sum and pick_next alone take 0.45–0.82 ns (1.7–3.2 cycles). / 列ありの走査は、どのビルドでも 32 バイトのタスク 1 つあたり 1 クロック未満です: 素の総和 0.10〜0.21 ns（0.39〜0.81 クロック）、条件付き総和 0.05〜0.10 ns、pick_next 0.07〜0.21 ns、条件付きの個数 0.00〜0.02 ns。列なしでは同じ走査が、条件付きの個数を含めて 0.28〜0.82 ns（1.1〜3.2 クロック）、素の総和・条件付き総和・pick_next だけなら 0.45〜0.82 ns（1.7〜3.2 クロック）です。
- A loop that writes a field no column reads (`tick`) is unchanged; a loop that writes `vruntime`, which three columns read, in every runnable task (`charge`) takes about twice as long. / どの列も読まないフィールドを書くループ（`tick`）は変わりません。3 本の列が読む `vruntime` を実行可能なタスクすべてで書くループ（`charge`）は約 2 倍の時間になります。
- The Id-chain and plain C rows do not involve columns; on this shared host they moved by up to ±50 % between rounds, in both builds alike. / Id の連鎖とふつうの C の行は列に関係しません。共用のこのホストでは回ごとに最大 ±50 % 揺れました（両方のビルドで同じように）。
- With 4,194,304 tasks (128 MB of slots; a separate test with `-march=native` and GCC, median of 9) the scans without columns take 1.9–2.9 ns per task and with columns 0.29 ns (sum, filtered sum, pick_next) and 0.004 ns (count): a u64 column then streams at memory bandwidth, about 28 GB/s here. / 4,194,304 タスク（スロットだけで 128 MB。`-march=native` の GCC で別に測った 9 回の中央値）では、列なしの走査が 1 タスク 1.9〜2.9 ns、列ありが 0.29 ns（総和・条件付き総和・pick_next）と 0.004 ns（個数）でした。このとき u64 の列はメモリの帯域（ここでは約 28 GB/s）で流れます。
- Not timed on AArch64 hardware. / AArch64 の実機では測っていません。

## November 2026 update (version unchanged) / 2026-11 の改修（版は 1.0.0 のまま）

The changes are described in [docs/PERF-1.0.md](../docs/PERF-1.0.md). / 変更の内容は [docs/PERF-1.0.ja.md](../docs/PERF-1.0.ja.md) にあります。

Machine / 機械: Intel Xeon @ 2.10 GHz, KVM virtual machine, 2 vCPUs, Linux 6.18 with transparent huge pages in `madvise` mode; GCC 13.3 and clang 18.1, `-O2 -std=c11`. Two sessions of 9 alternating runs (the 1.0.0 output and the new output, each with GCC and clang, the new one with and without huge pages); each number is the median of the 18 runs, each run the median of 15 timings. "1.0.0" is the C generated by the released 1.0.0 compiler from the same `sched_bench.fsc`, which never asks for huge pages. "4 KiB" runs the new output with `FASCON_NO_HUGEPAGE=1`; "2 MiB" lets the harness call `madvise(MADV_HUGEPAGE)`, and `/proc/<pid>/smaps` showed `AnonHugePages: 2048 kB` on the table. ns per task, shorter is faster.
2 回に分けて、それぞれ交互に 9 回走らせました（1.0.0 の出力と新しい出力を GCC と clang で。新しい出力は大きなページあり・なしの両方）。数字は合わせた 18 回の中央値（各回は 15 回の計測の中央値）。「1.0.0」は同じ `sched_bench.fsc` を出荷時の 1.0.0 でコンパイルした C で、大きなページは頼みません。「4 KiB」は新しい出力を `FASCON_NO_HUGEPAGE=1` で、「2 MiB」はハーネスに `madvise(MADV_HUGEPAGE)` を掛けさせて走らせたものです（`/proc/<pid>/smaps` で表に `AnonHugePages: 2048 kB` が付いていることを確認）。1 タスクあたりの ns、短いほど速い。

| Scenario / 状況 | 1.0.0 gcc | new gcc 4 KiB | new gcc 2 MiB | 1.0.0 clang | new clang 4 KiB | new clang 2 MiB |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| C raw pointers, packed malloc, creation order / 生ポインタ・連続確保・作成順 | 2.21 | 2.19 | 2.18 | 2.22 | 2.21 | 2.17 |
| Fascon `get` chain, creation order / `get` の連鎖・作成順 | 3.13 | 3.18 | 3.16 | 3.71 | 3.67 | 3.70 |
| Fascon `@task[id]` chain, creation order / `@task[id]` の連鎖・作成順 | 3.01 | 3.01 | 3.04 | 3.40 | 3.63 | 3.63 |
| Fascon `for id in @task` scan (sum) / 表の順走査（総和） | 0.74 | 0.72 | 0.63 | 0.71 | 0.69 | 0.57 |
| Fascon count of runnable tasks (filtered count) / 実行可能なタスクの数（条件付きの個数） | 1.10 | 0.79 | 0.71 | 1.10 | 0.80 | 0.70 |
| Fascon vruntime of runnable tasks (filtered sum) / 実行可能なタスクの vruntime の和（条件付き総和） | 1.08 | 1.09 | 1.04 | 1.12 | 0.74 | 0.66 |
| Fascon pick_next (least vruntime among runnable) / pick_next（実行可能で vruntime 最小） | 1.31 | 0.82 | 0.73 | 1.23 | 0.73 | 0.69 |
| Fascon tick of runnable tasks (general loop, writes) / 実行可能なタスクの tick（一般のループ・書き込み） | 1.12 | 1.19 | 1.15 | 1.12 | 1.12 | 1.10 |
| Fascon scan, sparse table (10 % of the 64-slot blocks live) / まばらな表（64 スロットの塊の 1 割が生存）の走査 | 0.73 | 0.73 | 0.62 | 0.71 | 0.69 | 0.56 |
| Fascon count of runnable tasks, sparse table / まばらな表の個数 | 0.49 | 0.07 | 0.07 | 0.48 | 0.07 | 0.07 |
| Fascon pick_next, sparse table / まばらな表の pick_next | 0.48 | 0.07 | 0.07 | 0.51 | 0.07 | 0.07 |
| C raw pointers, packed malloc, scattered order / 生ポインタ・連続確保・ばらばらの順 | 29.86 | 30.34 | 30.21 | 29.87 | 29.74 | 30.67 |
| C raw pointers, fragmented malloc, scattered order / 生ポインタ・断片化した確保・ばらばらの順 | 43.05 | 42.99 | 43.69 | 42.88 | 42.84 | 43.18 |
| Fascon `get` chain, scattered order / `get` の連鎖・ばらばらの順 | 17.43 | 16.95 | 14.23 | 22.91 | 23.21 | 15.93 |
| Fascon `@task[id]` chain, scattered order / `@task[id]` の連鎖・ばらばらの順 | 18.23 | 18.04 | 13.67 | 18.78 | 18.48 | 14.43 |

What the numbers say / 数字から言えること:

- The code alone (4 KiB columns): filtered counts 27–28 % faster, pick_next 37 % (GCC) / 41 % (clang), the filtered sum 34 % with clang and unchanged with GCC, sparse tables 7× (all-zero bitmap words are skipped). The plain sum and the general loop are unchanged with clang; with GCC the general loop is 3–6 % slower (in a dense table the per-slot `link[]` test of 1.0.0 was always predicted, and the bitmap adds ctz + blsr per slot). / コードの変更だけ（4 KiB の列）: 条件付きの個数 27〜28 %、pick_next 37 %（GCC）/ 41 %（clang）、条件付き総和は clang で 34 % 速く GCC では同じ、まばらな表は 7 倍（全部 0 のビットマップの語を飛ばす）。素の総和と一般のループは clang では変わらず、GCC の一般のループは 3〜6 % 遅い（密な表では 1.0.0 のスロットごとの `link[]` の判定がいつも予測どおりで、ビットマップはスロットごとに ctz と blsr が加わるため）。
- Huge pages on top (2 MiB against 4 KiB): the plain sum 13 % (GCC) / 17 % (clang) faster, the filtered scans 5–13 %, scattered Id chains 16–31 %. / 大きなページの上乗せ（2 MiB と 4 KiB の比較）: 素の総和 13 %（GCC）/ 17 %（clang）、条件付きの走査 5〜13 %、ばらばらの順の Id の連鎖 16〜31 %。
- The plain sum: 0.63 ns (GCC) and 0.57 ns (clang) are 1.32 and 1.20 cycles per task at 2.10 GHz, inside the target of 0.48–0.70 ns. Under one cycle (0.476 ns at this clock) was not reached: the scan has no branch left and reads 32 bytes per task (50–56 GB/s). / 素の総和: 0.63 ns（GCC）と 0.57 ns（clang）は 2.10 GHz で 1 タスク 1.32 / 1.20 クロックで、目標の 0.48〜0.70 ns に入っています。1 クロック未満（このクロックで 0.476 ns）には届いていません。この走査にはもう分岐がなく、1 タスク 32 バイト（50〜56 GB/s）を読む帯域が残っています。
- With `-DFASCON2_PICK_BRANCHLESS` (no data-dependent branch at all) pick_next takes 1.53 ns (clang) / 2.57 ns (GCC), median of 7 runs. / `-DFASCON2_PICK_BRANCHLESS`（データに依存する分岐が一本もない形）では、pick_next は 1.53 ns（clang）/ 2.57 ns（GCC）（7 回の中央値）。

Honest caveats / 正直な注意:

- Huge pages depend on the system. In an earlier session the same day the 4 KiB and 2 MiB columns came out equal, most likely because no huge page backed the table then; check `AnonHugePages` in `/proc/<pid>/smaps`. Only whole 2 MiB-aligned ranges become huge pages, so of this 2.4 MiB table the last 0.4 MiB stays on 4 KiB pages. / 大きなページは環境しだいです。同じ日のもっと早い回では 4 KiB と 2 MiB の列が同じになり、表に大きなページが付いていなかったと考えられます。確かめるには `/proc/<pid>/smaps` の `AnonHugePages` を見ます。大きなページにできるのは 2 MiB 境界の 2 MiB の範囲だけなので、この 2.4 MiB の表の最後の 0.4 MiB は 4 KiB ページのままです。
- A different machine from the 1.0.0 release measurements below (2.10 GHz here, 2.3 GHz there), so the two tables cannot be compared row by row. / 下の 1.0.0 出荷時の測定とは別の機械（こちらは 2.10 GHz、あちらは 2.3 GHz）なので、2 つの表を行ごとに比べることはできません。
- AArch64 was not timed on hardware; QEMU timings are meaningless. / AArch64 は実機で測っていません（QEMU の時間は意味がありません）。

## 1.0.0 release / 1.0.0 出荷時

Machine / 機械: Intel Xeon 6 (Granite Rapids) 2.3 GHz, KVM virtual machine, 2 cores, L2 2 MiB per core. The 1.0 preview (the build before the release) and 1.0.0 were measured in the same session, alternating runs (`fascon build` of each version, then the same harness); each number is the median of 5 alternating runs, each run the median of 15 timings. ns per task, shorter is faster.
1.0 のプレビュー（出荷前の版）と 1.0.0 を同じ回に交互に走らせて測りました（それぞれの版で `fascon build` し、同じハーネスで実行）。数字は交互に 5 回走らせた中央値（各回は 15 回の計測の中央値）。1 タスクあたりの ns、短いほど速い。

| Scenario / 状況 | 1.0 preview gcc | 1.0.0 gcc | 1.0 preview clang | 1.0.0 clang |
| --- | ---: | ---: | ---: | ---: |
| C raw pointers, packed malloc, creation order / 生ポインタ・連続確保・作成順 | 2.98 | 2.86 | 2.84 | 2.86 |
| Fascon `get` chain, creation order / `get` の連鎖・作成順 | 4.25 | 3.82 | 5.33 | 5.35 |
| Fascon `@task[id]` chain (latched in 1.0.0), creation order / `@task[id]` の連鎖・作成順 | 4.31 | 3.70 | 5.27 | 5.45 |
| Fascon `for id in @task` scan / 表の順走査 | 1.38 | 0.94 | 1.25 | 0.91 |
| C raw pointers, packed malloc, scattered order / 生ポインタ・連続確保・ばらばらの順 | 55.89 | 55.61 | 60.12 | 55.33 |
| C raw pointers, fragmented malloc, scattered order / 生ポインタ・断片化した確保・ばらばらの順 | 78.11 | 78.58 | 76.84 | 76.22 |
| Fascon `get` chain, scattered order / `get` の連鎖・ばらばらの順 | 44.98 | 28.95 | 39.97 | 44.87 |
| Fascon `@task[id]` chain, scattered order / `@task[id]` の連鎖・ばらばらの順 | 35.93 | 33.52 | 41.70 | 28.57 |

What changed in 1.0.0 / 1.0.0 で変わったこと: 32-bit Ids with K = 16 for this arena (16-bit generations stored in `uint16_t`), tables on a 64-byte line, branch-free latched `@task[id]`, and the sum specialization of `for id in @task`. / この表は K = 16（世代 16 ビットを `uint16_t` に）、表はキャッシュライン境界、`@task[id]` は分岐なしのラッチ、`for id in @task` は総和の特殊化。

Honest caveats / 正直な注意:

- One machine, one virtual machine, and it drifts: in an earlier, quieter session the same 1.0.0 scan measured 0.48–0.70 ns per task. 0.9 ns is about 2 cycles at 2.3 GHz, so "under one cycle per task" was not reached here. / 1 台の仮想マシンでの結果で、時間帯でも揺れます。もっと静かだった回では、同じ 1.0.0 の走査が 0.48〜0.70 ns でした。0.9 ns は 2.3 GHz で約 2 サイクルなので、ここでは「1 タスク 1 クロック未満」には届いていません。
- Scattered rows move by up to ±40 % between runs. The working set (1.9 MB of slots plus 128 KiB of generations) is the size of L2, so a threshold effect is part of every scattered number. / ばらばらの行は回ごとに最大 ±40 % ほど揺れます。作業領域（スロット 1.9 MB と世代 128 KiB）が L2 の大きさちょうどなので、境目の効果がばらばらの数字すべてに含まれます。
- The difference comes from density (32-byte packed slots against 48-byte or larger malloc blocks) and layout (the table can be scanned in order), not from indices as such. With a pointer list allocated contiguously and linked in creation order, raw pointers still win. / 差は添字そのものではなく密度と配置から来ます。作成順に連続確保したポインタのリストでは、生ポインタの方が速いままです。

## VRAM loop / VRAM のループ

`vram_raw.c` is the wallpaper loop of `examples/gui_desktop.fsc` written by hand with a raw pointer. Compiled with `clang --target=aarch64-none-elf -mcpu=cortex-a72 -O2`, its inner loop and the inner loop of the Fascon-generated `fsc_draw_wallpaper` are the same 13 instructions (10 × `stp q0, q0`, 80 pixels per iteration); the only differences are branch addresses and the position of alignment nops.
`vram_raw.c` は `examples/gui_desktop.fsc` の壁紙ループを生ポインタで手書きしたものです。上のオプションでコンパイルすると、Fascon が生成した `fsc_draw_wallpaper` と内側のループが同じ 13 命令（`stp q0, q0` × 10、1 周 80 画素）になり、違いは飛び先の番地と整列用 nop の位置だけです。
