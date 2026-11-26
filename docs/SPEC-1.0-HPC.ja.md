# Fascon 1.0 — 浮動小数点の完全列化・条件付き一括更新・GPU への貸し出し・HPC のプロファイル

版の名前は **Fascon 1.0（1.0.0）のまま**です。この文書は、HPC・スパコンの計算ノード・多チャネルメモリのワークステーション向けの 4 つの追加と、それぞれが Fascon 1.0 の不変条件をどう守るかを説明します。英語版は `SPEC-1.0-HPC.md` です。

| 追加 | ソースでの形 | 節 |
| --- | --- | --- |
| f32 / f64 と浮動小数点の完全列化 | `let x: f32 = 1.5;`、`#[fast_math]`、`--fast-math` | §1 |
| 条件付き一括更新（Masked Column RMW） | `for id in @a { if 条件 { @a[id].f += x; } }` | §2 |
| GPU・アクセラレータへの貸し出し | `gpu_arena @g: T[N];`、`GpuBuf<@g>`、`GpuFence<@g>` | §3 |
| HPC のプロファイル | `--target hpc`、`--topology`、`#[numa_node(n)]`、`#[no_smt]`、`#[allow(hpc_io)]` | §4 |

## 0. 不変条件（品質ゲート）

| ゲート | 確かめ方 |
| --- | --- |
| 既存のプログラムの生成 C は 1 バイトも変わらない（注釈と空白を除く） | `python3 tools/compare_c.py`: `examples/` と `tests/` のすべてのプログラムを、既定・`--columns mirrored`・`--columns off` の 3 つのモードで、`tests/golden_c.json`（追加の前のビルドで記録）と比べる |
| ライフタイム・ジェネリクス・生ポインタ・`unsafe`・0.1 専用の構文を足さない。新しいキーワードは文脈依存 | `gpu_arena`・`GpuBuf`・`GpuFence`・`f32`・`f64` は文脈依存（同じ名前の構造体・関数はそのまま使える）。属性はふつうの `#[..]` |
| 外部クレートゼロ | `Cargo.toml` の依存は無いまま |
| 新しい番号は辞書・規則・修正に | F2705〜F2707 と F3901〜F3903 を `data/dictionary.json`（日英）・`data/rules.json`・`data/fixes.json` に。`python3 tools/sync_dictionary.py && python3 tools/check_dictionary.py` |
| 生成 C はプラットフォームのフックだけを呼ぶ | 生成 C に CUDA・Vulkan・libnuma・pthread・インラインアセンブリを書かない。貸し出しとピン留めは `fascon_plat_*` を通す（§3.3、§4.3）。`tests/run_tests.py` が生成 C を検査する |
| パッケージの安全性ラベル | `src/pkg.rs` の `scan_text` が `gpu_arena`・`GpuBuf`・`GpuFence`・`.dispatch(..)` を "GPU / Accelerator offload" に分類する |
| 厳格形の往復 | `--emit-strict-fascon` は `gpu_arena`・`#[fast_math]`・`#[numa_node(n)]`・`#[no_smt]`・`#[allow(hpc_io)]`・`// fascon-target: hpc`・`// fascon-topology:` を残し、厳格形をコンパイルし直すと同じ C になる |
| オプトアウト | `--columns off`（FASCON_COLUMNS=off）・`--gpu=off`（FASCON_GPU=off）・`FASCON_HPC_NO_PIN=1` |

## 1. f32 / f64 と浮動小数点の完全列化

### 1.1 言語の浮動小数点数

`f32` と `f64` は IEEE-754 の単精度・倍精度です。リテラルは `1.5`・`2.0f32`・`1e-3`・`3f64`、接尾辞の無いリテラルは推論で型が決まります（決まらなければ f64）。演算は書いた順の IEEE-754 そのままで、生成 C は FMA への縮約を止めます（`#pragma STDC FP_CONTRACT OFF` と GCC / clang の同等のもの）。`a * b + c` はどのコンパイラでも 2 回丸めます。`x as i32` は飽和（NaN は 0。Rust と同じ）、`x as f64` は正確に広げます。`put(x)` は同じビットに読み戻せる最短の 10 進を Rust の `{:?}` の形（`1.0`・`0.1`・`1e-7`・`inf`・`NaN`）で出し、ちょうど中間なら偶数へ丸めます（乱数のビットの並み 40 万個で確かめた）。

