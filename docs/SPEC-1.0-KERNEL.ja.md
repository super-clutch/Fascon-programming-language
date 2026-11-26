# Fascon 1.0 カーネル拡張 設計仕様書 — プリエンプション・永続ストレージ・ネットワーク・ユーザー空間

2026-11-13 · Fascon 1.0（版の名前は 1.0 のまま、配布物は 1.0.0）· 状態: **Step 1 は実装済み・検証済み。Step 2〜4 は設計確定案（未実装）**

基本仕様: [SPEC-1.0.ja.md](SPEC-1.0.ja.md) · 拡張仕様: [SPEC-1.0-EXT.ja.md](SPEC-1.0-EXT.ja.md) · 構文イメージ: [kernel-design/](kernel-design/)

この文書は、Fascon 1.0 を次の 4 ステップで汎用 OS カーネルへ広げるための、言語仕様・効果解析・コンパイラ・BSP の設計です。
実装に入る前に 4 ステップ分をまとめて決め、後のステップで前のステップの構文や解析を作り直さないことを目的にしています。

| ステップ | 内容 |
| --- | --- |
| Step 1 | プリエンプティブ切り替え（GICv2 + Generic Timer、コンテキストスイッチ、IRQ とクリティカルセクション） |
| Step 2 | 永続ストレージ（virtio-blk、ブロックキャッシュ、ファイルの読み書き） |
| Step 3 | ネットワーク（virtio-net、ゼロコピーの Ethernet / ARP / IPv4 / ICMP エコー応答） |
| Step 4 | ユーザー空間（EL0）の分離とシステムコール（SVC）境界のメモリ受け渡し |

## 実装状況（2026-11-13）

| 範囲 | 状態 | 確かめたこと |
| --- | --- | --- |
| **Step 1** `critical`・`critical fn`・`irq 名前 at N`・`task_arena`・`start` / `current` / `yield_to` / `resume` / `enter` / `exit`・`timer_*`・`irq_open`・`wait_irq`、F2601・F2602・F2603・F2605、切り替えをまたぐ借用（F2502）、F2601 の Quick Fix、`fascon regions` の文脈の報告 | **実装済み** | PC 実行（仮想の時計）と QEMU virt AArch64（GICv2 + 仮想タイマー INTID 27、800 バイトの保存フレーム、EL1 の svc による自発的な切り替え）で同じ出力。`examples/kernel_preempt.fsc`、`tests/ok/critical.fsc`、`tests/err/f26*.fsc` |
| **Step 2** ワイヤ構造体（`#[wire(le)]` / `#[wire(be)]`）・窓（`&[u8]` / `&mut [u8]`、`window` / `view` / `read` / `copy_from`）・`dma`・`dma_arena`（`Dma<@b>`、`lend` / `reclaim`）・止まらない算術（`wrapping_*` / `checked_*` / `saturating_*`、`min` / `max`）、F2701〜F2703 | **実装済み** | `examples/kernel_storage.fsc`: QEMU の virtio-blk（virtio-mmio v2）で 2 回起動し、1 回目に書いた内容を 2 回目に読めること |
| **Step 3** virtio-net・Ethernet / ARP / IPv4 / ICMP echo（ゼロコピー） | **実装済み**（言語の追加は無い。Step 2 の機能だけで書いてある） | `examples/kernel_net.fsc`: QEMU の virtio-net へ外から ARP 要求と ICMP echo（1400 バイトを含む 3 通）を送り、正しいチェックサムの応答が返ること |
| **Step 4** `user_mem`・`start_user`・`@u.of(id)`・`syscall`（本体あり = カーネル側、本体なし = ユーザー側）・`trap user_fault`・`embed`・`put_bytes`・ユーザープログラムのビルド（`// fascon-abi:` / `--user --abi`）、F2801〜F2804 | **実装済み** | `examples/kernel_user.fsc` + `examples/user_init.fsc`: EL0 のタスク 4 本がタイマーで切り替わること、境界の外を指す窓が本体に届かず -14 になること、ユーザーのパニックと不正な命令でカーネルが止まらないこと |
| **SMP 拡張（Fascon 1.0）** `shared arena`・`chan`・`spawn_on`・`par()` | **実装済み**（別文書 `docs/SPEC-1.0-SMP.ja.md`） | `examples/smp_kernel.fsc`: QEMU の `-smp 4` と PC（スレッド）の両方 |

`docs/kernel-design/` の 4 本（`step1_preempt.fsc`〜`step4_userspace.fsc`）と `step4_init.fsc` は、どれも 1.0 のコンパイラで通ります。
`python3 tests/run_tests.py target/release/fascon --qemu` は 199 件すべて合格です（Linux x64 の上で確認）。

Step 2〜4 の実装が、この文書の設計より狭い点・違う点:

- **`&a[lo..hi]` の切り出し式は未実装**です。窓は `a.window(off, len)` / `a.window_mut(off, len)` で作ります（`Option` で返る）。
- **F2704（デバイスから読んだ値の汚染検査）は未実装**です。`reclaim(生の値)` は実行時に検証されます（貸し出し中のハンドルで
  なければ `None`）。
- **`Dma` を含む型の `clone` の禁止は未実装**です（`Dma` は移動専用なので、いまは `clone` を呼ぶ道がありません）。
- **システムコールの境界の検証は、分岐なしラッチではなくふつうの検査**です。入口のスタブが (番地, 長さ) をそのタスクのユーザー
  メモリと比べ、外れていれば本体を呼ばずに -14 を返します。`&mut` の窓がほかの窓と重なる呼び出しも -14 です。
- **（2026-11 に変更）アドレス空間はタスクごと**で、AArch64 は ASID（16 ビット）、x86_64 は CR3（PCID があれば付ける）を
  切り替えるだけで TLB を掃きません。ユーザーメモリは 4 KiB のページで、初めて触れたときに写します。`user_mem` の大きさと
  番地は 4 KiB の倍数で、番地は恒等マップ（0〜0x8000_0000）の外、0x80_0000_0000 未満、全体が 1 GiB の区画 1 つに収まる
  必要があります（F2803）。詳しくは [SPEC-1.0-INTEGRATION.ja.md](SPEC-1.0-INTEGRATION.ja.md)。
- **1 コアのプログラムでは、カーネルはユーザーの仮想番地を一度も読みません**。システムコールの窓と `@u.of(id)` は、恒等
  マップ側の番地で渡ります（PAN の設定には頼っていません）。マルチコアのプログラムでは、syscall の入口が窓を影バッファへ
  1 回だけ写します（AArch64 は LDTRB / STTRB、x86_64 は SMAP の STAC / CLAC。どちらも例外表つき）。カーネルの RAM の写像には UXN を付けてあります。
- **システムコールの引数はレジスタ 8 本まで**（窓は 2 本）。Id は渡せません（`u32` で受けて `validate` を通す。F2801）。
- **ユーザープログラムの出力と終了は ABI の名前で決まります**。ABI に `write(fd: u32, buf: &[u8])` があれば print 系は fd 1 へ、
  `exit(code)` があれば main の終わりとパニック（終了コード 70）はそこへ流れます。無ければ出力は捨てられ、終わりは未定義命令
  （カーネルの `trap` がそのタスクを終わらせる）になります。
- **`trap` はハンドラが戻るとそのタスクを終わらせます**（再開はしません）。ハンドラが無ければ BSP が 1 行出して終わらせます。
- **ユーザー空間（EL0）は PC 実行では再現しません**。`start_user` は PC ではパニックになります（`fascon qemu` で動かします）。
- **ABI の取り込みは、`syscall` の宣言とワイヤ構造体だけ**です。ワイヤ構造体の配列の長さに定数の名前を使っていると、ユーザー側で
  その定数が見つかりません（数で書いてください）。
