# Fascon 1.0 ユニバーサル OS 仕様（`--target rtos` / `--target generic-os`、fascon-gc、エコシステムと実機の基盤）

この文書は、Fascon 1.0 を「1 つの言語で、ベアメタルの RTOS から高性能な汎用マルチプロセス OS までを書ける」言語へ広げた
追加分の設計と実装をまとめたものです。版の表記はすべて **Fascon 1.0（1.0.0）** のままです。英語の要約は
[SPEC-1.0-UNIVERSAL.md](SPEC-1.0-UNIVERSAL.md) にあります。

---

## 0. 守ったこと（互換性の約束）

| 約束 | どう確かめたか |
|---|---|
| 既存の構文・メモリ安全の静的規則（F0507・F0515・F0520・F2101 など）は変えない | 既存のテスト（`tests/err` の全件）が同じコードで通る |
| 既存のプログラムが出す C を **1 文字も変えない** | 42 本のプログラム × 日英の 84 ファイルを、元の 1.0.0 の実行ファイルの出力とバイト単位で比較して **すべて一致**（`python3 tools/compare_c.py <新しい fascon> <古い fascon> <ファイル>...` で、英語の出力を注釈と空白を除いて同じように比べられます） |
| 新しい機能は、使ったプログラムにだけ C を足す | `gc_arena`・`--board`・`// fascon-target:` などを使わない限り、定義も関数も出ない |
| コンパイラを変えずに有志が拡張できる | 標準ライブラリ（`lib/`）・HAL・ボードのパッケージ（`boards/<名前>/`）・エラー辞書・修正規則（`data/*.json`）はすべて外部のファイル |
| 外部のクレートを使わない | `Cargo.toml` の依存は 0（TOML の部分集合の読み取り・JSON・ELF の読み取りも自前） |

テストの結果（この版）: `python3 tests/run_tests.py target/release/fascon --qemu` で **225 件以上すべて合格**（PC での実行、
エラーの検出、厳格形の往復、QEMU の AArch64 / x86_64 の起動、generic-os の BSP、fascon-gc、ボードのパッケージ）。

---

## 1. プロファイル: classic / rtos / generic-os

| | classic（既定） | `--target rtos` | `--target generic-os` |
|---|---|---|---|
| 用途 | 既存の 1.0（C は不変） | ベアメタル RTOS・マイコン | モバイル・汎用のマルチプロセス OS |
| Id | 32 ビット `[世代 32-K | 添字 K]` | 32 ビット（同じ） | **64 ビット** `[世代 32 | 添字 32]`、`0` = None |
| アリーナの置き場 | `.bss.fascon_arena` | `.fascon_arena`（固定） | **仮想番地を予約**（`FASCON2_ARENA_VA_BASE`）し 2 MiB ずつ要求時に写す |
| 再帰 | 許す | **F3001** で禁止（スタックの上限を証明するため） | 許す（守りページで検出） |
| スタック | — | `fascon stack` が呼び出しグラフから最大消費を証明 | 4 KiB の守りページ |
| 確保 | フリーリスト | フリーリスト | **Per-CPU マガジン**（ロックフリー）+ RCU で遅延解放 |

選び方（強い順）: ファイル先頭の `// fascon-target: generic-os` → `--target` / `--profile` → 環境変数 `FASCON_PROFILE` → board.toml の
`board.profile` → classic。

---

## 2. 言語に足したもの（ジェネリクスなし・型名が先・1 行のプリミティブ）

| 構文 | 意味 | 主な検査 |
|---|---|---|
| `Result<T, E>`・`?` | 失敗を値で返す（`Id<arena>`・`Option<T>` と並ぶ唯一の組み込みの型引数） | F3105 |
| `catch_fault { .. }` | ラッチ（`_flt`）に溜まった異常を `Result<T, FaultError>` にして局所的に後始末（goto と drop） | F3106 |
| `user [u8; N]`・`user Str` | ユーザー空間のバッファ。`copy_from_user(buf)?` で 1 回だけ影の写しを作る | F3101（2 回読み）・F3102 |
| `#[syscall(N)] fn` / `#[trap] fn` | システムコールの表と検証・EL0 の例外（Copy-on-Write）。#[trap] は眠れない | F3103・F3104・F3204・F3203 |
| `dma own T`・`dma_alloc(T)`・`submit_dma`・`complete_dma(T, iova)` | 専用のページ。貸している間は CPU から触れない | F3401〜F3403 |
| `bind_mmio(Regs, 番地)` | レジスタ構造体を装置の窓に結ぶ（board.toml / DTB / PCIe の窓だけ） | F3404 |
| `atomic { 表[id].f op= v; }` | 1 回の不可分な読み書き（列は自動で使わない＝ fascon regions に理由） | F3201 |
| `rcu_read { .. }` | ロックフリーの読み手（書き込み・確保・解放・眠りは不可） | F3202・F3203 |
| `#[secret]` | 0 埋め・秘密による分岐の禁止・定数時間の `ct_eq` | F3301〜F3304 |
| `#[on_drop(f)]`・`#[no_column]`・`#[stable_abi]` | 破棄の前処理・列を作らない・ABI の固定 | F3505・F3502・**F3501**（Id の直接配置） |
| `#[asm(..)]`・`extern "C"` | 効果つきの安全なアセンブリの境界・C との境界 | F3503・F3601（#[secret] はオペランドに入れない: F3303） |
| `import net::tcp;` | 名前空間のモジュール（ライフタイムもジェネリクスもなし） | F3701〜F3703 |
| `gc_arena n: T[N];`・`gc_collect(n)`・`promote_to_gc(n, id)` | fascon-gc（5 章） | F2621〜F2625 |

