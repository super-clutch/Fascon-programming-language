# Fascon 1.0 仕様: マルチコア・割り込み・ユーザー空間・fascon-gc の統合と、マルチアーキテクチャの基盤

版の名前は **Fascon 1.0（1.0.0）のまま**です。この文書は 2026-11 の統合改修の仕様と、実装・検証の結果を、限界も含めて
そのまま書いたものです。英語版: [SPEC-1.0-INTEGRATION.md](SPEC-1.0-INTEGRATION.md)。前提の仕様:
[SPEC-1.0-KERNEL.ja.md](SPEC-1.0-KERNEL.ja.md)（割り込み・タスク・ユーザー空間）、[SPEC-1.0-SMP.ja.md](SPEC-1.0-SMP.ja.md)
（マルチコア）、[SPEC-1.0-UNIVERSAL.ja.md](SPEC-1.0-UNIVERSAL.ja.md)（プロファイル・fascon-gc・エコシステム）。

## 1. 何ができるようになったか

| 項目 | 以前 | いま |
| --- | --- | --- |
| `spawn_on` と `irq`・`task_arena`・`dma_arena`・`syscall`・`trap` | F2614 で拒否 | 一緒に使える（**F2614 は廃止**）。タスクの表・いまのタスク・実行キュー・タイマーはコアごと |
| `user_mem` の形 | 2 MiB の倍数 | **4 KiB の倍数**（F2803）。ページは初めて触れたときに写す（要求時ページング） |
| アドレス空間 | 表 1 枚を書き換え、切り替えのたびに TLB を掃く | **タスクごとのアドレス空間**（AArch64 は ASID、x86_64 は CR3 + PCID）。切り替えでは TLB を掃かない |
| ユーザープロセス | CPU0 だけ | **全コアで EL0 / Ring 3 のプロセスがプリエンプティブに動く** |
| システムコールの窓 | 恒等マップ側の窓をそのまま本体へ | マルチコアでは入口で**影バッファへ 1 回だけ複製**（2 回読みなし）、`&mut` の窓は出口で書き戻す |
| パニック | 旗を立て、待ちループのコアだけが止まる | **全コアへ停止の割り込み**（AArch64 は SGI 2、x86_64 は NMI）。止まったコアを数えて報告する |
| fascon-gc の一時値 | 式の途中の Id は根にならない | コンパイラが一時変数へ展開して `F20_GC_ROOT` に登録 |
| チャネルの中の Id | 根にならない | `gc_collect` がリングバッファの公開済みのセルを根として走査 |
| x86_64 の仮想 BIOS | 1 コア・タスクも割り込みもなし | 4 コア（INIT-SIPI-SIPI）、LAPIC タイマー、HPET、Ring 3、IPI、NMI |
| ほかのアーキテクチャ | — | ARMv7・MIPS64・RISC-V64・AArch64・x86_64 で生成 C を `-Werror` で翻訳・リンク、RISC-V64 のボードは QEMU で 4 ハート起動 |

## 2. 書き方: 構文は 1 つも増やしていない

既存の `spawn_on`・`irq`・`task_arena`・`user_mem`・`syscall`・`trap`・`critical`・`shared arena`・`gc_arena` を組み合わせる
だけです。ライフタイム注釈・ジェネリクス・生ポインタ・`unsafe` は増えていません。次は 4 コアそれぞれが自分のタイマーで
自分の EL0 / Ring 3 のタスクを切り替えるカーネルの骨組みです（全体は `tests/ok/smp_preempt_user.fsc`）。

```fascon
task_arena task: Task[4] stack 16384;               // コアごとの実体（スロット i はコアごとに別のタスク）
user_mem umem for task: 0x8_0000 at 0x1_0000_0000;  // 512 KiB。4 KiB の倍数ならよい
shared arena tally: Tally[1];                        // 全コアで 1 つ（critical の中だけ）

irq tick at 27 {                                     // コアごとのタイマー: そのコアの次のタスクへ
    timer_arm(ms(4));
    // task の表をたどって次の生きているタスク n を選び、task.resume(n)
}

syscall write at 1 (fd: u32, buf: &[u8]) -> i64 {    // マルチコアでは buf は影バッファ（1 回だけ複製）
    critical { put_bytes(buf); }
    buf.len() as i64
}

trap user_fault(esr: u64, far: u64, pc: u64) {       // 不正な動作をしたタスクだけが終わる
    critical { /* 数える */ }
}

fn core_main(c: u64) {
    // そのコアのタスクの表にユーザータスクを作り、そのコアのタイマーを張って最初のタスクへ（戻らない）
}

fn main() {
    spawn_on(1, core_main, 1);
    spawn_on(2, core_main, 2);
    spawn_on(3, core_main, 3);
    core_main(0);
}
```