- **ディスクとネットワークの装置の位置は固定**です（virtio-mmio のバス 31 = ディスク、30 = ネットワーク）。

Step 1 の実装が、この文書の設計より狭い点・違う点:

- **環境効果の対象はアリーナだけ**（task_arena・bump_arena を含む）。MMIO・ポート・コンソールへの同時アクセスは検査しません
  （メモリ安全には関わりませんが、出力は混ざりえます）。own ヒープの確保と解放はランタイムの中で閉じています。
- **P6（bump_arena の印の交錯、F2604）は未実装**です。共有される bump_arena への個々のアクセスは F2601 で止まりますが、
  呼び出しリージョンそのものを閉じる検査はまだありません。
- **`@t.exit()` の次のタスクはランタイムが選びます**（生きているタスクを添字の順に探す。無ければ終了コード 0 で終了）。
- **スタックのあふれは番兵語**（スタックの底の 8 バイト）を切り替えごとに確かめるだけです。守りページは Step 4 で入れます。
- **割り込みの嵐の検出（I-12 のパニック 0x0011 の回数判定）は未実装**です。タイマーは BSP がハンドラの前にマスクするので、
  `timer_arm` を忘れても嵐にはなりません。ハンドラの無い割り込みはパニック 0x0011 です。
- **PC 実行では、割り込まれうる点は `cpu_relax()`・`wait_irq()`・閉じた区間の出口だけ**です（命令の途中では割り込まれません）。
  Windows の PC 実行はファイバーで書いてありますが、Windows の実機では確かめていません。
- x86_64 の BSP は対象外です。`irq`・`task_arena` を使うソースを `--target x86_64` でリンクすると、プラットフォーム層の関数が
  無いというリンクエラーになります。

守る規則（全ステップ共通）:

1. ライフタイム注釈・unsafe・生ポインタ・Box / Rc・0.1 専用構文（spawn / chan / async / isr）は導入しない。コメントにも一重引用符を書かない。
2. 書くのは宣言だけにし、安全性は効果解析 E(f) とラッチでコンパイラが保証する。
3. 既存の 1.0 のソースは同じ C になる（`tools/compare_c.py`）。Rust 製コンパイラに外部クレートを足さない。

---

## 1. 設計の中心: 4 ステップを 5 つの考えで通す

4 ステップに必要なものは、次の 5 つに集約できます。新しい構文はすべてこの 5 つのどれかの表面です。

| # | 考え | 使うステップ | 表面の構文 |
| --- | --- | --- | --- |
| A | **環境効果** E_env: 「割り込まれうる点では、ほかの実行ルートの効果が起きる」とみなす | 1〜4 | `irq`・`critical`・`task_arena` |
| B | **リージョンの一般化**: E(f) の対象をアリーナ以外（MMIO・ポート・DMA・ユーザーメモリ・ランタイムの隠れた状態）へ広げる | 1〜4 | （書かない。推論） |
| C | **ワイヤ構造体とビュー**: どんなビット列でも正しい値になる型（POD）を、バイト列の上に第二級参照で重ねる | 2・3・4 | `#[wire(le)]`・`.view(T, off)`・`&[u8]` |
| D | **移動で表す貸し出し**: デバイスへ渡したバッファは値として消え、検証付きの `reclaim` でしか戻らない | 2・3 | `dma`・`dma_arena`・`Dma<@a>` |
| E | **信頼できない値は停止させない**: デバイス・ユーザー・回線から来た値は、パニックではなく `Option` かラッチ経由のエラーになる | 2・3・4 | `syscall`・ユーザーラッチ・検査付き算術 |

既存の 1.0 から変えないもの: 第二級参照（F0515）、世代タグ付き Id、own の移動規則、コミット点、列、呼び出しリージョン。
新しい検査はすべて既存の不動点解析（呼び出しグラフ上の最小不動点）の成分を増やす形で入ります。

### 1.1 新しい構文の一覧

すべて**文脈依存キーワード**です（`at`・`at_port` と同じ扱い）。項目の先頭か文の先頭でだけ意味を持ち、
`irq`・`critical`・`dma` などを変数名や関数名に使っている既存のソースはそのまま通ります。

| 構文 | 種類 | ステップ | 意味 |
| --- | --- | --- | --- |
| `irq 名前 at INTID { .. }` | 項目 | 1 | 割り込みハンドラ。GIC の許可・ACK・EOI は BSP が行う |
| `critical { .. }` / `critical fn f(..) { .. }` | ブロック / 関数修飾 | 1 | この区間は割り込まれない（DAIF.I の退避と復帰） |
| `task_arena @t: T[N] stack バイト数;` | 項目 | 1 | タスク表。スロットごとにカーネルスタックと保存文脈を隠れた列として持つアリーナ |
| `@t.start(値, 入口, 引数)`・`@t.current()`・`@t.yield_to(id)`・`@t.resume(id)`・`@t.enter(id)`・`@t.exit()` | メソッド | 1 | タスクの生成・現在のタスク・自発的な切り替え・IRQ 出口での切り替え予約・起動・終了 |
| `timer_freq()`・`timer_now()`・`timer_arm(ticks)`・`irq_open()`・`wait_irq()` | 組み込み | 1 | Generic Timer と割り込み許可 |
| `#[wire(le)]` / `#[wire(be)] struct S { .. }` | 属性 | 2 | 詰め物なし・整列 1・バイト順を固定した POD の構造体 |
| `&[u8]` / `&mut [u8]`、`&a[lo..hi]`、`a.window(off, len)`・`a.window_mut(off, len)`、`s.len()`・`s[i]`・`s == t`・`s.copy_from(t)` | 第二級の型と式 | 2 | 長さ付きの第二級参照（保存できない窓） |
| `x.view(T, off)`・`x.view_mut(T, off)`・`x.read(T, off)` | メソッド | 2 | バイト列の上のワイヤ構造体（参照 / 複製）。`if let` / `match` の対象にだけ書ける |
| `dma @r: [T; N];` | 項目 | 2 | デバイスと共有する RAM（リング）。読み書きは常に volatile。`@r.phys()` が番地を整数で返す |
| `dma_arena @b: T[N];` と `Dma<@b>` | 項目と型 | 2 | DMA バッファの表と、移動専用のハンドル |
| `@b.alloc(v)`・`@b[h]`・`h.index()`・`h.phys()`・`@b.lend(h)`・`@b.reclaim(i)`・`@b.free(h)` | メソッド | 2 | 確保・CPU 所有中のアクセス・デバイスへの貸し出し（h は消える）・検証付きの回収 |
| `x.checked_add(y)`・`wrapping_add`・`saturating_add`（sub / mul も）、`min`・`max`、配列の `==` | 組み込み | 2 | 止まらない算術と比較 |
| `user_mem @u for @t: バイト数 at 仮想番地;` | 項目 | 4 | タスクごとのユーザーメモリ（2 MiB ブロック単位） |
| `@t.start_user(値, 入口番地, スタック番地)`・`@u.of(id)` | メソッド | 4 | EL0 タスクの生成・カーネルから見たユーザーメモリの窓 |
| `syscall 名前 at 番号 (引数) -> i64 { .. }` | 項目 | 4 | システムコール。引数の `&[u8]`・`&mut [u8]`・`&T`（T はワイヤ構造体）は境界で検証済みの窓になる |
| `trap user_fault(esr: u64, far: u64, pc: u64) { .. }` | 項目 | 4 | EL0 の例外（不正アクセスなど）の処理 |
| `static IMG: [u8; N] = embed("app.bin");` | 組み込み | 4 | ユーザープログラムの像の埋め込み |