**破棄の順序**（`#[on_drop]` と `#[secret]` を併用した型）は生成コードで厳密に **1. on_drop の関数 → 2. #[secret] の揮発性の 0 埋め
+ メモリバリア → 3. 記憶域の解放** です。

---

## 3. `--target rtos`

- Id は 32 ビット。アリーナは `.fascon_arena` に静的に置く（リンカスクリプトで位置が決まる）。
- **再帰は F3001**（関数ポインタの呼び先の候補も含めた呼び出しグラフの閉路を Tarjan で検出）。
- `fascon stack <file> [--limit N] [--ext-budget N] [--cc gcc]`: GCC の `-fcallgraph-info=su,da` から各関数のフレームを読み、
  呼び出しグラフの最長経路で **最大スタック消費を証明** する（単位の外の関数には予算を置く）。

```
$ fascon stack tests/ok/universal_lang.fsc
  worst case: 800 B / limit: 16384 B
  OK: proven to fit the limit
```

---

## 4. `--target generic-os`

### 4.1 64 ビット Id と仮想番地を予約したアリーナ

`f20_id_t = uint64_t`、上位 32 ビット = 世代、下位 32 ビット = 添字、`0ULL` = None。世代は 32 ビットなので実質枯渇しません
（32 ビット Id の世代リフレッシュは不要で、コンパイル時に省かれる）。共有アリーナは仮想番地の予約域に置き、追加の間接参照
なしで `F20_ARENA_VA(off)` を直接読みます。`next_unused` が 2 MiB の境界を越えるときだけ、ビットロックの下で
`fascon_plat_arena_grow()` が要求元のコアの NUMA ノードから 0 埋めの 2 MiB ページを写します。

### 4.2 Per-CPU マガジンと RCU

確保は各コアのマガジン（ロックフリー）から。解放は `fascon_rcu_defer()` で猶予期間の後にマガジンへ戻します。
`rcu_read { .. }` の読み手はプリエンプション禁止 + エポックの公開で、書き手は 2 回のエポックの進みを待ちます。

### 4.3 プロセスのアリーナと TLB

`arena procs: Proc[N];` には隠れた `cpu_mask` があり、`map_page`・`share_page`・`cow_copy`（`Option<own [u8; 4096]>`）・
`tlb_flush` などを持ちます。`tlb_flush` は、自分のコアだけならローカルの TLBI、ほかのコアには **そのプロセスを走らせている
コアへだけ SGI** を送ります（QEMU の BSP: `genos.c`）。

```c
static inline void genos_tlbi_local(uint64_t va) {
    /* このコアだけ（Non-shareable）: tlbi vaae1 + dsb nsh + isb / this core only */
    __asm__ __volatile__("dsb nshst\n tlbi vaae1, %0\n dsb nsh\n isb" : : "r"(va >> 12) : "memory");
}
/* 頼む側: 郵便受けに番地を置き、GICD_SGIR でターゲットの一覧へ SGI 1 番。待つ間は自分の郵便受けも見る（互いに頼み合っても
 * 止まらない）。答えが無ければ止まった理由をつけてパニック（黙って固まらない） */
MMIO32(GICD + 0xF00) = (uint32_t)((others & 0xFFu) << 16) | GENOS_SGI_TLB;
```

### 4.4 QEMU の generic-os BSP（`bsp/qemu-virt-aarch64/genos.c`）

`fascon qemu` は generic-os の C を見ると `-cpu max,pauth-impdef=on`・`-mbranch-protection=standard`・`-fstack-clash-protection`
で翻訳して起動します。起動時の表示（実際の出力）:

```
[genos] generic-os: null guard 0x0..0x200000, MMIO mirror 0x0000006000000000, arena VA 0x0000005000000000, PAN on, PAC on, BTI on
[genos] W^X: 16 text pages RX+GP, 1 rodata pages RO, 0 shared pages; L3 tables 3, stack guards 4, DTB devices 38, PCIe functions 2
[genos] self-test: exception-table fixup on the null guard ok, PAN: EL1 load of a user page blocked, LDTRB/STTRB ok
```

| 機能 | 実装 |
|---|---|
| 守り | 仮想 0〜2 MiB は未マップ（ヌル参照は Data Abort）。カーネルスタック・タスクスタック（フレームリージョン \| 守り 4 KiB \| スタック）の 4 KiB の守りページ |
| スタックの溢れ | 例外の入口が保存フレームを積む **前に** `AT S1E1W` で SP-1 と SP-800 を確かめ、守りなら各コアの非常用スタックへ移って報告 |
| W^X | カーネル像のブロックだけ起動時（MMU を有効にする前）に 4 KiB へ分ける: .text = RX + GP（BTI）、.rodata = RO、ほかは RW + PXN。break-before-make は不要 |
| PAN + LDTR | PAN = 1（SPAN = 0 で例外のたびに 1）。ユーザーメモリは `LDTRB`/`STTRB` だけで写し、失敗は `__ex_table` の飛び先へ（EFAULT） |
| PAC / BTI | APIA 鍵と `SCTLR_EL1.EnIA`・`BT1`。EnIA を立てる関数自身は `branch-protection=none`（戻りで検証しない） |
| 装置の高位の写し | 物理 0〜1 GiB を `0x60_0000_0000` に写し、`mmio` / `bind_mmio` はここを通す |
| バス列挙 | QEMU が置く DTB（FDT）を歩き、装置の `reg` を bind_mmio の窓に登録。PCIe（ECAM）の BAR を 32 ビットの MMIO 窓から割り当てる |

スタックの溢れを捕まえる入口（`start.S`）:

```asm
    .macro STACK_CHECK
    msr     tpidr_el1, x0
    mov     x0, sp
    sub     x0, x0, #1
    at      s1e1w, x0              /* SP-1 は写っているか / is SP-1 mapped? */
    isb
    mrs     x0, par_el1
    tbnz    x0, #0, el1_stack_overflow
    mov     x0, sp
    sub     x0, x0, #800           /* 保存フレームの底 / bottom of the saved frame */
    at      s1e1w, x0
    isb
    mrs     x0, par_el1
    tbnz    x0, #0, el1_stack_overflow
    mrs     x0, tpidr_el1
    .endm
```

ユーザーメモリの 1 バイトの読み（例外表つき）:

```c
__asm__ __volatile__("1: ldtrb %w0, [%2]\n"
                     "2:\n"
                     ".pushsection __ex_table, \"a\"\n.balign 8\n.quad 1b, 3f\n.popsection\n"
                     ".pushsection .text.fixup, \"ax\"\n3: mov %w1, #1\n b 2b\n.popsection\n"
                     : "+r"(v), "+r"(err) : "r"(ua) : "memory");
```

既存のカーネル（`examples/kernel_user.fsc`・`docs/kernel-design/step4_userspace.fsc`・`examples/smp_kernel.fsc`）は
`FASCON_PROFILE=generic-os` でもそのまま起動します（EL0 のタスク・システムコールの写しは PAN の下で LDTR を通る）。

---

## 5. fascon-gc（`gc_arena`: 決定論的・アリーナ連動型の正確な GC）

### 5.1 構文

```fsc
struct Node { v: u64, next: Option<Id<nodes>> }
gc_arena nodes: Node[1024];          // または gc_arena @nodes: Node[1024]; / shared gc_arena g: T[N];

fn main() {
    let a = nodes.alloc(Node { v: 1, next: None }).unwrap();
    let freed = gc_collect(nodes);    // 回収したスロット数（u32）
}
```

- 要素は既存の着色型 `Id<nodes>` で指します（F2101・F0515・F0520 はそのまま）。Id の幅はプロファイルに従います
  （rtos / classic = 32 ビット、generic-os = 64 ビット）。
- 手動の `free` / `remove` は **F2621**。解放はランタイムだけの仕事です。

### 5.2 型記述子（.rodata）と正確な走査

生ポインタが無いので、どのバイトが Id か own かはコンパイラが正確に知っています。生成される記述子（実際の出力）:

```c
static const f20_gc_field_t fsc_td_Node_f[] = {
    { .off = (uint32_t)__builtin_offsetof(fsc_Node, next), .kind = F20_GC_ID, .heap = 0u },
    { .off = (uint32_t)__builtin_offsetof(fsc_Node, peer), .kind = F20_GC_ID, .heap = 0u }
};
static const f20_type_desc_t fsc_td_Node = { (uint32_t)sizeof(fsc_Node), 2u, fsc_td_Node_f };
```

種類は `F20_GC_ID`（Id・Option<Id>）・`F20_GC_OWN`（own の先を記述子で）・`F20_GC_ARR`（固定長配列・値で埋め込んだ構造体）・
`F20_GC_OPT`（Option<構造体> の is_some が真のときだけ）。保守的 GC のような誤認は起きません。

### 5.3 根（ルート集合）

1. 静的変数（書き換えでは触れない） 2. ふつうの `arena` / `task_arena` / `bump_arena` の生きているスロット（自動で登録）
3. **シャドウスタック**: gc の Id を持つローカル・引数と、**own T・Option<own T>（構造体・配列の中も）を通して gc の Id に
   届くローカル・仮引数・一時値・戻り値の入れ物**（2026-11、5.3.1）。gc_arena に触れる関数だけが枠を積み、
   `__attribute__((cleanup))` がどの出口でも外す（関数・束縛の有効範囲が終わると根から消える）4. 止まっているタスクの
   シャドウスタック（BSP が切り替えで保存・復元し、`fascon_plat_gc_task_tops` で渡す）。

```c
static void fsc_make_cycle(uint64_t n) {
    F20_GC_FRAME(2u); /* この関数の根の枠（シャドウスタック） */
    fsc_Id_nodes a = _t1;
    F20_GC_ROOT(0, &a, &fsc_td_r_Id_nodes);
    ...
}
```

### 5.3.1 ローカルの own の根（2026-11）

own の中の Id も根として読むために、次の 3 つを組み合わせています。

1. **根にする型と記述子**: `src/codegen_gc.rs` の `gc_root_ty` は、Id を直接持つ型に加えて、own・Option<own T>（構造体・
   配列の中も）を通して gc の Id に届く型を根にします。own の先に Id がある型は、own を辿る記述子（`F20_GC_OWN` の行）で
   登録します。own の先に Id の無い型は今までの記述子（`fsc_td_r_*`）のままです。

   ```c
   static const f20_gc_field_t fsc_td_Own_Holder_f[] = {
       { .off = 0u, .kind = F20_GC_OWN, .desc = &fsc_td_Holder }
   };
   fsc_Holder *h = _o2;
   F20_GC_ROOT(0, &h, &fsc_td_Own_Holder);
   ```