メソッドは `to_bits()`・`is_nan()`・`is_finite()`・`abs()`、`f32::from_bits(u)`・`f64::from_bits(u)`、定数は `f64::INFINITY`・`f64::NEG_INFINITY`・`f64::NAN`・`f64::EPSILON`・`f64::MIN`・`f64::MAX`・`f64::MIN_POSITIVE`（f32 も同じ）です。

### 1.2 列と走査関数（`#[fast_math]` のときだけ）

浮動小数点の加算は結合則を満たさないので、ループかその関数に `#[fast_math]` が無い（`--fast-math` でもない）浮動小数点の集計は、フェーズ 1 の書いた順の走査のままです。付いていれば:

| 集計（`#[fast_math]` の下） | 列 | 死んだスロット | 走査関数 |
| --- | --- | --- | --- |
| 総和 `s += @a[id].f`・条件付き総和 | 完全列化の `float`（4 B）/ `double`（8 B） | `0.0` | `f20_col_sum_f32` / `_f64`（4 本の独立したベクトル累計。条件付きは条件のビット列と `_m`） |
| 最小 / 最大 `if @a[id].f < best { .. }` | 鍵の列（`int64_t`） | `F20_KEY_NONE` | `f20_col_pick_key`（整数と同じもの） |

**IEEE-754 の順序を保つ鍵**（`F20_KEY_F32` / `F20_KEY_F64`）: 符号ビットが 0 の値は符号ビットだけ、1 の値は全ビットを反転すると、符号付き整数として浮動小数点数と同じ順に並びます。NaN は `F20_KEY_NONE`（死んだスロットと同じく選ばれない）、`-0.0` は先に `+0.0` へそろえます。累計の初めの値が NaN なら、書いた順のループと同じく何も選びません。

8 MiB 以上の浮動小数点の完全列化の列への一括上書きは、整数の列と同じく `F20_NT_STORE_F32` / `_F64` と出口の `F20_SFENCE()` を使います。`#[no_column]` とほかの完全列化の除外はそのまま効きます。

## 2. 条件付き一括更新（Masked Column RMW）

`for id in @a { if 条件 { @a[id].f += x; } }` と `.. { @a[id].f = x; }` は、次のときベクトル幅の直線の更新になります。

- `f` が 1 段の完全列化のフィールドで、型が u32・u64・i32・i64、または `#[fast_math]` の下の f32 / f64
- `条件` が条件のビット列（`fbN`、Mirrored-SoA）にできる
- `x` がループの中で変わらず、失敗もしない（リテラル・符号を反転したリテラル・ループ変数でないローカル・static・定数、`as` を付けたもの）
- アリーナがだれとも共有しない表（shared でなく、割り込み・タスクが触らず、dma / gpu / gc / bump / task でない）

走査関数 `f20_col_masked_add_{u32,u64,i32,i64,f32,f64}` と `f20_col_masked_set_*` は、条件のビット列を 64 ビットの語（64 スロット）ずつ読みます。

| 語 | 動き |
| --- | --- |
| `0` | 飛ばす: その 64 スロットの列の行は読みも書きもしない |
| `~0` | マスクなしのベクトルの加算・代入。8 MiB 以上の列で `--nontemporal auto|on` なら `F20_NT_STORE`（先にあふれを確かめてから流し込む） |
| それ以外 | 語をレーンへ広げ（全レーンへ配って 1 << レーン番号と AND し 0 と比べる。SSE2 には 64 ビットの比較がないので、64 ビットのレーンも 32 ビットのレーンの比較で作る）、述語付きの演算でその場で更新する。キャッシュを汚さないよう非時間的ストアは使わない |

整数の加算は Fascon の検査付きの算術のままです。どのレーンも最上位ビットに、符号なしなら繰り上がり `(v & x) | ((v | x) & ~r)`、符号付きならあふれ `(v ^ r) & (x ^ r)` を作ります。あふれるレーンのある語は正確に元に戻し（書いた値は 2^n を法として回っているので、`x` を引けば元に戻る）、その語の先頭の添字を返します。呼び出し側はその語から逐次のループで続け、`--columns off` と同じスロットで同じ出力のまま panic します（`FASCON PANIC 0x000b add overflow`）。浮動小数点の加算は選ばれたレーンにだけ `v + x` を入れ、ほかのレーンはビットをそのまま残します（`-0.0` も NaN の中身も）。`fascon regions` に、ループごとの列・条件のビット列・走査関数・ストアの種類が出ます。

## 3. `gpu_arena`: アクセラレータへのゼロコピーの貸し出し

### 3.1 構文と型