---

## 2. 効果解析 E(f) の拡張仕様

### 2.1 リージョンの一般化（考え B）

1.0 の効果は「アリーナの集合」の 4 つ組（読む・書く・解放・確保。`src/types.rs` の `Effects`）です。対象を次の集合へ広げます。

```math
\mathcal{R} \;=\; \mathrm{Arena} \cup \mathrm{Bump} \cup \mathrm{Mmio} \cup \mathrm{Port} \cup \mathrm{Dma} \cup \mathrm{DmaArena} \cup \mathrm{UserMem} \cup \{\,@heap,\ @frame,\ @console\,\}
```

`@heap`（own のヒープ）・`@frame`（フレームリージョン）・`@console`（`put_*` の行バッファと UART）は、ソースに名前の無い
**擬似リージョン**です。1.0 ではこれらはランタイムの大域状態で、効果に現れません。割り込みとタスク切り替えが入ると
ここが最初に壊れるので（3.1 節）、効果の対象にします。

```math
E(f) \;=\; (R,\ W,\ F,\ A,\ S) \qquad R, W, F, A \subseteq \mathcal{R},\quad S \in \{0, 1\}
```

S は「切り替えうる」（`yield_to`・`exit`・それらを呼ぶ関数）です。E(f) は今までどおり呼び出しグラフ上の最小不動点で、
関数ポインタを通した呼び出しは候補の和です（`src/callback.rs` をそのまま使う）。

### 2.2 実行ルートと文脈

**実行ルート** ρ は次のどれかです。

| ルート | 入口 | 文脈 |
| --- | --- | --- |
| boot | `main`（`irq_open()` / `@t.enter(..)` より前） | 割り込み禁止 |
| task | `@t.start` に渡された関数（同じ関数を 2 回渡せば 2 つのルート）、`main` の `irq_open()` より後 | 割り込み許可 |
| irq | `irq` 項目の本体 | 割り込み禁止（入れ子の割り込みは無い） |
| syscall | `syscall` / `trap` 項目の本体 | 割り込み許可（そのタスクのカーネルスタックで動く） |

関数ごとに 2 ビットの要約を、ルートから下向きの不動点で求めます。

```math
\mathrm{Open}(f) \iff f \text{ が割り込み許可のまま呼ばれうる} \qquad
\mathrm{InIrq}(f) \iff f \text{ が irq ルートから届く}
```

`critical { .. }` の中と `critical fn` の本体、`irq` の本体は**閉じた区間**です。閉じた区間からの呼び出しは Open を立てません。

### 2.3 環境効果 E_env（考え A）

割り込まれうる点（開いた区間のすべての文の境目と、S = 1 の呼び出し）では、ほかのルートの効果が起きるとみなします。

```math
E_{\mathrm{irq}} = \bigcup_{h \in \mathrm{irq}} E(h) \qquad
E_{\mathrm{task}} = \bigcup_{\rho \in \mathrm{task} \cup \mathrm{syscall}} E(\rho)
```

```math
E_{\mathrm{env}}(p) =
\begin{cases}
\emptyset & p \text{ が閉じた区間の中（S = 0 の文）} \\
E_{\mathrm{irq}} \cup E_{\mathrm{task}} & p \text{ が S = 1 の呼び出し（どの区間でも）} \\
E_{\mathrm{irq}} & p \text{ が開いた区間・協調モード} \\
E_{\mathrm{irq}} \cup E_{\mathrm{task}} & p \text{ が開いた区間・プリエンプティブモード}
\end{cases}
```

**モードはコンパイラが決めます。** どこかの `irq` ハンドラ（から届く関数）が `@t.resume(..)` を呼ぶなら
プリエンプティブモード、呼ばなければ協調モードです。協調モードではタスク同士の切り替えは S = 1 の呼び出しでしか
起きないので、タスク間で共有するアリーナに `critical` は要りません。

この定義から、検査は既存の規則の一般化として出ます。

| 規則 | 内容 | コード |
| --- | --- | --- |
| P1 保護なしアクセス | 開いた区間で、E_env(p) が書く・解放する・確保するリージョン r を読む、または E_env(p) が触るリージョン r を書く・解放する・確保する | F2601 |
| P2 割り込みをまたぐ借用 | `&mut r[_]`・`&r[_]`・ビュー・窓が生きている間に、E_env がその r を触りうる点を通る | F2502（原因に環境を名指し） |
| P3 生存証明の失効 | E_env(p) が r を解放しうる点で、r の Id の生存証明を消す（`st.proven.retain`） | （エラーではない。検査が残る） |
| P4 irq での切り替え | InIrq(f) かつ S = 1 の呼び出し | F2603 |
| P5 resume の場所 | `@t.resume` を irq ルートの外で呼ぶ | F2603 |
| P6 共有できないリージョン | bump_arena が 2 つ以上のルートから触られる（呼び出しリージョンの印が交錯する）。`critical fn` を呼び出しリージョンにすれば可 | F2604 |

読み出しだけを共有するリージョン（どのルートも書かない）は P1 の対象外です。P1 の診断は「どのルートのどの行が
同じリージョンを触るか」をタイムラインで示し、Quick Fix で `critical { .. }` を挿入します。

**なぜ `critical` を自動挿入にしないか。** 1 文ごとの自動マスクでメモリ安全は保てますが、割り込み禁止の区間が
ソースから見えなくなり、複数の文にまたがる不変条件（読んでから書く）は守れません。Fascon の流儀（誤りは
コンパイルエラー、直し方は Quick Fix）に合わせ、**共有は推論・保護は明示・書き忘れはエラー**にします。

### 2.4 文脈ごとのコミット点とラッチ

ラッチ `_flt` は関数のローカルなので、割り込みにも切り替えにもそのまま耐えます。ビットを 1 つ足します。

| ビット | 意味 | コミット点での動作 |
| --- | --- | --- |
| `F20_FAULT_STALE`（1） | 古い Id | パニック 0x000D（今までどおり） |
| `F20_FAULT_ARITH`（2） | 算術のあふれ | パニック 0x000B（今までどおり） |
| `F20_FAULT_USER`（4）**新規** | ユーザーメモリの検証失敗 | `syscall` の本体では `-14`（EFAULT）を返す。それ以外の関数では呼び出し元へ伝える（戻り値とは別の隠れた出力引数） |

閉じた区間の出口（`critical` の終わり）はコミット点です。フォールトを持ったまま割り込みを許可しません。

---

## 3. 1.0 に決定的に足りないもの・落とし穴

### 3.1 IRQ 再入とプリエンプション