2. **変数はいつも「有効な値か 0 / NULL」**: どの命令の境目で根を読んでも、解放済みの own を指す根はありません。
   - 移動（`let y = x;`・値渡しの引数・リテラルのフィールド・return）は移動元を NULL / 0 にします（`SPEC-1.0-EXT` 10.1 と同じ）。
   - 解放は `fsc_own_drop_at_<型>(&持ち主)` を通します。子を解放し（#[on_drop] はこの間も持ち主から届く）→ 持ち主を NULL →
     記憶域を返す、の順です。構造体の中の own フィールドも同じ関数で、解放の前に NULL になります。解放した構造体・Option・
     配列の変数は 0 で埋めます。再帰的なリストは、残りの鎖を親から届いたまま 1 つずつ外します。
   - `.take()` は値を一時変数へ移し、移動元を None にします。上書き（`b = own ..`）は新しい値を作り、古い値を一時変数へ
     退避して新しい値を置き、それから古い値を解放します（解放中に回収が起きうるなら古い値も根にする）。
   - フレームリージョンの own は、領域を戻す前に変数を NULL にします。完全列化の要素まるごとの上書きは、新しい値を置いてから
     古い値を解放します（gc_arena のプログラムだけ）。
   - 値が入る前の変数（`let x = if .. { .. } else { .. };`）は NULL / 0 で始めます。
3. **一時値の受け渡し**: 式の途中で作った own（`own Node { next: .. }`・移動した own・呼び出しの戻り値・`.take()`）は、
   後ろの被演算子が回収しうるとき一時ローカルへ出して根にします。回収しうる被演算子を先に評価し終えてから、呼び出し・
   リテラルへ渡す直前に根から外します（呼び出し先の仮引数・行き先の変数が次の根）。同じ own を 2 つの根が指すことは
   ありません。世代リフレッシュで同じ own の Id を 2 回書き換えると None になってしまうので、これは正しさの条件です。

**#[on_drop] の順序**: スコープの終わりの破棄は、`#[on_drop(f)]` の f を呼ぶ（値はまだ根から届く。f の中の gc_collect も
その Id を生かす）→ 子を解放（それぞれ NULL にしてから）→ 持ち主を NULL → 記憶域を返す → 束縛の有効範囲の終わりで
cleanup が根の枠を外す、の順です。戻り値の入れ物（`_rv`・return の一時変数）も own の先まで根にするので、末尾の式を
入れたあとのスコープの終わりの #[on_drop] が回収しても戻り値の Id は漏れません。

**ランタイム**（`runtime/fascon2_rt.h` の `FASCON2_GC` の節だけ。gc_arena の無いプログラムは前処理の結果が同じ）:

- `F20_GC_OWN` の先が NULL（移動済み・解放済み・None）なら `f20_gc_walk` の入口で飛ばします。詰めた子・フレームリージョンの
  own も記述子のオフセットで読みます。
- 入れ子の `gc_collect`（掃除の中の #[on_drop]、同じコアで割り込んだタスク）は何もせず 0 を返します（外側の回収の印・
  作業スタックを壊さない）。
- `FASCON2_PREEMPT`（irq / task_arena / critical を使う）では、印付けと死んだスロットの一覧が決まるまで、このコアの割り込みを
  止めます（止まっているタスクの鎖を歩いている間に、そのタスクが走り出さない）。
- 鎖は先頭の枠で見分けて 1 回だけ歩きます。共有の gc_arena の回収中に別のコアがタスクを切り替えたら、新しいタスクを走らせる
  前に止まります（`bsp_switch`。QEMU virt AArch64 と x86_64 の BSP）。関数の入口の安全点は柵を置いてから回収中かを読み
  （`F20_GC_POLL_ENTRY`）、回収するコアも gc_req を奇数にしてから柵を置いて枠を読みます。枠の先頭・生存ビットマップは
  原子的に読み書きします（x86-64 / AArch64 ではふつうのロード・ストア）。共有のヒープの記述の初期化は 1 コアだけが行います。
- 検査用の `-DFASCON2_GC_ALIAS_CHECK`: 1 回の走査で同じ own の記憶域へ 2 度届いたら止まります（パニック 0x000f）。
  テストはこれと AddressSanitizer / UBSan / LeakSanitizer（`-DFASCON2_HEAP_MALLOC`）、共有の gc_arena は ThreadSanitizer でも
  走らせています。

### 5.4 印付けと掃除

- 印は `mark[]` のビット。作業スタックで辿り、**要素の Id がすべて単純なフィールドなら、スロット本体ではなく既存の列の
  仕組み（`F20_DEFINE_TABLE_COLS`）で作った「Id 専用の射影列」だけを読みます**（列が使えない要素は記述子でスロットを読む）。
- 掃除は `live & ~mark` を `ctz`（`__builtin_ctzll`）/ `blsr`（`m &= m - 1`）で 1 ビットずつ取り出し、先に添字を集めてから
  そのアリーナの `free` へ渡します: own フィールドの drop・0 埋め・世代 +1・列の unmirror（0 / NONE）・フリーリスト
  （generic-os の共有アリーナはマガジン）。共有の gc_arena は **RCU の猶予期間の後** に解放します。

### 5.5 3 つの動作モード

1. **エフェクト駆動の局所回収**: 共有でない gc_arena はコアごとの実体なので、回収はそのコアだけで完結し、ほかのコアは一切
   止まりません。**共有の gc_arena** は、効果 E(f) からその gc_arena に（推移的に）触れる関数だけに安全点（入口とループの頭、
   チャネル・錠の待ち）を置き、回収するコアは「その関数の中にいるコア」だけを待ちます（枠が空のコアは止めない）。
2. **世代リフレッシュ**（32 ビット Id のときだけ）: 世代が上限の 3/4 を越えたスロットや引退したスロットがあると、gc_collect の
   中で全ての根・全てのアリーナの生きている Id を世代 1 へ書き換え（古い Id は 0 = None）、引退したスロットを空きへ戻します。
   64 ビット Id（generic-os）ではコンパイル時に省かれます。`tests/ok/gc_refresh.fsc`（K = 20 → 12 ビットの世代で 6,600 回の回収）
   で、ローカル・ほかのアリーナ・gc_arena の中の Id がリフレッシュ後も同じ要素を指すことを確かめています。