fascon-gc では、Id を `let` に分けなくても回収で根から漏れません。

```fascon
gc_arena nodes: Node[8];
fn churn(n: u64) -> Option<Id<nodes>> { gc_collect(nodes); nodes.alloc(Node { v: n, next: None }) }

let s = link(nodes.alloc(Node { v: 1, next: None }), churn(2));
// 1 番目の引数の一時値（新しい Id）は、2 番目の引数の回収の間も根として残る（tests/ok/gc_temp_roots.fsc）
```

## 3. 規則とエラー

| 番号 | 内容 |
| --- | --- |
| F2614 | **廃止**。以前は `spawn_on` を使うプログラムで irq・task_arena・dma_arena・syscall・trap を拒んでいた |
| **F2616**（新） | マルチコアで、共有の周辺の割り込み（INTID 32 以上。CPU0 にだけ届く）のハンドラが、コアごとのアリーナに触った（ほかのコアで作った Id を CPU0 の表で引いてしまう） |
| F2601（広がった） | マルチコアでは irq の本体は「そのコアの割り込み禁止」にすぎない。ほかのコアの全ルート（task・syscall・irq）の効果を環境効果 E_env に含めて調べ、共有の実体への保護なしのアクセスを拒む |
| F2603（広がった） | `critical` の中のブロッキング操作（チャネルの `send` / `recv_wait`、タスクの切り替え・待ち）を、マルチコアでも拒む |
| F2613（そのまま） | 2 本以上のコアのルートから届く MMIO・ポート・`dma`・`dma_arena` への保護なしのアクセス |
| F2622（広がった） | `#[trap]`・`rcu_read`・`atomic` に加えて `critical` と `irq` のハンドラの中（そこから呼ぶ関数も）で、gc_arena の確保・回収・昇格 |
| F2803（緩めた） | `user_mem` の大きさと番地は 4 KiB の倍数、番地は恒等マップ（0〜0x8000_0000）の外・0x80_0000_0000 未満・1 GiB の区画の中 |

どのエラーも `fascon explain <ファイル>` で根本原因のタイムライン・直し方・数理的な理由（`data/rules.json`）が出ます。
`fascon dict F2616` で辞書を引けます（`data/dictionary.json`。VS Code 拡張の辞書も同じ内容）。

## 4. 9 つの不変条件と、どう守ったか

| # | 不変条件 | 実装と確かめ方 |
| --- | --- | --- |
| 1 | 言語の核は変えない | 構文の追加なし。F0515（第二級参照）と世代付き Id はそのまま。`&str` の定数はいまも F0515 |
| 2 | 新機能を使わないプログラムの C はバイト単位で同じ | `tools/compare_c.py` 相当の比較で 240 件中 234 件が一致。違う 6 件は、`spawn_on` を使うプログラムを `--target x86_64` で作ったもの（x86_64 がマルチコアの C を出すようになったため） |
| 3 | `critical` は ① 割り込み禁止 → ② スピンロック、解放は逆順 | `fascon_plat_critical_enter` / `exit`（ランタイムの既定: `fascon_plat_irq_save` → ロック）。待つ操作は F2603 |
| 4 | E(f) と E_env は単調 | マルチコアの E_env はほかのコアの全ルートの和。コアごとのアリーナ（TLS）は除き、分岐なしのラッチと列の走査の高速パスはそのまま |
| 5 | タスクの状態はコアごと | `f20_cpu_ctx_t` の `f20_sched_t`（いまのタスク・実行キュー・切り替えの依頼）。タイマー（PPI 27 / LAPIC タイマー）は全コアで有効 |
| 6 | TOCTOU（2 回読み）を構造的に防ぐ | マルチコアの syscall の入口が、窓を `fascon_plat_copy_from_user` で影バッファへ 1 回だけ写して本体へ渡す（AArch64 は LDTRB / STTRB と例外表、x86_64 は SMAP の STAC / CLAC と例外表） |
| 7 | パニックで全コアを止める | `fascon_plat_panic_broadcast`（AArch64: SGI 2、x86_64: NMI）。待ちループで気づいたコアは `fascon_plat_core_parked` で止まり、数えられる |
| 8 | 外部クレートなし・ABI の安定 | `Cargo.toml` の依存は空のまま。F3501・F3601 はそのまま |
| 9 | リアルタイム領域と GC 領域を分ける | F2622 を `critical`・`irq` へ広げた。共有の gc_arena の安全点の待ち合わせは、ロックを 1 つも持たない開いた区間でだけ起きる |