| # | 落とし穴 | 1.0 の現状 | 対策 |
| --- | --- | --- | --- |
| I-1 | **ランタイムの隠れた大域状態** | own ヒープの空きリスト、フレームリージョンの先頭 `fascon__frame.top`、`put_*` の行バッファは 1 つしか無く、効果にも出ない | 擬似リージョン（2.1）。`fascon_own_alloc` / `free` は O(1) なのでランタイムの中で割り込みを閉じる（`FASCON2_PREEMPT`）。フレームリージョンは**タスクごと**に持ち、切り替えで先頭を入れ替える |
| I-2 | **アリーナの管理情報** | `alloc` / `free` は `link[]`・`live[]`・`gen[]`・`next_unused`・列を複数の格納で更新する | P1 により、共有アリーナへの確保・解放は閉じた区間でしか書けない |
| I-3 | **生存証明の静的な省略** | `if let Some(_) = @a.get(id)` の後は検査を省く | P3。開いた区間では、ほかのルートが解放しうるアリーナの証明は文をまたがない |
| I-4 | **検査と使用の間の割り込み** | 世代の比較とスロットのアクセスは別の命令。own を持つ要素では、割り込みの間に解放されると本物の解放後使用になる | P1 が比較とアクセスを同じ閉じた区間に入れる。これが P1 をメモリ安全の必要条件にしている理由 |
| I-5 | **列（集計の写し）** | 書き込みの後に作り直す | 書き込みと作り直しは同じ文なので、同じ閉じた区間に入る。`for id in @a` の走査も P1 の対象 |
| I-6 | **bump_arena の印** | 呼び出しリージョンの出口で `top` を入口の印へ戻す | タスク A の印より後ろにタスク B が確保すると、A の出口が B の Id を無効にする。P6（F2604） |
| I-7 | **FP / SIMD レジスタ** | コンパイラは NEON を使う C を出す（VRAM の塗りつぶしは `stp q0, q0`）。start.S は FPEN を許可するだけ | 例外入口で q0〜q31・FPSR・FPCR も保存する（1 フレーム 800 バイト）。最初は常に保存し、遅延保存は後の最適化にする |
| I-8 | **カーネルスタックのあふれ** | 守りのページ無し（SPEC の限界 4）。スタックが 1 本なので壊れるのは自分だけだった | タスクごとのスタックでは隣のタスクを壊す。Step 1 はスタックの底の番兵語を切り替えごとに確かめる（パニック 0x0010）。Step 4 で 4 KiB ページの守りページに置き換える |
| I-9 | **割り込みフレームの分のスタック** | 考慮なし | ハンドラは割り込まれたタスクのスタックで動く。`stack` の最小値を「800 バイト + ハンドラの深さ」から決め、小さければ F2605 |
| I-10 | **アイドルでの取りこぼし** | `halt` は止まるだけ | `wait_irq()` は「割り込み禁止のまま wfi、起きてから許可」。条件の確認と待ちの間に割り込みが入っても取りこぼさない |
| I-11 | **待ちに入る直前の起床** | — | `critical` の中から `@t.yield_to` を呼べる。DAIF は保存フレーム（SPSR）に入るので、戻ってきたタスクは自分の閉じた区間の続きから動く |
| I-12 | **レベル割り込みの嵐** | — | virtio の割り込みは、デバイスの InterruptACK に書くまで下がらない。ハンドラが書かずに戻ると EOI の直後に再入する。BSP は同じ INTID が連続 10 万回来たらパニック 0x0011 で止める |
| I-13 | **タイマーの選択** | — | QEMU virt を EL1 で使うなら仮想タイマー（CNTV、INTID 27）。EL2 から降りる経路（start.S）では CNTVOFF_EL2 = 0 と CNTHCTL_EL2 の設定が要る。`-machine virt,gic-version=2` を明示する |
| I-14 | **エンジンの振り分け** | `lexer.rs` の `uses_v2` / `has_v1_only` がファイルを 1.0 / 0.1 のどちらで処理するか決める | 新しい項目だけを使うファイルが 0.1 エンジンへ流れないよう、`irq 名前 at`・`task_arena`・`dma`・`syscall` を 1.0 の印に足す。`#[isr]` は今までどおり F2900 |
| I-15 | **現在のタスクの解放** | — | `@t.free(@t.current())` は書けない（F2605）。終了は `@t.exit()` で、スロットとスタックは次の切り替えの後に返す |

### 3.2 DMA の所有権移動

| # | 落とし穴 | 対策 |
| --- | --- | --- |
| D-1 | **Id はコピーできる**ので「デバイスが持っている間は触れない」を表せない | 移動専用のハンドル `Dma<@b>`。`@b.lend(h)` で h は消え（以後の使用は既存の F0382）、戻す道は `@b.reclaim(index)` だけ |
| D-2 | **デバイスが返す添字と長さは信頼できない** | `reclaim` は「範囲内・貸し出し中」をスロットごとの状態ビットで確かめ、`Option` を返す。二重の完了通知は `None`。長さは `window(off, len)` が `Option` で切る |
| D-3 | **デバイスが書いた中身** | DMA の要素は POD に限る（F2702）: 整数・その配列・ワイヤ構造体。`bool`・`Option`・Id・own・`Str`・関数ポインタは置けない。どんなビット列でも正しい値になる |
| D-4 | **リング（記述子表・avail・used）は両者が同時に触る** | `dma @r` は所有の対象にせず、読み書きを常に volatile + バリアにする（`mmio` と同じ経路）。C コンパイラが 2 回読みを 1 回にまとめたり、検査の後で読み直したりしない |
| D-5 | **番地の取り出し** | `h.phys()`・`@r.phys()` は u64 を返すだけ。Fascon には整数からポインタを作る式が無いので、番地を知っても安全性は変わらない |
| D-6 | **own ヒープのブロックを渡したくなる** | 16 バイトの見出しがあり、解放で再利用される。デバイスが遅れて書くと別の値を壊す。DMA は `.bss.fascon_dma`（4 KiB 境界・スロットは 64 バイト境界）の専用の表からだけ |
| D-7 | **ハンドルを捨てる** | `Dma<@b>` を持つ構造体は own と同じく移動専用で、捨てるとスロットを返す（CPU が持っているので安全）。貸し出し中のスロットはハンドルが無いので解放できない |
| D-8 | **レガシー virtio-mmio** | QEMU virt の既定はレガシー（v1）で、ページ番号と整列の約束が違う。`-global virtio-mmio.force-legacy=false` で v2 に固定する（`src/qemu.rs`） |
| D-9 | **デバイスの並び** | virtio-mmio の 32 個の窓（0x0a00_0000 から 0x200 刻み、INTID 48〜79）のどこに付くかはコマンドラインの順で変わる。`irq .. at N` の N は定数なので、`fascon qemu` が出すコマンドラインを固定し、ドライバは起動時にその窓の MagicValue と DeviceID を確かめ、食い違えば止まる |
| D-10 | **キャッシュと DMA** | QEMU では不要。実機では `lend` / `reclaim` にキャッシュの掃き出しが要る。ランタイムのフック `fascon_dma_sync` を最初から通しておく |
| D-11 | **回線のバイト順と整列** | IPv4 ヘッダはフレームの 14 バイト目（virtio-net ヘッダ込みで 26 バイト目）から始まり、整列していない。ワイヤ構造体の読み書きは必ず `__builtin_memcpy` 形で出す |
| D-12 | **配列の比較が無い** | 1.0 では `[u8; 6] == [u8; 6]` は F0369。MAC アドレスの比較に要るので、整数配列の `==` / `!=` を足す |
| D-13 | **止まらない算術が無い** | 1.0 の算術は検査付きで、あふれるとパニック。チェックサムと、信頼できない長さの計算に `wrapping_*`・`checked_*`・`saturating_*` が要る |

### 3.3 EL0 境界の検証