3. **bump_arena からの退避**: `promote_to_gc(nodes, bump_id)`。さらに **エスケープ解析による自動昇格**: bump_arena の Id を、
   同じ要素型の gc_arena の Id が要る場所（戻り値・引数・フィールド・型つきの let）へ渡すと、コンパイラが昇格を差し込みます。

```fsc
fn best(n: u64) -> Option<Id<leaves>> {   // leaves: gc_arena, scratch: bump_arena（要素は同じ Leaf）
    let mut pick = scratch.alloc(Leaf { v: 0, up: None });
    ...
    pick                                    // ← Option<Id<scratch>> が自動で gc_arena へ写される
}
```

### 5.6 安全とリアルタイム性（コンパイル時の検証）

| コード | 内容 |
|---|---|
| F2621 | gc_arena に `free` / `remove` |
| F2622 | `#[trap]`・`rcu_read`・`atomic` の中で gc_arena への確保・`gc_collect`・`promote_to_gc`・自動昇格 |
| F2623 | `--gc=off`（`FASCON_GC=off`）で gc_arena を使った。GC のランタイムは `FASCON2_GC` が無い限り 1 バイトもリンクされない |
| F2624 | gc_arena の要素に `dma own T` |
| F2625 | コアごとのアリーナの要素に共有の gc_arena の Id（回収するコアから見えない根） |
| F3501 | `#[stable_abi]` の構造体に `Id<@a>` を直接置いた（rtos 32 / generic-os 64 ビットで幅が変わる） |

**エラーコードの番号について**: 依頼の仕様では F2601〜F2604 でしたが、Fascon 1.0 では F2601（共有リージョンへの保護なしアクセス）・
F2602（割り込みハンドラの形）・F2603（割り込み文脈での切り替え）・F2605 がすでに別の意味で使われており、変えると既存の
プログラムの診断が変わってしまいます。そこで **下 1 桁を仕様と合わせて F2621〜F2624**（と F2625）に置きました。
同じ理由で、`#[on_drop]` の関数の形の誤りは F3505 に移し、F3501 を仕様どおり `#[stable_abi]` の Id に使っています。

### 5.7 限界（正直に）

- 回収点は `gc_collect` の文（と共有の gc_arena の安全点）です。**（2026-11 に解消）** 式の途中の一時値だけが持つ Id も、
  コンパイラが一時ローカルへ展開して `F20_GC_ROOT` に登録するので、根になります（`let` に分けなくてよい）。
- 共有の gc_arena の回収は「その gc_arena に触れる関数の中にいるコア」の短い停止（安全点での待ち合わせ）です。ライトバリアに
  よる並行・インクリメンタルな印付けは設計（5.5）にとどまり、この版では実装していません。
- **（2026-11 に解消）** ローカル変数・仮引数・一時値・戻り値の入れ物の `own T`・`Option<own T>`、own フィールドを持つ
  構造体と、その配列の中の gc の Id も根になります（5.3.1）。移動・上書き・`.take()`・drop のたびに C の変数を NULL / 0 に
  するので、移動の後も変数が古い own を指したままにはなりません。`tests/ok/gc_local_own_roots.fsc`（5 つの場合と #[on_drop]・
  戻り値）、`gc_shared_own.fsc`（共有の gc_arena・4 コア）、`gc_own_refresh.fsc`（世代リフレッシュ）で確かめています。
- 共有の gc_arena と、ほかのコアの割り込みによるタスクの切り替えが重なる場合は、`bsp_switch` で止まる BSP（QEMU virt AArch64
  と x86_64）だけが対象です。`par()` の読み手を持ったまま割り込まれたタスクは止まらずに切り替わります。どちらも実機では
  確かめていません。
- **（2026-11 に解消）** チャネルで送っている途中の値（own の中も）にある共有の gc_arena の Id も、`gc_collect` がリング
  バッファの公開済みのセルを走査するので根になります。

---

## 6. エコシステム（コンパイラを変えずに有志が足せる基盤）

### 6.1 モジュールとパッケージ

- `import net::tcp;` は 2 パス解析を広げた名前空間の分割です（ライフタイム・ジェネリクスなし）。モジュールごとに並列に型検査し、
  結果をキャッシュします。探す順: このファイルのフォルダ → `fsc_modules/` → `FASCON_PATH` → 標準ライブラリ（埋め込み）。
- `fascon pkg init | new | add | install | list | label | std`。

### 6.2 パッケージの安全性のラベル（自動）

`fascon pkg label`（と `pkg install`・パッケージの中での `fascon build`）が全ソースの字句を走査し、`fascon.toml` の `[package]` に
書きます:

```toml
safety = "Low-Level / Hardware"  # fascon pkg が構文を走査して自動で付ける
safety_reasons = [
    "MMIO access (src/uart.fsc:3)",
]
```

`#[asm]`・`extern "C"`・装置の境界（`mmio`・`port_io`・`bind_mmio`・`dma`・`irq`・`#[trap]`・`#[syscall]`・`user_mem`・
`volatile_bypass`）が 1 つも無ければ **Pure Safe**。`fascon.lock` にも依存ごとのラベルが入り、`fascon pkg std` は標準ライブラリの
ラベルを表示します。

### 6.3 ライブラリを書くためのプリミティブと標準ライブラリ

`#[on_drop]`・`#[no_column]`・`#[stable_abi]`・効果解析（Sleep / atomic / trap）と統合された `#[asm]` で、`lib/` に純粋な .fsc として
`sync::spinlock`・`sync::mutex`（優先度継承）・`collections::vec`・`time::wheel`・`sched::hmp`・`net::wire / udp / tcp`・
`fs::pagecache / cowfs`・`ipc::rpc`・`loader::elf`・`pm::power` を実装しています。