## 5. 抽象プラットフォーム API（`fascon_plat_*`）

コンパイラが出す C は、特定の CPU の命令を 1 つも含まず、次の関数だけを呼びます（宣言は `runtime/fascon2_rt.h`）。
割り込みコントローラ（GICv2・APIC など）・ページ表・TLB の命令は、すべて BSP の中にあります。

| まとまり | 関数 | 意味 |
| --- | --- | --- |
| 割り込みとロック | `irq_save` / `irq_restore` / `irq_open` / `wait_irq`、`critical_enter` / `critical_exit` | このコアの割り込みの禁止・許可・待ち。閉じた区間は ① 割り込み禁止 → ② ロック |
| 時計 | `timer_now` / `timer_freq` / `timer_arm` | 全コアで共通の単調な時計と、このコアの単発のタイマー |
| タスク | `task_create` / `task_create_user` / `task_enter` / `task_yield` / `task_resume` / `task_exit` / `task_current` | このコアのタスクの表への操作。文脈の保存と切り替えは BSP |
| マルチコア | `spawn_on` / `cpu_count` / `cpu_id` | コアの起動と番号 |
| アドレス空間 | `vm_space` / `vm_map` / `vm_unmap` / `vm_switch` / `vm_flush` / `vm_drop` | 4 KiB のページ・タスクごとの空間・ASID（PCID）ごとの TLB の撃ち落とし |
| ユーザー空間 | `user_register` / `user_window` / `copy_from_user` / `copy_to_user` / `syscall_register` / `trap_register` | 窓の検査と、境界を越える写し |
| パニック | `panic_broadcast` / `core_parked` | 全コアの停止と、止まったコアの数え方 |
| fascon-gc | `gc_task_tops` | 止まっているタスクのシャドウスタック |

いまある実装:

| プラットフォーム層 | 中身 |
| --- | --- |
| `bsp/qemu-virt-aarch64/`（`bsp.c`・`genos.c`・`start.S`・`kernel.ld`） | 上の全部。GICv2（SGI 1 = generic-os の TLB、2 = パニック、3 = アドレス空間の TLB）、PSCI、ASID 16 ビット、TTBR0 の切り替え |
| `bsp/qemu-x86_64-bios/`（`bios.S`・`bsp.c`・`bios.ld`） | 上の全部。IDT 256・GDT / TSS / IST・LAPIC・I/O APIC・HPET・INIT-SIPI-SIPI・FS の TLS・CR3 + PCID・SMAP / SMEP / NX・IPI 0xF0・NMI |
| `bsp/generic-aarch64/`、`boards/qemu-virt-riscv64/` | 割り込み・時計・コアの数。タスクとユーザー空間はない（作ると失敗を返す） |
| `bsp/portable-stub/` | 移植のひな形（ARMv7・MIPS64・RISC-V64 ほか）。★の箇所をボードの装置に置き換える。タスクは 1 つずつ最後まで走らせる |
| `bsp/host/` | PC での実行（コア = スレッド） |

有志が新しいボードを足すときは、この表の関数を `boards/<名前>/` の C で書き、`board.toml` に書くだけで、コンパイラには
触れません。`python3 tools/cross_check.py target/release/fascon` が、5 つのアーキテクチャで生成 C を `-Werror` で翻訳して
参照プラットフォーム層とリンクできることを確かめます。

## 6. AArch64（QEMU virt）の実装

- **コアごとのタスク**: タスクの表は `bsp_tasks[コア][スロット]`、文脈はそのタスクのカーネルスタックの上の保存フレーム
  （800 バイト）の番地 1 つ。切り替えは `start.S` の出口へ別のフレームを返すだけ。
- **割り込み**: GICv2。PPI（16〜31。タイマーの 27 / 30）はコアごとに、そのコアが割り込みを開いたときに許可。SPI（32 以上）は CPU0 へ。
- **アドレス空間**: タスクごとの L1 表（カーネルの L1 を写す）と、その下の L2 / L3（4 KiB）。TTBR0 に 16 ビットの ASID を
  載せて書くだけで、TLB は掃かない。掃くのはページを外す・空間を捨てるときだけで、その ASID を使ったコアだけ
  （自分は `tlbi vae1` / `aside1`、ほかは郵便受けと SGI 3）。
