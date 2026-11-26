# Fascon 1.0 仕様: マルチコア（SMP）拡張

対象: Fascon 1.0 のコンパイラ・ランタイム・AArch64 の BSP・PC 実行。配布物のバージョン表記は 1.0.0 のままで、この文書が
「Fascon 1.0 の SMP 拡張」として追加分をまとめます。1 コアのプログラムが出す C は、この拡張の前と 1 文字も変わりません
（`tools/compare_c.py` で元の 1.0.0 と比べて 24 本すべて一致）。

## 1. 考え方

1 コアのときに速かった道（分岐なしラッチ・番兵スロット・`for id in a` の走査特殊化・列）には、ロックもアトミック命令も
入れません。そのために、データを 3 種類に分けます。

| 種類 | 書き方 | 実体 | 触り方 |
| --- | --- | --- | --- |
| コアごと | `arena a: T[N];`（これまでどおり） | コアごとに 1 つ（TLS） | 1 コアのときと同じ C。ロックなし |
| 共有 | `shared arena g: T[N];` | 全コアで 1 つ | `critical { }`（書き手）か `for id in g.par()`（読み手）の中だけ |
| 受け渡し | `chan ch: own T[CAP];` | ロックフリーのリングバッファ | `send`（移動）/ `recv` / `recv_wait` |

`spawn_on` を使わないプログラムは、これまでどおり 1 コアのプログラムです（`shared arena` も `chan` も 1 コアで動きます）。

## 2. 構文

```
shared arena procs: Proc[64];          // 全コア共有
chan work: own TaskPacket[8];          // 容量は 2 の累乗（2〜65536）

spawn_on(1, worker);                   // コア 1 で fn worker() を走らせる
spawn_on(2, worker, arg);              // 引数は 1 個まで: 整数・bool・共有アリーナの Id・own T

work.send(p);                          // p は移動する（以後の使用は F0382）。満杯なら空くまで待つ
match work.recv() { Some(p) => .., None => .. }   // 待たない
let p = work.recv_wait();              // 届くまで待つ

critical { procs[id].state = 2; }      // 書き手の区間（割り込み禁止 + 書き手のロック）
for id in procs.par() { .. }           // 読み手の区間: ほかのコアの par() と同時に進む

let n = cpu_count();                   // 動かせるコアの数
```

`shared`・`chan`・`spawn_on`・`par`・`cpu_count` は予約語ではありません（これらを名前に使っている既存のソースはそのまま通ります）。
0.1 専用構文の `spawn` / `chan::` とは別のもので、0.1 の構文は引き続き導入していません。

## 3. 規則とエラー

F2601〜F2605 は Step 1（割り込みとタスク）で使っているので、SMP の番号は F2611 からにしました。

| 番号 | 内容 |
| --- | --- |
| **F2611** | コアごとのアリーナの Id を、チャネルの要素・共有アリーナの要素・`spawn_on` の引数に（構造体や own の中まで調べて）含めた。ほかのコアでは別の表を指してしまう |
| **F2612** | 並列走査 `par()` の中に、共有アリーナへの書き込み・確保・解放、それを行う関数の呼び出し（効果 E(f) の W・F・確保に共有アリーナがある）、`critical` がある。または `par()` をコアごとのアリーナに使った |
| **F2613** | 2 本以上のコアのルートから届く MMIO・ポート・`mmio_read` / `mmio_write` へのアクセスが、`critical` の外にある |
| ~~F2614~~ | **廃止**（2026-11 の統合改修で解除）。以前は `spawn_on` を使うプログラムで irq・task_arena・dma_arena・syscall・trap を拒んでいた。[SPEC-1.0-INTEGRATION.ja.md](SPEC-1.0-INTEGRATION.ja.md) |
| **F2616** | 共有の周辺の割り込み（INTID 32 以上）のハンドラが、コアごとのアリーナに触った（統合改修で追加） |
| **F2615** | `spawn_on` の関数の形（引数 0〜1 個・戻り値なし）、引数の型、チャネルの容量・要素の型の誤り |
| F2601（既存） | 共有アリーナへのアクセスが、`critical` / `par()` の外にある（2 本以上のコアのルートから届くとき） |
| F2603（既存） | `critical` / `par()` の中で、待つチャネル操作（`send` / `recv_wait`）を使った |
| F0382（既存） | `send` した値・`spawn_on` に渡した own を、その後で使った |
| F0515（既存） | チャネルの要素・`spawn_on` の引数に参照（と関数ポインタ）を使った |
| F2803（既存） | ユーザープログラム（EL0）で `shared arena`・`chan`・`spawn_on` を使った |
| パニック 0x0012 | `spawn_on` のコアが無い、またはそのコアが前の関数をまだ走らせている |

コアのルートは、`main`（最初の `spawn_on` より後の部分）と、`spawn_on` で起動される関数です。Step 1 の実行ルートの解析
（`src/context.rs`）にそのまま載せてあります。最初の `spawn_on` より前の `main` は 1 コアだけなので、共有アリーナも装置も保護なしで
触れます。

## 4. 依頼の仕様と違う点（そのまま読んでください）

- **エラー番号**: 依頼は F2601・F2602・F2603 でしたが、Step 1 で使用済みのため F2611・F2612・F2613 にしました。テストの
  ファイル名も `tests/err/f2611_percpu_id_send.fsc`・`tests/err/f2612_par_scan_write.fsc` です。