### 6.4 標準 HAL（関数ポインタの構造体）

```fsc
struct Console     { putc: fn(u8), getc: fn() -> Option<u8>, flush: fn() }
struct Timer       { now_ns: fn() -> u64, freq_hz: fn() -> u64, arm_oneshot: fn(u64) -> bool, cancel: fn() }
struct BlockDevice { sectors: fn() -> u64, read: fn(u64, &mut [u8; 512]) -> bool, write: fn(u64, &[u8; 512]) -> bool, flush: fn() -> bool }
struct NetDevice   { mac: fn() -> u64, mtu: fn() -> u32, send: fn(&[u8; 1514], u32) -> bool, recv: fn(&mut [u8; 1514]) -> u32, link_up: fn() -> bool }
```

`import hal::console;` などで使います。ボードのパッケージは同じ名前の `board_hal.fsc`（`fn console() -> console::Console`）を
持つので、アプリは `import board_hal;` のままボードを替えられます（`tests/board/hal_hello.fsc` が Raspberry Pi 3 と QEMU virt の
両方で同じソースのまま動く）。

### 6.5 宣言的なエラー辞書・修正規則

| ファイル | 中身 |
|---|---|
| `data/dictionary.json` | 用語・エラーコードの解説（日英） |
| `data/rules.json` | `fascon explain` のタイムラインの数学的な説明 |
| `data/fixes.json` | `fascon fix` の修正規則（`wrap_statement`・`replace_in_span`・`replace_in_line`・`prefix_line_token`） |

上書きの場所（環境変数 `FASCON_DATA` のフォルダ・実行ファイルの横の `data/`）に同じ名前のファイルを置けば、コンパイラを作り直さずに翻訳・修正規則を足せます。
辞書を変えたら `python3 tools/sync_dictionary.py` で VS Code 拡張の辞書へ写し、`python3 tools/check_dictionary.py` で確かめます
（コンパイラが出しうるエラー番号がすべて載っているか、日英がそろうか、拡張と同じ文か）。どちらも `tests/run_tests.py` に入っています。
例: `data/fixes.json` の F2621 の規則は `nodes.free(a)` を `gc_collect(nodes)` に置き換え、直した結果をコンパイルして
エラーが消えることを確かめてから適用します。

### 6.6 自動リフレーミングの範囲の凍結

`#![reframing(rust)]` は既存の Rust 風のコードを移すための入り口として、現在の変換の範囲で凍結します（広げない）。
変換したコードは `--emit-strict-fascon` で純粋な Fascon 1.0 の構文（`.strict.fsc`）へ書き出して定着させる方針です。

---

## 7. 実機の基盤（board.toml・汎用 BSP・キャッシュの一貫性）

### 7.1 board.toml

```toml
[board]
name = "raspberrypi3b"
arch = "aarch64"          # riscv64 などは [toolchain] と自前の start.S / bsp.c で
cpus = 4

[memory]
ram_base = 0x0000_0000
ram_size = 0x3f00_0000

[uart]
kind = "pl011"            # pl011 / ns16550（shift = レジスタの間隔）
base = 0x3f20_1000

[timer]
hz = 19_200_000           # CNTFRQ が 0 のときだけ

[boot]
load_addr = 0x8_0000
stack_size = 0x1_0000     # コアごと
smp = "spin-table"        # none / spin-table / psci-hvc / psci-smc / ipi
release_addrs = [0xe0, 0xe8, 0xf0]
exit = "loop"             # loop / psci / semihosting
entry_el = "el2"

[dma]
coherent = false          # false → キャッシュラインごとの掃き出し・無効化
cache_line = 64
bus_offset = 0xc000_0000  # 装置から見た番地（bus_addr）

[qemu]
machine = "raspi3b"       # fascon qemu --board で試すとき
```

`fascon build --board raspberrypi3b app.fsc` → `app.raspberrypi3b.elf` と、フラッシュ・SD カードへそのまま書ける生の
`app.raspberrypi3b.img`（objcopy -O binary と同じものを自前の ELF の読み取りで作る）。`fascon qemu --board <名前>` は
`[qemu] machine` で起動します（終わりだけセミホスティング）。`--board` には board.toml の道か名前を渡し、名前は
`boards/<名前>/board.toml`・`boards/<名前>.toml` を、今のフォルダ・ソースのフォルダとその上・`FASCON_BOARDS`・実行ファイルの
近くから探します。ボードのパッケージのフォルダは `import` の検索先に入ります。

### 7.2 汎用 AArch64 BSP（`bsp/generic-aarch64/`）

起動時の例外レベルを見て EL1 へ降ります（ファームウェアの有無を問わない）:

```asm
_start:
    mrs     x19, mpidr_el1
    and     x19, x19, #0xff
    mrs     x0, CurrentEL
    lsr     x0, x0, #2
    cmp     x0, #3
    b.ne    1f
    mov     x0, #((1 << 10) | (1 << 0))   /* SCR_EL3: RW | NS */
    msr     scr_el3, x0
    ...
1:  cmp     x0, #2
    b.ne    el1_entry
    mov     x0, #(1 << 31)                /* HCR_EL2.RW = 1 */
    msr     hcr_el2, x0
    mrs     x0, cnthctl_el2
    orr     x0, x0, #3                    /* EL1 から物理カウンタ・タイマー */
    msr     cnthctl_el2, x0
    ...
    eret
```

コアごとのスタック、MMU（[memory] の RAM = Normal WB、ほか = Device の恒等写像）、TLS、コンソール（pl011 / ns16550）、
汎用タイマー、終わり方を board.toml から作ります。割り込みコントローラ・タスク・EL0 はこの雛形には無く、呼ぶと理由を表示して
止まります（qemu-virt-aarch64 の BSP を手本に足す）。