| # | 落とし穴 | 対策 |
| --- | --- | --- |
| U-1 | **ユーザーの番地をカーネルが保存する**（後で使うと別の中身） | システムコールの引数は第二級参照（`&[u8]` など）としてしか現れない。保存は既存の F0515 / F0520 が拒否する |
| U-2 | **範囲の検証** | コンパイラが作る入口の関数が、(番地, 長さ) がそのタスクのユーザーメモリに収まることを分岐なしで確かめる。外れたら番兵ページへ逸らして `F20_FAULT_USER` を立てる。本体の最初のコミット点で `-14` を返す |
| U-3 | **重なる引数** | `&mut` の窓が別の窓と重なると、`&mut` の排他（F0499 の前提）が崩れる。入口で区間の重なりを調べ、重なれば U-2 と同じ扱い |
| U-4 | **2 回読み** | ユーザーメモリの型は POD だけ。制御に使う値は `buf.read(T, off)` で 1 回だけ複製する。今回は 1 コア・アドレス空間の共有なしなので、カーネルが走っている間にユーザーメモリは変わらない。**スレッド・共有メモリ・SMP を入れるときは、ユーザーの窓を volatile 読みか複製に切り替える**（設計上の予約事項） |
| U-5 | **偽造した Id** | Id は引数にできない（F2801）。u32 で受けて既存の `@a.validate(raw)` を通す |
| U-6 | **ユーザーの値でカーネルが止まる** | パニックはカーネル全体の停止。ユーザーが渡した長さの足し算があふれるだけで止められる。**汚染の検査**（F2704）: ユーザーの窓・ワイヤのビュー・DMA・システムコールの整数引数から来た値を、停止しうる算術・添字・`unwrap` に直接使えない。`checked_add`・`min`・`window`（どれも止まらない）を通す |
| U-7 | **カーネルの値の漏れ** | ワイヤ構造体は詰め物を持てない（F2701）ので、書き出す構造体に未初期化のバイトが無い。EL0 へ初めて入るときは全レジスタを 0 にする |
| U-8 | **ページ表** | 1.0 の BSP は RAM を 1 GiB または 2 MiB のブロックで恒等写像し、実行中に表を変えない。全部 EL1 専用（AP[1] = 0、UXN）のままにし、ユーザーメモリは別の仮想番地（`at 0x1_0000_0000`）の 2 MiB ブロックとして、タスクごとの第 1 段の表に差す。切り替えは TTBR0 と ASID（スロットの添字 + 1）。カーネルを上位半分へ移す必要は無い |
| U-9 | **PAN が無い**（Cortex-A72 は ARMv8.0） | カーネルがユーザーの番地を直接読む式が言語に無いことで代える。ユーザーのブロックは PXN（カーネルは実行しない） |
| U-10 | **EL0 から見えるレジスタ** | CNTKCTL_EL1 でカウンタの読み出しだけ許可。SCTLR_EL1 の UCI / UCT / DZE を落とす。FP は EL0 でも使える（I-7 で保存済み） |
| U-11 | **像を書いた後の命令キャッシュ** | `@t.start_user` が、そのタスクのユーザーメモリ全体に IC IVAU + ISB を行う |
| U-12 | **システムコールの番号** | 表の大きさを 2 の累乗にして添字をマスク。空きは `-38`（ENOSYS）。番号の重なりは F2802 |
| U-13 | **システムコール中のタスクの消滅** | ユーザーの窓は `@u`（UserMem）への借用。その間に `@t.exit()` や別タスクの解放を呼ぶと、E がそのリージョンを解放するので既存の F2502 になる |

### 3.4 そのほかに決めておくこと

- **`enum` が無い。** プロトコル番号や状態は今までどおり `const` と整数で書きます。4 ステップの範囲では足しません。
- **インクルードが無い。** カーネルとユーザープログラムで `syscall` の宣言を共有する手段が要ります。Step 4 で
  `fascon build --user app.fsc --abi kernel.fsc`（カーネルのソースから `syscall` の宣言だけを読む）を足します。新しい構文は増やしません。
- **1 コア。** 4 ステップとも CPU0 だけです。`critical` を「割り込み禁止」で実装できるのはこの前提があるからです。
  SMP では `critical` の実装をロックに替えますが、P1〜P6 の規則と構文は変わりません（閉じた区間の意味が「排他」になるだけ）。
- **パニックは止まるだけ。** タスク単位の巻き戻しはしません（own の解放と閉じた区間の整合が取れないため）。U-6 の検査で、外から来た値がパニックに届かないようにします。

---

## 4. ステップごとの仕様と構文イメージ

### 4.1 Step 1 — プリエンプティブ切り替え

```rust
struct Task { pid: u32, state: u8, vruntime: u64 }

task_arena @task: Task[64] stack 16384;       // スロットごとに 16 KiB のカーネルスタックと保存文脈（隠れた列）

const TICK: u64 = 625000;                     // 62.5 MHz のカウンタで 10 ms

irq tick at 27 {                              // 仮想タイマー（CNTV）。GIC の許可・ACK・EOI は BSP
    timer_arm(TICK);
    let cur = @task.current();                // 割り込まれたタスク。生存は証明済み
    @task[cur].vruntime += 1;
    if let Some(next) = pick_next() {
        @task.resume(next);                   // この割り込みの出口で next へ切り替える（予約するだけ）
    }
}

fn pick_next() -> Option<Id<@task>> {         // 方針は Fascon で書く。1.0 の列の走査がそのまま効く
    let mut best: u64 = 0xffff_ffff_ffff_ffff;
    let mut pick = None;
    for id in @task {
        if @task[id].state == READY {
            if @task[id].vruntime < best { best = @task[id].vruntime; pick = Some(id); }
        }
    }
    pick
}

fn worker(arg: u64) {
    loop {
        critical {                            // @task は irq tick も触る。無ければ F2601
            let me = @task.current();
            @task[me].pid = arg as u32;
        }
        cpu_relax();                          // ここは開いた区間: いつでも切り替わる
    }
}

fn main() {
    let a = @task.start(Task { pid: 1, state: READY, vruntime: 0 }, worker, 1).unwrap();
    let b = @task.start(Task { pid: 2, state: READY, vruntime: 0 }, worker, 2).unwrap();
    timer_arm(TICK);
    @task.enter(a);                           // 割り込みを許可して a へ。戻らない
}
```

**仕組み。** 保存の形は 1 つだけです（`f20_trapframe_t`: x0〜x30・SP_EL0・ELR・SPSR・q0〜q31・FPSR・FPCR、800 バイト）。
IRQ も、自発的な `yield_to`（EL1 からの `svc`）も、Step 4 の EL0 からの SVC と IRQ も、同じ入口で同じフレームを
そのタスクのカーネルスタックに積み、出口は「戻す先のフレームの番地」を受け取って復帰します。切り替えは
「出口で別のタスクのフレームの番地を返す」だけです。

- `task_arena` は普通のアリーナに隠れた列 `ctx[]`（保存したスタックポインタ・スタックのブロック・フレームリージョンの先頭・Step 4 の TTBR0）を足したものです。1.0 の列の仕組み（`F20_DEFINE_TABLE_COLS`）に乗せます。要素の型・Id・世代・`for id in @task`・列の走査は普通のアリーナと同じです。
- コンパイラは `irq` 項目から表 `f20_irq_table[]`（INTID・関数）を出します。1.0 の MMIO リージョン表と同じく BSP が起動時に読み、GICD で許可します。Fascon のソースは GIC に触りません。
- タスクの入口の関数から戻ると `@task.exit()` と同じです。
- 生成 C: `critical { B }` は `{ const uint64_t _im = fascon_irq_save(); B; F20_COMMIT(..); fascon_irq_restore(_im); }`。`break`・`continue`・`return` での出口も、フレームリージョンの巻き戻しと同じ仕組みで復帰を通します。PC 実行（`fascon run`）では入れ子の深さを数えるだけです。

新しいエラー: F2601・F2603・F2604・F2605（`task_arena` の形: スタックの大きさ、現在のタスクの解放、入口の型が `fn(u64)` でない）、F2602（`irq` の形: INTID が 16〜1019 の外、同じ INTID が 2 つ、引数や戻り値がある）。

### 4.2 Step 2 — 永続ストレージ