- **要求時ページング**: EL0 の変換フォルト（FSC 4〜7）で、そのタスクのユーザーメモリの中なら 4 KiB を写して命令をやり直す。
- **境界の写し**: `LDTRB` / `STTRB`（EL0 の権限で読み書き）と例外表。まだ写していないページは写してやり直し、それ以外は EFAULT。
- **撃ち落としの郵便受け**: 送り手は 1 度に 1 つ（割り込みを止めて取る全体のロック）。郵便受けの引数は 1 つなので、同じ
  コアへの同時の依頼が互いを上書きしない。待つ間も自分の郵便受けを読むので、頼み合っても止まらない（`genos.c` の SGI 1 も同じ）。
  起動時に全コアへ SGI 3 を送る自己試験をする。

## 7. x86_64（QEMU pc・Fascon の仮想 BIOS）の実装

- **入口**: 256 個のベクタすべてが同じ形の保存フレーム（FXSAVE 512 バイト + 汎用レジスタ 15 本 + ベクタ・エラーコード +
  ハードウェアのフレーム、計 688 バイト）を作り、`bsp_x86_trap` が復帰するフレームを返す（AArch64 と同じ形）。
- **GDT・TSS・IST**: Ring 0 / Ring 3 のセグメントと、コアごとの TSS。NMI は IST 1、#DF と #MC は IST 2。
- **時計**: `timer_now` / `timer_freq` は HPET の主カウンタ（QEMU では 100 MHz。無い機械では PIT で測った TSC）。
  `timer_arm` はそのコアの LAPIC タイマーの単発で、起動時に測った比で刻みを直す。LAPIC タイマーは **irq 27**（27 が無く
  30 だけあれば 30）、I/O APIC の入力 n は **irq 32 + n**。同じ Fascon のソースが両方のアーキテクチャで動く。
- **マルチコア**: コア 0 がトランポリンを 0x8000 へ写し、INIT → SIPI → SIPI で起こす（コアの数は QEMU の fw_cfg）。
  各コアは FS の土台を自分の TLS ブロックの終わりへ向ける（x86_64 の TLS variant II、`%fs:0` は自分自身の番地）。
- **Ring 3**: `int $0x80`（DPL 3 のゲート）。番号は rax、引数は rdi・rsi・rdx・r10・r8・r9・r12・r13、戻り値は rax。
  Ring 3 から入ったときは、スタックの一番上に置いたこのコアの FS の土台を入口で FS へ戻す（ユーザーが FS を変えても
  カーネルは自分の実体を引ける）。
- **アドレス空間**: タスクごとの PML4 と PDPT（カーネルの 0〜4 GiB の恒等写像は大域ページのまま共有）、その下に PD / PT。
  PCID があれば CR3 を「掃かない印」付きで書き、ほかのコアの PCID は INVPCID か「次に載せるときに掃く」印で消す。
  カーネルのタスクへ切り替えるときは、いま載っている空間をそのまま使う（カーネルの半分はどの空間でも同じで、ユーザーの
  半分は SMEP / SMAP が守る）。
- **境界の写し**: SMAP があれば STAC / CLAC の間だけ Ring 3 のページに触れ、例外表で EFAULT を返す。入口で `clac` するので、
  ユーザーが AC を立てても SMAP は効いたまま。
- **trap**: Ring 3 の例外は、AArch64 の ESR と同じ区分で trap のハンドラへ渡す（#UD → 0x00、#PF → 0x24 / 0x20 と
  DFSC、#AC → 0x24 / 0x21、int3 → 0x3C、#DE / #MF / #XM → 0x2C、ほかは 0x00 で ISS = ベクタ）。`far` は CR2、`pc` は RIP。
- **パニック**: NMI を「自分以外の全コア」へ。NMI は割り込みを止めた区間にも届くので、コンソールのロックを持ったまま
  止まるコアがありうる。パニックしたコアは、しばらく待っても空かないコンソールを取り上げて報告する（ランタイム）。
- **generic-os**: どのアーキテクチャでも同じ部分（64 ビットの Id・コアごとのマガジン・RCU・共有の gc_arena）は動く。
  守りページ・仮想番地を予約したアリーナ・装置の高位の写し・PAN / PAC / BTI は AArch64 の BSP の機能で、x86_64 には無い。

## 8. fascon-gc

- **一時値**: 回収を起こしうる式（`gc_collect` を直接・間接に呼ぶ関数、確保）の評価の間、それより前に評価した gc の Id
  （を含む構造体・Option）の一時値を、コンパイラが一時ローカルへ移して `F20_GC_ROOT` でシャドウスタックへ登録する。
  登録は文の終わりで外す（ブロックの終わりまで根を延ばさない）。回収を起こさない式には何も足さないので、既存の
  プログラムの C は変わらない。
- **ローカルの own（2026-11）**: ローカル・仮引数・一時値・戻り値の入れ物の `own T`・`Option<own T>` の先にある Id も根。
  移動・解放・take のたびに C の変数を先に NULL / 0 にする（SPEC-1.0-UNIVERSAL 5.3.1）。