```fascon
struct Particle { x: f32, v: f32, steps: u32, tag: u32 }
gpu_arena @parts: Particle[64];            // 要素は POD（F2705）

let h: GpuBuf<@parts> = @parts.alloc(Particle { x: 0.0, v: 0.5, steps: 0, tag: 1 }).unwrap();
@parts[h].v = 0.75;                        // h を持っている間は CPU がスロットに触れる
let f: GpuFence<@parts> = @parts.dispatch(STEP, grid, block, h);
// h は移動済み: ここで読み書き・解放すると F0382
if let Some(back) = @parts.reclaim(f) {    // 待たない。終わるまでは None
    put(@parts[back].x);
    @parts.free(back);
}
let w = @parts.reclaim_wait(f);            // 待つ。Sleep 効果
```

- `alloc(v) -> Option<GpuBuf<@g>>`、`free(h) -> bool`（h は消える）、`dispatch(kernel: u32, grid: u64, block: u64, h) -> GpuFence<@g>`（h は消える）、`reclaim(f)` / `reclaim_wait(f) -> Option<GpuBuf<@g>>`、`len()`、`capacity()`、`h.index()`。
- `GpuBuf` は移動専用（`Dma` と同じ）、`GpuFence` はコピーできる引換券 `[番号 32 | 添字 32]`。引換券がハンドルを返すのは 1 回だけで、同じ引換券の 2 度目・古い引換券は None。ハンドルが 2 つになることはありません。
- 表は `.bss.fascon_gpu` の 2 MiB 境界（PE / Mach-O は 4 KiB）に置かれ、全コアで 1 つの実体で、列にはしません。
- `reclaim_wait` は Sleep 効果を持ちます。`critical`・並列走査・割り込みハンドラから届くコードでは F2603、`rcu_read` / `atomic` の中では F3203。

### 3.2 エラー

| 番号 | いつ |
| --- | --- |
| F2705 | 要素に own・Id・Str・参照・関数ポインタ・bool・Option がある（整数・浮動小数点数と、その配列・構造体だけ） |
| F2706 | 別の gpu_arena のハンドル・引換券、引換券の代わりの整数やハンドル、gpu_arena でない表の `GpuBuf<@x>`、`for id in @g`、Id の操作（get・contains・validate など） |
| F2707（警告） | gpu_arena のあるプログラムを `--gpu=off` でコンパイルした |
| F0382 | `dispatch` や `free` のあとにハンドルを使った |

### 3.3 プラットフォームの API

生成 C が呼ぶのは次のフックだけです（ランタイムの包み `f20_gpu_*` を通す）。

```c
int  fascon_plat_gpu_register(void *base, size_t bytes, uint32_t arena_id);   /* 0 = 成功。スロットの配列をピン留め・写像 */
int  fascon_plat_gpu_dispatch(uint32_t arena_id, uint32_t slot_idx, uint32_t kernel_id,
                              uint64_t grid, uint64_t block, uint64_t *out_fence);  /* 0 = 積んだ */
int  fascon_plat_gpu_poll(uint64_t fence);                                       /* 終わっていれば 0 以外 */
void fascon_plat_gpu_wait(uint64_t fence);
```

生成 C は記述子の表 `fascon2_gpu_arenas[]`（`name`・`base` = スロット 0・`bytes`・`stride`・`elem_size`）も出します。スロット i の要素は `base + i * stride` にあります。ボードのパッケージ（`boards/<名前>/`）や別のバックエンドは、コンパイラを変えずにフックを差し替えられます。フックを持たないベアメタル（`FASCON2_BOARD_GPU` なし）では、dispatch はその場で走ってすぐ完了します。

### 3.4 PC での真似とカーネル

`bsp/host/fascon2_plat_host.c` は dispatch を FIFO に積み、1 本のワーカースレッドが順に走らせます。カーネルはソースの隣の `<名前>.gpu.c` の C の関数で、`fascon run` が一緒にビルドします。

```c
#include "fascon2_rt.h"
typedef struct { float x, v; uint32_t steps, tag; } Particle;   /* Fascon の構造体の C の形 */
FASCON_GPU_KERNEL(1) {
    Particle *p = (Particle *)elem;
    p->x += p->v * (float)(grid * block);
}
```

`FASCON_GPU_KERNEL(番号)` は関数を定義し、`main` の前に登録します（登録簿は生成 C の `fascon_gpu_kernel_register` / `fascon_gpu_kernel_lookup`）。引換券は 1 から増える番号で、ワーカーは終えた数を release で書き、`poll` は acquire で読むので、回収した CPU からカーネルの書いた値が見えます。終了時（ふつうの終了も panic も）は、積んだ仕事を走らせ終えてから終わります。`tests/run_tests.py` は `tests/ok/gpu_offload.fsc` を ASan + UBSan と TSan で走らせます（メモリの誤り・データ競合なし、`--gpu=off` と同じ出力）。