```rust
#[wire(le)] struct VqDesc { addr: u64, len: u32, flags: u16, next: u16 }     // 16 バイト。詰め物があれば F2701
#[wire(le)] struct BlkReq { kind: u32, reserved: u32, sector: u64, data: [u8; 512], status: u8 }
#[wire(le)] struct SuperBlock { magic: u32, nfiles: u32, root_lba: u64 }

mmio @virtio: [[u32; 128]; 32] at 0x0a00_0000;   // 32 個の窓。起動時に窓 31 の DeviceID = 2 を確かめる
dma @vq_desc: [VqDesc; 16];                      // デバイスと共有するリング: 常に volatile
dma @vq_avail: [u16; 20];
dma @vq_used: [u32; 34];
dma_arena @req: BlkReq[16];                      // 要求 1 つ = ヘッダ + 512 バイト + 状態

critical fn submit_read(lba: u64) -> bool {
    if let Some(h) = @req.alloc(BlkReq { kind: 0, reserved: 0, sector: lba, data: [0; 512], status: 255 }) {
        let i = h.index();
        @vq_desc[i].addr = h.phys();             // 番地は整数として記述子へ
        @vq_desc[i].len = 529;
        @vq_desc[i].flags = 2;
        @req.lend(h);                            // 所有権がデバイスへ移る。h はここで消える（以後の使用は F0382）
        kick(i);
        true
    } else {
        false
    }
}

irq blk_done at 79 {
    ack_device();                                // InterruptACK。書かずに戻ると割り込みの嵐（I-12）
    let raw = next_used();                       // デバイスが書いた添字: 信頼できない
    if let Some(h) = @req.reclaim(raw) {         // 範囲内かつ貸し出し中のときだけハンドルが戻る
        if @req[h].status == 0 {
            if let Some(sb) = @req[h].data.view(SuperBlock, 0) {   // バイト列の上の第二級参照
                mount(sb.magic, sb.root_lba);
            }
        }
        @req.free(h);
    }
}
```

**ワイヤ構造体（考え C）。** `#[wire(le)]` / `#[wire(be)]` の構造体は、フィールドが整数・整数の配列・ワイヤ構造体だけで、
整列 1・詰め物なし・バイト順固定です。C では 1 バイト整列の構造体になり、読み書きは `__builtin_memcpy` とバイト順の変換
（`__builtin_bswap`）で出します。どんなビット列でも正しい値なので、`.view(T, off)` は境界の検査 1 回だけで第二級参照を作れます。

**窓 `&[u8]`。** 長さ付きの第二級参照です。参照なので構造体・戻り値・Option・アリーナに置けません（既存の F0515）。
`&a[lo..hi]` は範囲外でパニック（信頼できる値用）、`a.window(off, len)` は `Option`（信頼できない値用）です。
`s.len()`・`s[i]`・`for i in 0..s.len()`（範囲解析で検査を省く）・`s == t`（長さと中身の比較）・`s.copy_from(t)`（短い方の長さまで写す。止まらない）を持ちます。
リフレーミングの `&[u8]` → `Str` の書き換えは、引数の位置では窓に替えます（`Str` は今までどおり静的なバイト列の値）。

**ブロックキャッシュとファイル。** ここからは新しい言語機能は要りません。キャッシュは
`arena @bcache: Buf[32]`（`struct Buf { lba: u64, dirty: bool, data: Option<Dma<@req>> }`）で、入出力中は
`data` が `None` になります（`.take()` して `lend`）。「入出力中のブロックは読めない」が Option の形で保証されます。
ファイルシステムは固定長の表（スーパーブロック・名前 28 バイト + 開始 LBA + 長さ）を `view` で読むだけの単純な形から始めます。

新しいエラー: F2701（ワイヤ構造体の形）、F2702（DMA の要素が POD でない）、F2703（`Dma<@b>` を bump_arena・static・別の
`dma_arena` に置く、別の表のハンドルを渡す）、F2704（汚染された値を停止しうる操作へ。最初は警告で入れ、Step 4 でエラーにする）。

### 4.3 Step 3 — ネットワーク

```rust
#[wire(be)] struct Eth  { dst: [u8; 6], src: [u8; 6], kind: u16 }
#[wire(be)] struct Ipv4 { vihl: u8, tos: u8, len: u16, id: u16, frag: u16, ttl: u8, proto: u8, csum: u16, src: u32, dst: u32 }
#[wire(be)] struct Icmp { kind: u8, code: u8, csum: u16, id: u16, seq: u16 }
#[wire(le)] struct Frame { vhdr: [u8; 12], data: [u8; 1514] }    // virtio-net ヘッダ + Ethernet フレーム

dma_arena @pkt: Frame[64];                       // 受信と送信で同じ表を使う = ゼロコピーで付け替えられる

fn on_frame(h: Dma<@pkt>, len: u32) {            // 受信リングから回収したハンドルを値で受け取る
    let mut reply = false;
    if let Some(eth) = @pkt[h].data.view_mut(Eth, 0) {
        if eth.kind == 0x0800 {
            let me = eth.dst;
            eth.dst = eth.src;                   // その場で書き換える
            eth.src = me;
            reply = true;
        }
    }
    if reply {
        reply = echo_reply(&mut @pkt[h].data, len);   // IPv4 と ICMP を同じバッファの上で書き換え、チェックサムを直す
    }
    if reply {
        tx_submit(h, len);                       // 同じハンドルを送信リングへ貸す。複製は 1 バイトも無い
    } else {
        rx_refill(h);                            // 受信リングへ戻す
    }
}

fn csum16(s: &[u8]) -> u16 {                     // 1 の補数和。wrapping で止まらない
    let mut sum: u32 = 0;
    let mut i = 0;
    while i + 1 < s.len() {
        sum = sum.wrapping_add(((s[i] as u32) << 8) | (s[i + 1] as u32));
        i += 2;
    }
    while (sum >> 16) != 0 { sum = (sum & 0xffff) + (sum >> 16); }
    (sum ^ 0xffff) as u16
}
```

Step 3 は Step 2 の機能（ワイヤ構造体・ビュー・窓・`dma_arena`・止まらない算術）だけで書けます。**新しい言語機能はありません。**
これが Step 2 で考え C・D・E を先に入れる理由です。足すのは `src/qemu.rs` の起動オプションとテストの相手だけです。

- テストは root 権限なしで動く形にします: `-netdev socket,id=n0,udp=127.0.0.1:P1,localaddr=127.0.0.1:P2` と `-device virtio-net-device,netdev=n0`。
  `tests/net_peer.py` が UDP で生の Ethernet フレーム（ARP 要求・ICMP エコー要求）を送り、応答を確かめます。Windows・Linux・macOS で同じです。
- virtio-net は VIRTIO_F_VERSION_1 だけを受け入れ、MRG_RXBUF・チェックサムのオフロードは断ります（ヘッダは常に 12 バイト）。

### 4.4 Step 4 — ユーザー空間とシステムコール

```rust
user_mem @umem for @task: 0x20_0000 at 0x1_0000_0000;     // タスクごとに 2 MiB。どのタスクからも同じ仮想番地に見える

static INIT: [u8; 4096] = embed("init.bin");               // fascon build --user で作った像

#[wire(le)] struct Stat { size: u64, blocks: u32, kind: u32 }

syscall write at 1 (fd: u32, buf: &[u8]) -> i64 {          // ユーザーは (fd, 番地, 長さ) を渡す。buf は検証済みの窓
    if fd != 1 { return -9; }
    for i in 0..buf.len() { putc(buf[i]); }
    buf.len() as i64
}

syscall read at 0 (fd: u32, buf: &mut [u8]) -> i64 {
    match file_of(fd) {                                    // u32 を validate して Option<Id<@file>> に
        Some(f) => read_file(f, buf),                      // 窓をそのまま渡せる（第二級参照）。保存は F0515
        None => -9,
    }
}

syscall stat at 4 (fd: u32, out: &mut Stat) -> i64 {       // ワイヤ構造体への参照も境界で検証される
    out.size = 0;
    out.blocks = 0;
    out.kind = 1;
    0
}

trap user_fault(esr: u64, far: u64, pc: u64) {
    @task.exit();                                          // 不正アクセスしたタスクだけを終わらせる
}

fn main() {
    let t = @task.start_user(Task { pid: 1, state: READY, vruntime: 0 }, 0x1_0000_0000, 0x1_0020_0000).unwrap();
    if let Some(m) = @umem.of(t) {                         // カーネルから見たそのタスクのユーザーメモリ（&mut [u8]）
        m.copy_from(&INIT);
    }
    @task.enter(t);
}
```