- **チャネル**: `gc_collect` は、共有の gc_arena の Id を含みうる要素のチャネルについて、リングバッファの公開済みの
  セル（`seq == 位置 + 1`、head から tail まで。`own` の中も型の記述子でたどる）を根として読む（`F20_CHAN_GC_WALK`）。

## 9. 確かめたこと（この環境で実際に動かした結果）

- `python3 tests/run_tests.py target/release/fascon --qemu --fuzz 2`: **317 件すべて PASS**（うち 5 件は辞書の検査: `tools/check_dictionary.py`、VS Code 拡張との同期、全項目と別名を日英の `fascon dict` で引くこと、全エラーテストの `fascon explain`）。
- 生成 C の比較: 240 件中 234 件が元の 1.0.0 と 1 バイトも違わない（違う 6 件は 4 節の不変条件 2 のとおり）。
- QEMU virt（AArch64）`-smp 4`: `tests/ok/smp_preempt_user.fsc`（8 本の EL0 プロセス + 敵対的なプロセス、要求時ページング、
  TOCTOU の検査 8/8、E2BIG、`&mut` の書き戻し、全コアのタイマーによる切り替え）が繰り返して成功、SGI 3 の自己試験 3/3。
- QEMU pc（x86_64）`-smp 4`: `tests/qemu/smp_preempt_user_x86.fsc`（Ring 3 で同じシナリオ）が 10 回中 10 回成功。カーネルの
  テスト 10 本（`kernel_preempt`・`critical`・`smp_basic`・`smp_irq_tasks`・`smp_dma_ring`・`gc_basic`・`gc_own_promote`・
  `gc_temp_roots`・`gc_chan_roots`・`gc_shared`）が AArch64 と同じ出力（`timer_freq` の値だけは機械の時計）。
- パニックの全コア停止: `tests/qemu/smp_panic_broadcast.fsc` と、出力の最中のコアを止める `tests/qemu/smp_panic_console.fsc`
  が両方のアーキテクチャで「3 other core(s) stopped」・終了コード 70（修正前の x86_64 では後者が行き詰まった）。
- 敵対的なユーザープログラム: カーネルの番地の窓とユーザーメモリより長い窓がどちらも -14（EFAULT）、未定義命令は trap へ。
- 生成 C の翻訳とリンク: 9 本 × 5 アーキテクチャ（ARMv7・MIPS64・RISC-V64・AArch64・x86_64）= 45 本、`-Werror` で警告なし。
- RISC-V64 のボードのパッケージ: `qemu-system-riscv64` の virt で 4 ハート起動、`FASCON-BOARD-OK`。
- 新しいエラーのテスト: F2616・F2601（SMP）× 2・F2603（SMP）・F2613（DMA）・F2622（critical / irq）・F2803（4 KiB）。

## 10. 限界・確かめていないこと（正直に）

- **QEMU の TCG は PCID も INVPCID も真似しません**（この環境には KVM もありません）。x86_64 の PCID の道は実装してあり
  ますが、ここでは動かしていません。TCG の上では CR3 を書くたびに大域でない TLB の項目が消えます（カーネルの大域ページは
  残る。カーネルのタスクへの切り替えでは CR3 を書かない）。
- タスクはコアを移りません。ASID / PCID は（コア, スロット）ごとなので、ふつうの空間の掃除は自分のコアだけで済みます。
  IPI / SGI による撃ち落としは、起動時の自己試験と generic-os の CoW で確かめています。
- 境界の写しは 1 バイトずつです（影バッファは最大 0xFFF0 バイト）。速さはまだ詰めていません。
- x86_64 の Ring 3 の時計は RDTSC で、速さは CPUID 15H / 16H から求め、無い CPU（QEMU の TCG など）では 1 GHz とみなします。
- generic-os の `f20_aspace`（プロセスの所有権の表と CoW）は、まだ実のページ表につながっていません
  （`fascon_plat_pte_set` は既定の空の実装）。EL0 / Ring 3 のタスクのアドレス空間は実のページ表です。
- AArch64 の仮想 BIOS（`--target aarch64`）は PSCI が無いので 1 コアのままです（`--target x86_64` はマルチコア）。
- `bsp/portable-stub/` はひな形で、タスクの文脈の切り替えはしません。ARMv7・MIPS64 は翻訳とリンクまでで、実行はしていません。
- 実機（基板・PC）では確かめていません。macOS と Windows の実行ファイルはクロスビルドで、macOS 版は動かしておらず、Windows 版は Wine でだけ動かしました。