### 7.3 2 番目以降のコアの起こし方（標準のブートフック）

| `smp` | 動作 |
|---|---|
| `spin-table` | `release_addrs` の各番地へ `_start` を書き、`dc civac` で PoC まで掃き出して `sev`（Raspberry Pi のファームウェアの方式） |
| `psci-hvc` / `psci-smc` | `CPU_ON(コア, _start, 0)` を HVC / SMC で |
| `ipi` | RISC-V: CLINT の msip に 1 を書いて WFI から起こす（ボードのパッケージの bsp.c） |
| `none` | 1 コア |

起きたコアの数は `cpu_count()` で返ります（答えなかったコアは数えない）。

### 7.4 非一貫の DMA と bus_addr

`[dma] coherent = false` のとき、`fascon_plat_dma_sync` が `submit_dma` の前にキャッシュラインごとに `dc cvac`（PoC まで掃き出し）、
`complete_dma` の後に `dc civac`（掃き出し + 無効化。端の半端なラインの汚れも失わない）を行います（RISC-V は Zicbom の `cbo.clean` /
`cbo.flush`）。`bus_offset` があれば `submit_dma` が返す IOVA が装置から見た番地になり、`complete_dma` はそれを元へ戻します。
`dma own` の所有権の規則により、装置に貸している間は Fascon のコードからバッファに触れません。

Raspberry Pi 3 のメールボックス（`boards/raspberrypi3b/rpi_mailbox.fsc`、純粋な Fascon）:

```fsc
fn bus_addr(buf: dma own [u32; 36]) -> u64 {
    submit_dma(buf)                       // キャッシュを掃き出し、0xC000_0000 の別名の番地を返す
}

fn mbox_call(msg: dma own [u32; 36]) -> Option<dma own [u32; 36]> {
    let mb = bind_mmio(Mbox, 0x3f00_b880);
    let bus = bus_addr(msg);
    while (mb.status & MBOX_FULL) != 0 { cpu_relax(); }
    mb.write = ((bus as u32) & 0xffff_fff0) | PROPERTY;
    loop { while (mb.status & MBOX_EMPTY) != 0 { cpu_relax(); }  if (mb.read & 0xf) == PROPERTY { break; } }
    complete_dma([u32; 36], bus)          // 無効化して所有権を取り戻す
}
```

QEMU の raspi3b での実際の出力:

```
[board] Fascon 1.0 on raspberrypi3b: image 0x0000000000080000..0x0000000000a1c000, EL1, timer 62500000 Hz, DMA non-coherent (cache line 64 B)
[board] SMP: 4 core(s) online (spin-table)
2026
board revision 0xa02082
ARM memory MiB 960
squares from 3 cores 50
FASCON-RPI3-OK
```

### 7.5 同梱のボードのパッケージ

| フォルダ | 内容 | 確認 |
|---|---|---|
| `boards/raspberrypi3b/` | board.toml・board_hal.fsc（PL011）・rpi_mailbox.fsc（VideoCore のメールボックス） | QEMU raspi3b で起動（4 コア・メールボックス） |
| `boards/qemu-virt-generic/` | board.toml（PSCI）・board_hal.fsc | QEMU virt で起動（4 コア） |
| `boards/qemu-virt-riscv64/` | board.toml（[toolchain]）・start.S・bsp.c（M モード・IPI）・board_hal.fsc（ns16550） | QEMU の virt で 4 ハートの起動を確認（`FASCON-BOARD-OK`。実機の基板は未確認） |
| `boards/raspberrypi4.toml` ほか | 装置の窓だけの定義（qemu-virt BSP 用） | — |

x86_64 は既存の Fascon の仮想 BIOS（`fascon qemu --target x86_64`）で起動します。x86_64・Cortex-M などのボードは、パッケージに
`start.S`・`bsp.c`・`[toolchain]` を置けばコンパイラを変えずに足せます（この版に同梱の見本は AArch64 と RISC-V）。

---

## 8. エラーコード（ユニバーサル OS 拡張）

| コード | 内容 |
|---|---|
| F2621〜F2625 | fascon-gc（5.6） |
| F3001 | `--target rtos` の再帰 |
| F3010（警告） / F3011 | generic-os の生の Id の幅 / generic-os 専用の操作 |
| F3101〜F3106 | ユーザーメモリ・システムコール・Result と `?`・catch_fault |
| F3201〜F3204 | atomic の形・rcu_read の中の書き込み・Sleep 効果・#[trap] の形 |
| F3301〜F3304 | #[secret]: 出力・比較・asm のオペランド・ct_eq |
| F3401〜F3404 | DMA と MMIO |
| F3501 / F3502 / F3505 | #[stable_abi] の Id / #[stable_abi] の型 / #[on_drop] の関数の形 |
| F3503 / F3504（警告） | #[asm] / 知らない属性 |
| F3601 | extern "C" を渡れない型 |
| F3701〜F3703 | モジュール（見つからない・エラー・別名の衝突） |
| F3801 | board.toml の誤り |

解説は `fascon dict F2621` や `fascon explain <file>`、VS Code の辞書で引けます。

---

## 9. 実装の差分（コンパイラ・ランタイム）