**境界の規則。**

| 引数の型 | レジスタ | 入口での検証 |
| --- | --- | --- |
| 整数（u8〜u64・i8〜i64） | 1 本 | 幅へ切り詰め。汚染された値として扱う |
| `&[u8]` / `&mut [u8]` | 2 本（番地・長さ） | 区間がユーザーメモリの中、ほかの `&mut` と重ならない |
| `&T` / `&mut T`（T はワイヤ構造体） | 1 本（番地） | 番地から sizeof(T) がユーザーメモリの中 |
| Id・own・`Dma`・`Str`・関数ポインタ・`bool`・ワイヤでない構造体 | — | F2801 |

レジスタは x0〜x7 の 8 本まで、番号は x8、戻り値は x0（i64）です。検証は分岐なしで、失敗は番兵ページとユーザーラッチへ流し、
本体の最初のコミット点が `-14` を返します（2.4）。ソースに検証のコードは 1 行も現れません。

**ユーザー側。** `fascon build --user app.fsc --abi kernel.fsc` は、カーネルのソースから `syscall` の宣言だけを読み、
`write(1, "hello")` のような呼び出しを `svc` に変えます。宣言は 1 か所なので、番号と引数の食い違いは起きません。
ユーザー側では `mmio`・`port_io`・`irq`・`critical`・`dma`・`task_arena` は使えません（F2803）。アリーナ・own・bump は使えます
（ユーザーメモリの中に置かれる）。

新しいエラー: F2801（引数の型）、F2802（番号の重なり・0〜255 の外・9 本以上のレジスタ）、F2803（ユーザー側で使えない項目、
`user_mem` の形: 4 KiB の倍数でない・カーネルと重なる。2026-11 に 2 MiB から緩めた）。x86_64 では、番号は rax、引数は
rdi・rsi・rdx・r10・r8・r9・r12・r13、戻り値は rax（`int $0x80`）です。

---

## 5. 実装ブループリント

### 5.1 順番の原則

1. **フェーズ 0 で土台だけを入れ、生成 C を 1 文字も変えない。** リージョンの一般化（考え B）は既存の全検査の下を通る変更なので、新しい構文より前に単独で入れ、`tools/compare_c.py` で確かめます。
2. **どのフェーズも PC（`fascon run`）で試せる形を先に作る。** `critical` は深さの計数、`irq` は「テストが関数として呼ぶ」、`dma` は普通の RAM、`syscall` は検証関数を直接呼ぶ。QEMU はその後です。解析の誤りと BSP の誤りを分けて見つけるためです。
3. **解析（flow.rs）を BSP より先に。** P1〜P6 の検査を先に入れてエラーのテストを固めてから、実際に割り込みを許可します。
4. **1 フェーズ = 1 つの考え。** 各フェーズは単独でテストが全件通る状態で終わります。

### 5.2 フェーズ表

| フェーズ | 内容 | 主なファイル | 完了の条件 |
| --- | --- | --- | --- |
| **0a** | `Effects` の対象を `RegionId`（アリーナ / bump / MMIO / ポート / 擬似）へ。E に S を足す（常に 0） | types.rs・flow.rs・sema.rs・callback.rs | 既存 107 件合格、生成 C が同一 |
| **0b** | 文脈依存キーワードの受け口、エンジン振り分け（I-14）、新しい項目の事前登録 | lexer.rs・prescan.rs・parser.rs・ast.rs | 新しい項目を「未対応」と診断できる。既存は同一 |
| **0c** | 止まらない算術（`wrapping_*`・`checked_*`・`saturating_*`・`min`・`max`）、整数配列の `==` | sema.rs・codegen.rs・fascon2_rt.h・strict.rs・dict.rs | tests/ok/arith_nostop.fsc、往復（厳格形）合格 |
| **0d** | ランタイムの `FASCON2_PREEMPT`: `fascon_irq_save` / `restore`、ヒープの O(1) 操作を閉じる、フレームリージョンを構造体 1 つにまとめて差し替え可能に | fascon2_rt.h・bsp/host | PC では無動作。既存の ASan / UBSan 合格 |
| **1a** | `critical { }`・`critical fn`（構文・型・厳格形・生成 C・すべての出口） | parser.rs・ast.rs・sema.rs・flow.rs・codegen.rs・strict.rs・resolve.rs・escape.rs | PC で入れ子と早期 return のテスト |
| **1b** | `irq` 項目、`f20_irq_table[]`、`timer_*`・`irq_open`・`wait_irq`。BSP: 例外入口と 800 バイトのフレーム、GICv2 の初期化とディスパッチ、仮想タイマー | parser.rs・sema.rs・codegen.rs・start.S・bsp.c・fascon2_rt.h・qemu.rs | QEMU: タスクなしで tick を 100 回数えて終了 |
| **1c** | 実行ルート・Open / InIrq・E_env・P1〜P5（F2601・F2603、F2502 の環境の名指し、証明の失効） | flow.rs（新しいモジュール `context.rs` に分ける）・driver.rs（`fascon regions` に共有の表） | tests/err に 8 件以上。1b のテストに `critical` が無ければ F2601 |
| **1d** | `task_arena`・`start`・`current`・`yield_to`・`enter`・`exit`（協調モード）。隠れた列 `ctx[]`、タスクごとのスタックとフレームリージョン、番兵語 | sema.rs・codegen.rs・fascon2_rt.h・start.S（EL1 の svc 経路）・bsp.c | QEMU: 2 タスクが `yield_to` で交互に 1000 回 |
| **1e** | `resume`（プリエンプティブモード）、モードの判定、P6（F2604） | flow.rs / context.rs・codegen.rs・bsp.c | QEMU: 譲らない 2 タスクがタイマーで切り替わる。kernel_core をタスク化した例 |
| **2a** | `#[wire]`・窓 `&[u8]`・`view` / `view_mut` / `read` / `window` / `copy_from` | parser.rs・types.rs・sema.rs・flow.rs・escape.rs・codegen.rs・strict.rs・reframe.rs | PC: バイト列の解析のテスト、F2701、窓の保存が F0515 |
| **2b** | `dma`・`dma_arena`・`Dma<@b>`（own の移動検査を再利用）・`lend` / `reclaim` の状態ビット | types.rs・sema.rs・flow.rs・codegen.rs・fascon2_rt.h・kernel.ld（`.fascon_dma`） | PC: 偽のデバイス（テストが `reclaim` に不正な添字を渡す）で `None`。F2702・F2703・F0382 |
| **2c** | virtio-mmio v2 + virtio-blk の例（最初はポーリング、次に IRQ）。`fascon qemu --disk 像` | examples/・qemu.rs・tests/ | QEMU: セクタを書いて再起動後に読める |
| **2d** | ブロックキャッシュ・単純なファイルシステムの例、汚染の検査 F2704（警告） | examples/・flow.rs | QEMU: ファイルの作成・読み書き・再起動後の確認 |
| **3a** | `fascon qemu --net udp:P1:P2`、`tests/net_peer.py` | qemu.rs・tests/ | 相手のスクリプトだけで往復を確認 |
| **3b** | virtio-net の例（受信の補充・送信の回収）、Ethernet / ARP / IPv4 / ICMP | examples/ | QEMU: ARP 応答と ICMP エコー応答 100 回、複製 0 回（`fascon regions` で確認） |
| **4a** | BSP: タスクごとの第 1 段の表と ASID、ユーザーメモリの置き場、Lower EL のベクタ、EL0 への復帰、守りページ（I-8 の置き換え） | bsp.c・start.S・kernel.ld | QEMU: 手書きの数命令の EL0 コードが svc で戻る |
| **4b** | `user_mem`・`start_user`・`@u.of`・`embed` | parser.rs・sema.rs・codegen.rs・driver.rs | PC: ユーザーメモリを普通の配列で真似る |
| **4c** | `syscall`・`trap`、入口の関数の生成、検証とユーザーラッチ、F2801・F2802、F2704 をエラーに | parser.rs・sema.rs・flow.rs・codegen.rs・fascon2_rt.h | PC: 範囲外・重なり・番号外のテスト。QEMU: write / read |
| **4d** | `fascon build --user --abi`、ユーザー用の起動コードとリンカスクリプト、F2803 | driver.rs・main.rs・target.rs・bsp/user-aarch64/（新規） | QEMU: Fascon で書いた init が Step 2 のファイルを読み、Step 3 の ping に応答しながら動く |