### 3.5 `--gpu=off`

`--gpu=off`（FASCON_GPU=off）は `FASCON2_GPU_OFF` を定義します。dispatch は登録されたカーネルを呼んだコアでその場で走らせ（登録が無ければ何もしない）、完了済みの引換券を返します。`fascon_plat_gpu_*` は呼ばず、スレッドのライブラリも要りません。コンパイラは gpu_arena ごとに 1 回警告します（F2707）。

## 4. `--target hpc`

### 4.1 プロファイル

`--target hpc` か `// fascon-target: hpc`（先頭 20 行の中）。既定のプロファイルとの違い:

- Id は 64 ビット `[世代 32 | 添字 32]`（generic-os と同じ `FASCON2_ID64`）。`fascon regions` もそう説明します。
- どの表も大きさによらず `.bss.fascon_arena` の 2 MiB 境界に置き、PC の実行層はすべてに `MADV_HUGEPAGE` を掛けます。
- `FASCON2_MAX_CPUS` は計算ノードの形の論理コアの数。コアの文脈 `fascon__cpu[core]` と既定の own ヒープは、それぞれ自分だけのページに置きます。
- main とすべての spawn_on の入口で、コアごとに 1 回、生成 C が `f20_hpc_core_enter()` を呼びます: `fascon_plat_pin_core(core)` のあと、そのコアのコアごとのアリーナ（TLS の実体）・`fascon__cpu[core]`・既定の own ヒープを `fascon_plat_numa_bind` で `fascon_plat_numa_node(core)` へ固定します（MPOL_BIND）。`#[numa_node(n)]` を付けた共有の表は、main の入口でコア 0 がノード n へ固定します。

### 4.2 計算ノードの形

`--topology cores=N,smt=K,numa=M[,layout=adjacent|interleave]` か、ファイルの `// fascon-topology: cores=N smt=K numa=M`。既定は `cores=128 smt=2 numa=8`。論理コア c の物理コアは `c mod (cores / smt)`（interleave。Linux / x86 の番号の振り方で、兄弟は c と c + 物理コア数）か `c / smt`（adjacent）、NUMA ノードは物理コアを順に等分します。厳格形はこの指定を書き出すので、同じ C になります。

### 4.3 プラットフォームの API

```c
typedef struct { uint32_t cpus, cores, smt, numa_nodes; } FasconTopology;
void     fascon_plat_topology(FasconTopology *t);
void     fascon_plat_pin_core(uint32_t core);
uint32_t fascon_plat_numa_node(uint32_t core);
int      fascon_plat_numa_bind(void *base, size_t bytes, uint32_t node);   /* 0 = 成功 */
```

Linux の PC の実行層は、使ってよい論理 CPU（`sched_getaffinity`。taskset・cgroup を尊重）を `/sys` の topology で物理コアごとにまとめます。コア k は、k が物理コアの数より小さければ k 番目の物理コアの最初のスレッドへ（SMT の兄弟を使わない）、それ以上なら兄弟スレッドへ回ります。NUMA ノードは `/sys/devices/system/cpu/cpuN/nodeM`、固定は `mbind` のシステムコール（MPOL_BIND・MPOL_MF_MOVE。libnuma は使わない）。どれも助言で、拒まれても（コンテナ・NUMA の無いカーネル・機械に無いノード）プログラムは止まりません。`FASCON_HPC_NO_PIN=1` で止められます。Windows はピン留めだけ、macOS は何もしません。`FASCON2_BOARD_HPC` の無いベアメタルでは何もしない版が入ります。

### 4.4 検査: 管理コアと計算コア

コア 0 は `main` を走らせて全体を管理し、`spawn_on(k, ..)`（k > 0、または実行時に決まるコア）で起動した関数が計算コアのルートです。届くかどうかは静的な呼び出しグラフでたどります。