- **`critical_section` は足していません**。既存の `critical { }` / `critical fn` が、SMP のプログラムではロックになります。
- **共有アリーナは、世代タグのアトミック操作やアリーナごとの細粒度ロックではなく、プログラム全体で 1 つの読み書きロック**で
  守ります（`critical` = 書き手、`par()` = 読み手）。確保・解放・要素の読み書きを同じロックで守らないと、世代の検査と要素の
  書き換えの間に競合が残るためです。共有アリーナの中のコードは 1 コアのときと同じ（ラッチもそのまま）で、アトミック命令は
  ロックの出入りにだけあります。アリーナごとにロックを分ける最適化はしていません。
- **`par()` は、1 つのループを自動でコアに分配するものではありません**。「読み手として入る走査」で、複数のコアがそれぞれの
  `par()` を同時に進められる、という意味です。集計はローカル変数に取り、ループの後で `critical` の中から書きます。
- **（2026-11 に解除）** 以前は `spawn_on` を使うプログラムで irq・task_arena・dma_arena・syscall・trap を使えませんでした
  （F2614）。いまはタスクの表・タイマー・アドレス空間がコアごとになり、一緒に使えます（[SPEC-1.0-INTEGRATION.ja.md](SPEC-1.0-INTEGRATION.ja.md)）。
- **コアごとの状態の持ち方**: `f20_cpu_ctx_t fascon__cpu[4]`（128 バイト境界）に own ヒープ・返却リスト・コアの番号・フレームと
  TLS ブロックへのポインタを置きます。ふつうのアリーナとフレームリージョンの管理そのものは `__thread`（AArch64 では
  TPIDR_EL0 からの固定の位置）です。
- **（2026-11 に変更）** パニックのときは、共有の旗に加えて、ほかの全コアへ停止の割り込み（AArch64 は SGI 2、x86_64 は NMI）を
  送ります。待ちループで旗に気づいたコアも `fascon_plat_core_parked` で止まり、止まったコアの数が報告に出ます。
- **AArch64 の仮想 BIOS（`--target aarch64`）は 1 コアのまま**です。PSCI が無いので、`spawn_on` は実行時にパニック 0x0012 に
  なります。**x86_64 の仮想 BIOS（`--target x86_64`）は 2026-11 からマルチコア**です（INIT-SIPI-SIPI）。SMP は既定の
  `fascon qemu`（`-kernel`、PSCI は HVC）・`--target x86_64`・PC 実行で動きます。
- **PSCI を SMC で呼ぶ道（EL2 で起動した場合）と、Windows / macOS / Linux arm64 の PC 実行は、実機では確かめていません**。
- **スタックとフレームの置き方**: CPU0 はこれまでの場所（128 KiB・1 MiB）、コア 1〜3 のぶんを `.smp_stack`（128 KiB × 3）と
  `.smp_frame`（1 MiB × 3）に足しました。own ヒープは 8 MiB を 4 等分します（SMP のプログラムだけ）。
- **`heap_live_blocks()` は全コアの合計**で、ほかのコアが動いている間に読むと途中の値になりえます。

## 5. 実装の場所

| 層 | ファイル | 内容 |
| --- | --- | --- |
| 構文 | `src/ast.rs`・`src/parser.rs`・`src/prescan.rs`・`src/lexer.rs` | `shared arena`・`chan`・`par()` |
| 型と規則 | `src/sema.rs`・`src/types.rs` | チャネルのメソッド、`spawn_on`・`cpu_count`、F2611・F2614・F2615 |
| 解析 | `src/context.rs`・`src/flow.rs` | コアのルート、F2601・F2603・F2612・F2613、`send` の移動 |
| C の生成 | `src/codegen.rs`・`src/strict.rs` | コアごとのアリーナ（`F20_PERCPU`）、`F20_DEFINE_CHAN`、`F20_READ_SECTION`、`spawn_on` の入口 |
| ランタイム | `runtime/fascon2_rt.h` | `f20_cpu_ctx_t`、返却リスト付きの own ヒープ、読み書きロック、チャネル、コンソールのロック、パニックの旗 |
| BSP | `bsp/qemu-virt-aarch64/start.S`・`bsp.c`・`kernel.ld`・`firmware.ld` | `_secondary_start`、PSCI の CPU_ON、`fascon2_secondary_main`、TLS ブロック、コアごとのスタック・フレーム・ヒープ |
| PC 実行 | `bsp/host/fascon2_plat_host.c` | コア = スレッド（pthread / Windows のスレッド） |
| 起動 | `src/qemu.rs`・`src/main.rs` | `-smp 4`（`--smp N` で 1〜4）、`-DFASCON2_SMP=1` |

## 6. 確かめたこと

- `examples/smp_kernel.fsc`: QEMU（`-smp 4`）と PC で `FASCON-SMP-OK`。仕事 48 件を 3 つのワーカーが分け合い、受け取った
  パケット（CPU0 が確保）をワーカーが解放し、返事（ワーカーが確保）を CPU0 が解放して、最後に生きている own ブロックが 0。
- `tests/ok/smp_basic.fsc`: PC と QEMU で同じ出力。PC では 25 回くり返して同じ出力。
- `tests/err/`: F2611 × 3、F2612 × 3、F2613、F2614、F2615、F2601（共有アリーナ）、F2603（チャネル）、F0382、F0515。
- 既存のテストを含む全 199 件が合格（`python3 tests/run_tests.py target/release/fascon --qemu`）。