各フェーズで合わせて更新するもの: `src/dict.rs`（辞書）・`src/lsp.rs`（補完とホバー）・`src/strict.rs`（厳格形の往復）・
`docs/`（日英）・`vscode-extension/`（構文の色分け・スニペット・辞書・例）・`tests/run_tests.py`。

### 5.3 ファイルごとの変更の粒度

**コンパイラ**

| ファイル | 変更 |
| --- | --- |
| `src/lexer.rs` | 新しいトークンは作らない（文脈依存。`..` は既存）。`uses_v2` に新しい項目の形を足す |
| `src/prescan.rs` | `irq`・`task_arena`・`dma`・`dma_arena`・`user_mem`・`syscall`・`trap` の名前の登録 |
| `src/ast.rs` | `Item::Irq`・`Item::Syscall`・`Item::Trap`・`Item::Dma`・`Item::UserMem`、`ArenaDef` に種類（普通 / task / dma）と `stack`、`FnDef` に `critical`、`ExprKind::Critical(Block)`・`ExprKind::Slice`、`TypeExpr::Slice` |
| `src/parser.rs` | 上の項目と式。`critical {` は文の先頭でだけ認識する（構造体の名前は大文字始まりなので曖昧にならない） |
| `src/types.rs` | `RegionId`、`Effects`（R / W / F / A を RegionId に、S）、`Ty::Slice`・`Ty::Dma`、構造体の `wire` とバイト順、POD の判定 |
| `src/sema.rs` | 項目の宣言と形の検査（F2602・F2605・F2701・F2702・F2801・F2802）、新しいメソッド（`check_arena_method` に task / dma の枝）、窓とビュー（`scrut_ok` の仕組みを再利用）、止まらない算術 |
| `src/context.rs`（新規） | 実行ルートの列挙、Open / InIrq、モードの判定、E_env の計算。flow.rs から呼ぶ |
| `src/flow.rs` | 閉じた区間の状態、P1〜P6、証明の失効、`Dma` の移動（own と同じ道）、汚染の伝播（F2704） |
| `src/escape.rs` | 窓とビューを参照として追う（F0520 がそのまま効く） |
| `src/codegen.rs` | `critical` の出入り、`f20_irq_table`、task_arena の隠れた列と切り替えの呼び出し、ワイヤ構造体の読み書き、DMA の表、システムコールの入口の関数と表。**6,438 行あるので、新しい出力は `codegen_kernel.rs`（新規）に分ける** |
| `src/strict.rs` / `src/reframe.rs` | 新しい構文の厳格形（往復の保証を保つ）。`&[u8]` の引数は窓に |
| `src/qemu.rs` / `src/driver.rs` / `src/main.rs` | `gic-version=2`・`force-legacy=false`・`--disk`・`--net`、`--user`・`--abi`、`fascon regions` に共有リージョンと閉じた区間の一覧 |
| `src/dict.rs` / `src/lsp.rs` | 辞書・補完・ホバー・F2601 の Quick Fix（`critical` で囲む） |

**BSP とランタイム**

| ファイル | 変更 |
| --- | --- |
| `bsp/qemu-virt-aarch64/start.S` | ベクタ 16 本を実装（今は同期例外以外すべて `bad_exception`）。共通の入口と出口（800 バイトのフレーム）、EL1 の svc（yield）、Lower EL の同期（svc と例外）と IRQ、EL2 から降りるときのタイマーの設定 |
| `bsp/qemu-virt-aarch64/bsp.c` | GICv2（GICD 0x0800_0000・GICC 0x0801_0000）の初期化と `f20_irq_table` の許可、ディスパッチ（IAR → ハンドラ → EOIR → 切り替えの判定）、タイマー、タスクのスタックの確保、Step 4 のページ表と ASID |
| `bsp/qemu-virt-aarch64/kernel.ld` | `.fascon_dma`（4 KiB 境界）、`.fascon_user`（2 MiB 境界・NOLOAD）、番兵ページ |
| `runtime/fascon2_rt.h` | `f20_trapframe_t`（start.S とずれないよう `_Static_assert` で位置を固定）、`fascon_irq_save` / `restore`、`F20_FAULT_USER`、`F20_DEFINE_TASK_TABLE`・`F20_DEFINE_DMA`、ワイヤの読み書き、ユーザーの窓の検証、`fascon_plat_*` の追加（`task_switch`・`timer_arm`・`dma_sync`） |
| `bsp/host/fascon2_plat_host.c` | 上の `fascon_plat_*` の PC 版（計数・偽の時計・普通の RAM） |
| `bsp/qemu-x86_64-bios/` | **4 ステップの対象外。** 新しい項目を含むソースを `--target x86_64` でビルドすると「このターゲットでは未対応」と診断する。既存の例は今までどおり |

### 5.4 後方互換の確かめ方

- 既存の全テスト（107 件、`--qemu` 付きで 136 件）を各フェーズの完了条件に含める。
- `tools/compare_c.py` で、既存の例・テスト・ベンチマークの生成 C が同じであることを各フェーズで確かめる。
- 新しい語を識別子に使った回帰テスト（`let irq = 1; fn critical() {}` など）を 0b で足す。
- 走査のベンチマーク（`bench/`）はフェーズ 0a と 1d の後に測り直す（列の仕組みに触るため）。

---

## 6. この設計が決めていないこと

- 入れ子の割り込みと優先度（GIC の優先度マスク）。`irq 名前 at N priority P` の形で後から足せます。
- SMP。`critical` の実装と E_env の定義（ほかの CPU のルートを常に含める）が変わりますが、構文は変わりません。
- 4 KiB 単位のユーザーメモリ、要求時ページング、共有メモリ。U-4 の予約事項が効いてきます。
- TCP / UDP、複数のファイルシステム。言語機能は Step 2 のもので足りる見込みです。
- x86_64 での割り込みとユーザー空間。

Step 1 の数値（フレームの 800 バイト、INTID 27、GIC の番地）は QEMU virt 8.2 で確かめました。Step 2〜4 の数値（virtio の番地と INTID、
ユーザーメモリの番地）は仕様から決めたもので、フェーズ 2c・4a の完了条件で確かめます。実機では 1 つも確かめていません。