| ファイル | 変更 |
|---|---|
| `src/parser.rs`・`ast.rs`・`types.rs`・`lexer.rs`・`prescan.rs`・`modules.rs`・`strict.rs` | `gc_arena` / `shared gc_arena`（文脈依存のキーワード） |
| `src/sema.rs` | `gc_collect` / `promote_to_gc` の検査、F2621、自動昇格（`check_expr` で期待の型が gc の Id のとき） |
| `src/universal.rs` | F2622〜F2625、F3501（#[stable_abi] の Id）、`ty_contains`、`GC_OFF` |
| `src/codegen_gc.rs`（新規） | 型記述子・ヒープ・根の関数・枠と根の登録・安全点・Id 専用の射影列・昇格の関数 |
| `src/codegen.rs` | 2 回の生成で枠の有無を決める（ラッチと同じ仕組み）、`emit` の後ろで根を登録、generic-os のタスクスタックの区分 |
| `runtime/fascon2_rt.h` | `FASCON2_GC` の節（印付け・掃除・リフレッシュ・待ち合わせ）、非一貫 DMA のキャッシュ操作、bus_offset。2026-11: 入れ子の回収の禁止・印付けの間の割り込みの停止・鎖を 1 回だけ歩く・`F20_GC_POLL_ENTRY`・原子的な枠の先頭と生存ビットマップの読み書き・`FASCON2_GC_ALIAS_CHECK` |
| `src/codegen_gc.rs`・`src/codegen.rs`（2026-11） | own の先まで辿る根（`gc_root_ty`・`gc_root_desc`）、`fsc_own_drop_at_*`（持ち主を NULL にしてから解放）、一時値の受け渡し（`gc_hand_over`）、戻り値の入れ物の根、解放中の根（`gc_drop_temp`）、共有のヒープの記述の 1 回だけの初期化 |
| `bsp/qemu-virt-aarch64/bsp.c`・`bsp/qemu-x86_64-bios/bsp.c`（2026-11） | 共有の gc_arena の回収中のタスクの切り替えで止まる（`FASCON2_GC` と `FASCON2_SMP` のときだけ） |
| `src/board.rs`・`src/qemu.rs`・`src/main.rs` | [boot] / [dma] / [qemu] / [toolchain]、リンカスクリプトの生成、ELF → 生の像、`build --board`・`qemu --board`、ボードの名前の解決 |
| `src/pkg.rs` | 安全性のラベル（`classify`・`scan_text`・`label_manifest`） |
| `bsp/qemu-virt-aarch64/genos.c`（新規）・`start.S`・`bsp.c`・`kernel.ld` | generic-os の BSP（4 章）、タスクごとのシャドウスタックの保存 |
| `bsp/generic-aarch64/`（新規） | 汎用 BSP（7 章） |
| `lib/hal/`（新規）・`boards/`（新規） | 標準 HAL・ボードのパッケージ |

自動昇格の検査（`sema.rs`）の要点:

```rust
if let Some((g, opt)) = want.filter(|(g, _)| self.t.arenas[*g].gc) {
    let r = self.resolve(&t);
    ...
    if self.t.arenas[b].bump && self.t.arenas[b].elem == self.t.arenas[g].elem {
        self.auto_promote.insert(e.id, (g, b));   // 生成では fsc_gc_promote_<g>_<b>(..) で包む
        t = x.clone();
    }
}
```

---

## 10. 検証

| 対象 | 結果 |
|---|---|
| 既存のプログラムの生成 C（42 本 × 日英を元の 1.0.0 の実行ファイルと比較） | 84 ファイルすべてバイト単位で一致 |
| `tests/run_tests.py --qemu` | 全件合格（GC: gc_basic・gc_own_promote・gc_refresh・gc_shared・gc_auto_promote、エラー: F2621〜F2625・F3501・F3505、HAL: lib_hal、QEMU: genos_qemu・genos_guard・GC 3 本・ボード 3 本） |
| generic-os の QEMU | PAN・PAC・BTI on、W^X の共有ページ 0、例外表・PAN の自己試験、SGI による TLB の撃ち落とし（served 1）、守りページでのスタック溢れの検出（終了コード 70） |
| ボード | raspi3b（スピンテーブル・メールボックス・非一貫 DMA）と QEMU virt（PSCI）で同じアプリが 4 コアで動く |
| ローカルの own の根（2026-11） | `tests/run_tests.py --qemu` 402 件すべて合格。`gc_local_own_roots`・`gc_shared_own`（QEMU virt AArch64 `-smp 4`・x86_64 の仮想 BIOS `-smp 4`・PC）・`gc_own_refresh`（PC）。gc のテスト 9 本を ASan + UBSan + LeakSanitizer + 別名の検査で、共有の gc_arena の 2 本を ThreadSanitizer で（データ競合なし）。gc_arena を使わないプログラム 183 本 × 3 モードの生成 C は変わらない（`tools/compare_c.py`） |

---

## 11. 未実装・未確認（正直な一覧）

- 共有の gc_arena の **並行・インクリメンタルな印付け（ライトバリア）** は未実装（停止は触れているコアだけの短い待ち合わせ）。
- 実機（Raspberry Pi 3 / 4 の基板そのもの）での起動は確かめていません（QEMU の raspi3b で確認）。RISC-V のボードのパッケージは
  QEMU の virt で 4 ハートの起動まで確認しました（実機は未確認）。
- 汎用 BSP は割り込みコントローラ・タスク・EL0 を持ちません（qemu-virt-aarch64 と qemu-x86_64-bios の BSP にはある）。
- generic-os のプロセスの表（`f20_aspace`）は所有権の表と TLB の撃ち落としまでで、実のページ表にはつながっていません。
  EL0 / Ring 3 のタスクのアドレス空間は、2026-11 から ASID / CR3 ごとの実のページ表です。
- macOS・Windows 用の実行ファイルはクロスコンパイルしたものです。macOS 版は実行しておらず、Windows 版は Wine でだけ動かしました（Linux x64 は実行確認、Linux arm64 は qemu-user で確認）。