| 番号 | 重さ | いつ |
| --- | --- | --- |
| F3901 | 警告 | MMIO・I/O ポート・dma のリージョンとアリーナ・`bind_mmio`・`submit_dma` / `complete_dma`・gpu の `dispatch` / `reclaim` が計算コアのルートから届く。その関数かルートに `#[allow(hpc_io)]` を付けると外れる |
| F3902 | エラー | 計算コアから届く道で動的に確保する（`own`・`own_alloc`・gc_arena の確保・`promote_to_gc`・`gc_collect`・`dma_alloc`）、またはその道の `for id in @a` が Mirrored-SoA の列（条件のビット列・鍵の列）の読むフィールドへ書き、写しを表ごと作り直す（本体から呼ぶ関数の書き込みも含む: 呼ぶたびに要素の写しを作り直すので表ごとと同じ）。後者はどの列が写しになるかを知る C 生成のあとで調べる |
| F3903 | エラー | spawn_on の定数のコアが `cores` 以上、`#[numa_node(n)]` が `numa` 以上、または `fn main` に `#[no_smt]` があるのに 2 つのコア（コア 0 を含む）が同じ物理コアの兄弟スレッドになる |

固定の表のスロットの確保（`@a.alloc(..)`）は動的な確保ではありません。このプロファイルでは表は静的で、大きくなりません。

### 4.5 `fascon regions`

```text
  --target hpc: 計算ノード cores=8 smt=2 numa=2 (// fascon-topology:)、#[no_smt]
    core 0   管理コア: main — 物理コア 0, NUMA ノード 0 (main の入口でピン留め)
    core 1   計算コアのルート: `worker` (行 72) — 物理コア 1, NUMA ノード 0 (spawn_on の入口でピン留めし、...)
    core 2   計算コアのルート: `worker` (行 73) — 物理コア 2, NUMA ノード 1 (...)
    #[numa_node(1)] totals → NUMA ノード 1 (main の入口でコア 0 が固定)
    計算コアから届く関数: worker, beacon
    F3901 計算コアから届く I/O: I/O ポート `post` — `beacon` (行 66)（#[allow(hpc_io)] で外した）
```

## 5. 自動修正（`data/fixes.json`）

| 番号 | 修正 |
| --- | --- |
| F2705・F2707 | 表をふつうの `arena` にする（CPU に置く） |
| F2706 | ふつうの表のハンドルなら `GpuBuf<a>` → `Id<a>` |
| F3901 | 囲む関数の前に `#[allow(hpc_io)]` を差し込む（新しい修正の種類 `insert_before_enclosing_fn`） |
| F3902 | 仕事を管理コアで走らせる: ルートの `spawn_on(c, f, a)` → `f(a)`（新しい修正の種類 `replace_in_note`） |
| F3903 | `spawn_on(c, ..)` → `spawn_on(1, ..)`、`#[numa_node(n)]` → `#[numa_node(0)]` |

修正はコンパイルし直して検証します。言語サーバーは警告 F3901・F2707 の修正も出します。

## 6. テスト

- `tests/ok/tsoa_float.fsc`・`tsoa_float_nt.fsc`・`tsoa_rmw.fsc`: `--columns` × `--nontemporal` のどの組み合わせでも同じ出力。C の形と `fascon regions` の検査。
- `tests/cols/rmw_kernels.c`: 条件付き一括更新の走査関数をスカラの模型と比べる（0・全部 1・ばらばらの語、端数、-0.0、NaN、あふれる語）。-O2 と ASan / UBSan。`tests/cols/rmw_overflow.fsc`: あふれの panic が `--columns on` と `off` で同じ。
- `tests/float/fmtfuzz.c` + `fmtcheck.py`: 浮動小数点の出力を、ビットの並み 40 万個で Python と比べる。
- `tests/cols/colfuzz.py --float`（NaN・-0.0・inf、`#[fast_math]` と書いた順の比較）と `--rmw`（6 種類の列への条件付きの += / = を確保・解放と混ぜる）。
- `tests/ok/gpu_offload.fsc` + `gpu_offload.gpu.c`: ワーカースレッドの真似・`--gpu=off`・ASan + UBSan・TSan。`tests/err/f0382_gpu_use_after_dispatch.fsc`・`f2705_*`・`f2706_*`・`f2603_gpu_wait_in_critical.fsc`・`f2603_gpu_wait_in_irq.fsc`・`f3203_gpu_wait_in_rcu.fsc`。
- `tests/ok/hpc_compute.fsc`（`--target hpc` の C と regions の報告）、`tests/hpc/io_noise.fsc`（F3901）、`tests/err/f3902_hpc_alloc.fsc`・`f3902_hpc_mirror.fsc`・`f3902_hpc_mirror_callee.fsc`・`f3903_hpc_core.fsc`・`f3903_hpc_numa.fsc`・`f3903_hpc_smt.fsc`。
- `bench/fsimd_bench.sh`: 浮動小数点の総和と条件付きの更新を、`-O2` と `-ffast-math` の C（AoS）と比べる（bench/RESULTS.md）。
