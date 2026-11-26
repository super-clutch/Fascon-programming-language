# Fascon Language Support (VS Code)

**English** | [日本語](#日本語)

Language support for **Fascon 1.0**: Rust-like syntax with **no lifetime annotations**, checked for memory safety at compile time and compiled to readable C that GCC / Clang optimize well.
The extension and its messages work in **English and Japanese** (setting `fascon.language`: `auto` / `en` / `ja`; `auto` follows the VS Code display language).

## Extensions (version 1.0)

- **Type inference** — `let task = create_process(100);` needs no type; **names without `@`** (`pcb_table[id].state = 1;` is the generation-verified `.get_mut(id)`); **declarations in any order**.
- **`bump_arena`** — `bump_arena scratch: Node[256];` allocates by advancing a top and releases everything the function allocated with one store at every exit (O(1)); 64-bit generation-tagged Ids still catch stale use.
- **`port_io`** — `port_io com1 at_port 0x3f8;` then `com1 = c;` is `out dx, al` on x86_64 and the PL011 on AArch64 (setting `fascon.target`).
- **`own_alloc(T)`** — a zero-filled `own T` by type name, no generics.
- **Automatic reframing** — `#![reframing(rust)]` (or `fascon.reframing: rust`) accepts plain Rust syntax; "Fascon: Show the Strict Fascon Form (functional-safety review)" shows the fully typed result; "Fascon: Toggle Automatic Reframing (plain Rust syntax) for This Workspace" switches it.
- **Virtual BIOS on x86_64 and AArch64** — "Fascon: Boot in QEMU (x86_64, Fascon virtual BIOS)" (Ctrl+Alt+X), "Fascon: Boot in QEMU (AArch64, Fascon virtual BIOS)", and "Fascon: Build BIOS Images for x86_64 and AArch64 (one source)".
- New examples: `gui_dual.fsc` (one GUI for both architectures), `bios_dual.fsc`, `rust_style.rs`, `app_wordfreq.fsc`.
- **`own` fields of any value type** — `struct MyContext { buffer: own i32, size: i32 }`, `own [u8; 4096]`, `own Str`. `*ctx.buffer += 1;` reads and writes the contents. A struct with own fields is move-only: moves, scope ends, overwrites and `drop` free the owns inside with it, and use after a move (**F0382**) or taking an own field out (**F0507**) is a compile error, so a double free cannot be written (snippet `ownfield`).
- **Callbacks** — function pointers `fn(&mut i32)` as parameters, struct fields (`(h.on_click)(&mut b)`) and static tables (snippets `foreachfn`, `handlerfn`). A callback must use the references it receives only inside its own stack frame (stack_only); storing one into an arena, a static, the caller's data or an own, or returning it, is **F0520 — reference escape constraint violated**, with a root-cause timeline in "Fascon: Explain Error (timeline, fixed code, mathematical explanation)". Example: `own_callbacks.fsc`.

See `docs/SPEC-1.0-EXT.md` in the source or release archives.

## What is new in 1.0

**Universal OS extension** (see `docs/SPEC-1.0-UNIVERSAL.md` in the source archive): profiles `rtos` / `generic-os`
(setting `fascon.profile`), **fascon-gc** (`gc_arena`; setting `fascon.gc`), board images
(`Fascon: Build a real-hardware image for a board (--board: ELF + raw .img)`, `Fascon: Boot on a board's QEMU machine (--board)`,
setting `fascon.board`; board packages in `boards/`), package safety labels
(`Fascon: Label the package safety (Pure Safe / Low-Level / Hardware)`) and the stack proof
(`Fascon: Prove the maximum stack depth (call graph, --target rtos)`).


- **Arenas with generation-tagged Ids** — `arena @process: Process[64];` is a static table. `Id<@process>` is 32 bits, [generation 32−K | index K], with K chosen from the capacity (here 26 | 6; 65,536 slots give 16 | 16), so every arena lasts for about 2.1–4.3 billion allocations. An Id of another arena is a type error (**F2101**); an Id whose slot was freed gives `None` (no ABA). Aliases such as `type FileId<@a: File> = Id<@a>` are checked against the arena's element type (**F2102**).
- **No branch on an unproven Id** — the index is masked into the table, so no Id can address outside it; a stale read only sets a latch, a stale write goes to a sentinel slot, and the program stops at the next commit point (before a call, output or return) with memory intact. `for id in @task { total += @task[id].vruntime; }` becomes one straight, unrolled scan.
- **Branch-free table scans (November 2026 update)** — a general `for id in @a` walks a live bitmap with ctz (TZCNT / RBIT + CLZ) instead of testing each slot. Filtered counts, filtered sums and min / max with their Id compile to masked scans with no branch but the loop's back edge (snippets `forcount`, `forpick`). Commit points leave fixed-trip loops. `#[repr(align(N))]` aligns a struct (snippet `repralign`), arenas live in a 2 MiB-aligned section, and an `own` value that never leaves its function uses a stack-like frame region. See `docs/PERF-1.0.md`.
- **Multi-core kernels with user space (November 2026 integration)** — `spawn_on` now works together with `irq`, `task_arena`, `dma_arena`, `syscall` and `trap` (F2614 retired; new F2616 for a shared-interrupt handler touching a per-core arena). `user_mem` takes any multiple of 4 KiB and is paged on demand, every task has its own address space (ASID on AArch64, CR3 on x86_64), EL0 / ring 3 processes run preemptively on every core, system-call windows are copied once into shadow buffers (no double reads), a panic stops every core (SGI / NMI), and fascon-gc keeps Ids alive in expression temporaries and inside channels. The x86_64 virtual BIOS (`fascon.target` = x86_64) is multi-core now. See `docs/SPEC-1.0-INTEGRATION.md`.
- **Columns** — every `for id in @a` reduction reads a column of just the values it reads (8 / 4 bytes or 1 bit per task instead of the 32-byte slot), so sums, filtered sums, counts and min / max picks run under one cycle per task. The compiler keeps the columns in step at every write. Settings: `fascon.columns` (default on; off gives the earlier tables) and `fascon.cpu` ("Fascon: Run on This PC (screen saved as PNG)" compiles for this CPU by default). "Fascon: Show Region Assignment" lists the columns.
- **`own T` trees** — move-only owners. `own Window { ok: own Button {..}, cancel: own Button {..} }` is one allocation; dropping the window frees the whole tree in one go.
- **Fixed-address MMIO regions** — `#[framebuffer] mmio @fb: [[u32; 800]; 600] at 0x4800_0000;`. Indices are checked at compile time where they can be proven (a constant out of range is **F2201**, overlapping regions are **F2203**), with no raw pointers and no `unsafe`.
- **Boot in QEMU with no BIOS** — "Fascon: Boot in QEMU (target from settings; portable = BIOS-less AArch64)" builds an AArch64 ELF with the bundled boot code and starts `qemu-system-aarch64 -M virt`. The screen appears through QEMU's `ramfb`.
- **Save screens without a window** — "Fascon: Boot in QEMU and Save Screenshots (headless, target from settings)" runs QEMU headless and saves a PNG at every `FASCON-GUI-READY` line.
- **Run on this PC** — "Fascon: Run on This PC (screen saved as PNG)" compiles the generated C for your PC; a `#[framebuffer]` screen is saved as PNG.
- **Examples** — "Fascon: Open a 1.0 Example (kernel / GUI / dual-architecture / BIOS / Rust style / app / callbacks)": the Mission 1 kernel (task_struct / VFS-like cyclic structures), the Mission 2 GUI (a window with two buttons) and the vruntime scan.
- **Tool check** — "Fascon: Show Version and Tool Check" lists the C compilers and QEMU it found and how to install missing ones.
- Files that only use 0.1 features are still handled by the bundled 0.1.0 engine (`fascon-core`).

## Tools you install yourself

| For | Windows | macOS | Linux |
|---|---|---|---|
| Boot in QEMU (`fascon qemu`, `fascon bios`) | `winget install LLVM.LLVM` and `winget install --id SoftwareFreedomConservancy.QEMU -e` | `brew install llvm lld qemu` | `clang lld qemu-system-arm qemu-system-x86` |
| Run on this PC (`fascon run`) | gcc from MSYS2 | `xcode-select --install` | `gcc` |

Fascon looks in `PATH` and in the usual install folders (`C:\Program Files\qemu`, `C:\Program Files\LLVM\bin`, Homebrew). Other folders can be added with `fascon.extraPath`.

## Features carried over from 0.1.0

- **Diagnostics** with a **root-cause timeline**, **verified quick fixes**, **Explain Error** (`Ctrl+Alt+E`), the **reference dictionary** (`Ctrl+Alt+D`, now with the 1.0 entries) and **C generation** ("Fascon: Generate C Code (build/\<name>.fascon.c)" writes `build/<name>.fascon.c` and `fascon2_rt.h`; the source is never overwritten).
- The bundled `fascon` executables also work in other editors through `fascon lsp`.

## Settings

| Setting | Default | Description |
|---|---|---|
| `fascon.language` | auto | Display language: `auto` / `en` / `ja` |
| `fascon.outDir` | build | Output directory for C, ELF and screenshots |
| `fascon.extraPath` | [] | Folders added to `PATH` when running QEMU and C compilers |
| `fascon.screenshotFrames` | 3 | Screens saved by "Fascon: Boot in QEMU and Save Screenshots (headless, target from settings)" |
| `fascon.qemuTimeout` | 90 | Time limit (seconds) for headless QEMU runs |
| `fascon.ptrBits` | 64 | Pointer width of the target |
| `fascon.cacheLine` / `fascon.barrierPolicy` / `fascon.isrCapacity` | | 0.1 engine settings |
| `fascon.serverPath` | (empty) | Use a different fascon executable |
| `fascon.target` | portable | `portable` (both architectures through `#if`; QEMU = BIOS-less AArch64), `x86_64` (`in` / `out`; QEMU pc + Fascon virtual BIOS), `aarch64` (ports remapped to MMIO; QEMU virt + Fascon virtual BIOS) |
| `fascon.reframing` | auto | `auto` (follow `#![reframing(rust)]` at the top of each file), `rust` (always accept plain Rust syntax), `off` |
| `fascon.emitStrictOnBuild` | false | Also write `<name>.strict.fsc` (strict Fascon) when generating C |

## Honest limits

Compile-time checks rule out use-after-free, double free, dangling references, out-of-bounds writes to declared regions and use of a stale Id. They cannot stop hardware faults (a cosmic-ray bit flip in RAM), bugs in the BSP's C and assembly, or wrong addresses written in an `mmio` declaration. A slot whose generation would wrap is retired instead of reused, so an arena is finite (about 2.1–4.3 billion allocations). See `docs/SPEC-1.0.md`.

---

## 日本語

**Fascon 1.0**（ライフタイム注釈なしでメモリ安全性をコンパイル時に検査し、GCC / Clang がよく最適化できる読みやすい C を出力する Rust 風の言語）のための拡張機能です。
拡張機能とメッセージは**日本語と英語**に対応しています（設定 `fascon.language`: `auto` / `ja` / `en`。`auto` は VS Code の表示言語に従います）。

### 拡張（版は 1.0 のまま）

- **型推論** — `let task = create_process(100);` に型は要りません。**`@` なしの名前**（`pcb_table[id].state = 1;` は世代タグで検証する `.get_mut(id)` と同じ）。**宣言の順序は自由**です。
- **`bump_arena`** — `bump_arena scratch: Node[256];` は先頭を進めるだけで確保し、関数のどの出口でも、その関数が確保したものを 1 回の格納でまとめて解放します（O(1)）。64 ビットの世代付き Id が古い Id の使用を見つけます。
- **`port_io`** — `port_io com1 at_port 0x3f8;` のあと `com1 = c;` は x86_64 では `out dx, al`、AArch64 では PL011 になります（設定 `fascon.target`）。
- **`own_alloc(T)`** — 型名だけで 0 埋めの `own T` を確保します（ジェネリクスなし）。
- **自動リフレーミング** — `#![reframing(rust)]`（または設定 `fascon.reframing: rust`）で、ふつうの Rust の構文を受け付けます。「Fascon: 厳格な Fascon（機能安全証明形態）を表示」で、すべての型を書いた結果を見られます。「Fascon: このワークスペースで自動リフレーミング（ふつうの Rust 構文）を切り替え」で切り替えます。
- **x86_64 と AArch64 の仮想 BIOS** — 「Fascon: QEMU で起動（x86_64・Fascon の仮想 BIOS）」（Ctrl+Alt+X）、「Fascon: QEMU で起動（AArch64・Fascon の仮想 BIOS）」、「Fascon: x86_64 と AArch64 の BIOS の像を作る（1 つのソースから）」。
- 新しい例: `gui_dual.fsc`（1 つの GUI を両方のアーキテクチャで）、`bios_dual.fsc`、`rust_style.rs`、`app_wordfreq.fsc`。
- **どんな値の型でも持てる `own` フィールド** — `struct MyContext { buffer: own i32, size: i32 }`、`own [u8; 4096]`、`own Str`。中身は `*ctx.buffer += 1;` のように `*` で読み書きします。own フィールドを持つ構造体は移動専用で、移動・スコープの終わり・上書き・`drop` のときに中の own も連動して解放されます。移動した後の使用（**F0382**）や own フィールドだけの取り出し（**F0507**）はコンパイルエラーなので、二重解放は書けません（スニペット `ownfield`）。
- **コールバック** — 関数ポインタ `fn(&mut i32)` を引数・構造体のフィールド（`(h.on_click)(&mut b)`）・static の表に置けます（スニペット `foreachfn`・`handlerfn`）。コールバックは受け取った参照を自分のスタックフレームの中でだけ使います（stack_only）。アリーナ・static・呼び出し元のデータ・own へ保存したり返したりすると **F0520 — 参照のエスケープ制約違反** になり、「Fascon: エラーを解説（タイムライン・修正コード・数理的解説）」に根本原因のタイムラインが出ます。例: `own_callbacks.fsc`。

詳しくはソースまたは配布アーカイブの `docs/SPEC-1.0-EXT.ja.md` を見てください。

### 1.0 の新機能

**ユニバーサル OS 拡張**（ソースの `docs/SPEC-1.0-UNIVERSAL.ja.md`）: プロファイル `rtos` / `generic-os`（設定 `fascon.profile`）、
**fascon-gc**（`gc_arena`。設定 `fascon.gc`）、ボードの像（「Fascon: ボード用の実機の像を作る（--board: ELF と生の .img）」
「Fascon: ボードの QEMU のマシンで起動する（--board）」、設定 `fascon.board`、`boards/` のボードのパッケージ）、
パッケージの安全性のラベル（「Fascon: パッケージの安全性のラベルを付ける（Pure Safe / Low-Level / Hardware）」）、
スタックの証明（「Fascon: 最大スタックを証明する（呼び出しグラフ、--target rtos）」）。


- **世代タグ付き Id のアリーナ** — `arena @process: Process[64];` は静的な表。`Id<@process>` は 32 ビットの [世代 32−K | 添字 K] で、K は容量から決まります（この例は 26 | 6、65,536 スロットなら 16 | 16）。どのアリーナも約 21〜43 億回の確保まで使えます。別のアリーナの Id は型エラー（**F2101**）、解放済みスロットの Id は `None`（ABA が起きない）。`type FileId<@a: File> = Id<@a>` のような別名はアリーナの要素型と照合されます（**F2102**）。
- **証明できない Id でも分岐しない** — 添字は表へのマスクなので、どんな Id も表の外を指せません。古い Id の読み出しはラッチを立てるだけ、書き込みは番兵スロットへ逸れ、次のコミット点（呼び出し・出力・戻りの直前）でメモリを壊さずに止まります。`for id in @task { total += @task[id].vruntime; }` は表を一直線に走査する展開済みのループになります。
- **分岐のない表の走査（2026-11 の改修）** — 一般の `for id in @a` は、スロットを 1 つずつ調べる代わりに生存ビットマップを ctz（TZCNT / RBIT + CLZ）でたどります。条件付きの個数・条件付きの総和・最小 / 最大とその Id は、ループの戻り以外に分岐のないマスク付きの走査になります（スニペット `forcount`・`forpick`）。コミット点は回数の決まったループの外へ出ます。`#[repr(align(N))]` で構造体の整列を指定でき（スニペット `repralign`）、アリーナは 2 MiB 境界のセクションに置かれ、関数の外へ出ない `own` はスタックと同じ動きのフレームリージョンに置かれます。詳しくは `docs/PERF-1.0.ja.md` を見てください。
- **ユーザー空間つきのマルチコアのカーネル（2026-11 の統合改修）** — `spawn_on` と `irq`・`task_arena`・`dma_arena`・`syscall`・`trap` を一緒に使えます（F2614 は廃止。共有の割り込みのハンドラがコアごとのアリーナに触ると新しい F2616）。`user_mem` は 4 KiB の倍数なら何でもよく、要求時ページングで写されます。タスクごとにアドレス空間（AArch64 は ASID、x86_64 は CR3）を持ち、EL0 / Ring 3 のプロセスが全コアでプリエンプティブに動き、システムコールの窓は影バッファへ 1 回だけ写されます（2 回読みなし）。パニックは全コアを止め（SGI / NMI）、fascon-gc は式の一時値とチャネルの中の Id を根として守ります。x86_64 の仮想 BIOS（`fascon.target` = x86_64）もマルチコアになりました。詳しくは `docs/SPEC-1.0-INTEGRATION.ja.md`。
- **列** — `for id in @a` の集計は、その集計が読む値だけの列（32 バイトのスロットではなく 1 タスク 8 / 4 バイトか 1 ビット）を読むので、総和・条件付き総和・個数・最小 / 最大の選択が 1 タスク 1 クロック未満になります。列はコンパイラが書き込みのたびに同期させます。設定: `fascon.columns`（既定 on。off で前の表）と `fascon.cpu`（「Fascon: この PC で実行（画面は PNG に保存）」は既定でこの CPU 向けにコンパイル）。「Fascon: リージョン確定結果を表示」に列の一覧が出ます。
- **`own T` のツリー** — 移動しかできない所有者。`own Window { ok: own Button {..}, cancel: own Button {..} }` は確保 1 回。ウィンドウを捨てるとツリー全体が一括で解放されます。
- **固定番地の MMIO リージョン** — `#[framebuffer] mmio @fb: [[u32; 800]; 600] at 0x4800_0000;`。証明できる添字はコンパイル時に検査（定数の範囲外は **F2201**、リージョンの重なりは **F2203**）。生ポインタも `unsafe` もありません。
- **BIOS なしで QEMU 起動** — 「Fascon: QEMU で起動（ターゲットは設定に従う。portable = BIOS なし AArch64）」で、同梱の起動コードと一緒に AArch64 の ELF を作り `qemu-system-aarch64 -M virt` を起動します。画面は QEMU の `ramfb` に出ます。
- **ウィンドウなしで画面を保存** — 「Fascon: QEMU で起動して画面を保存（ヘッドレス・ターゲットは設定に従う）」は QEMU を画面なしで動かし、`FASCON-GUI-READY` の行ごとに PNG を保存します。
- **この PC で実行** — 「Fascon: この PC で実行（画面は PNG に保存）」は生成 C を PC 用にコンパイルして実行し、`#[framebuffer]` の画面を PNG に保存します。
- **例** — 「Fascon: 1.0 の例を開く（カーネル・GUI・両アーキテクチャ・BIOS・Rust 風・アプリ・コールバック）」: ミッション1 のカーネル（task_struct / VFS 風の循環構造）、ミッション2 の GUI（ボタン 2 個のウィンドウ）、vruntime の集計。
- **ツールの確認** — 「Fascon: バージョンと必要なツールを確認」で、見つかった C コンパイラと QEMU、足りないものの入れ方を表示します。
- 0.1 の機能だけを使うファイルは、同梱の 0.1.0 エンジン（`fascon-core`）がこれまでどおり処理します。

### 自分で入れるツール

| 用途 | Windows | macOS | Linux |
|---|---|---|---|
| QEMU で起動（`fascon qemu`・`fascon bios`） | `winget install LLVM.LLVM` と `winget install --id SoftwareFreedomConservancy.QEMU -e` | `brew install llvm lld qemu` | `clang lld qemu-system-arm qemu-system-x86` |
| この PC で実行（`fascon run`） | MSYS2 の gcc | `xcode-select --install` | `gcc` |

Fascon は `PATH` と、よくあるインストール先（`C:\Program Files\qemu`、`C:\Program Files\LLVM\bin`、Homebrew）を探します。ほかのフォルダは設定 `fascon.extraPath` で追加できます。

### 0.1.0 から引き継いだ機能

- **根本原因タイムライン**付きの**診断**、**検証済み Quick Fix**、**エラーを解説**（`Ctrl+Alt+E`）、**仕様辞書**（`Ctrl+Alt+D`。1.0 の項目を追加）、**C コード生成**（「Fascon: C コードを生成（build/<名前>.fascon.c）」で `build/<名前>.fascon.c` と `fascon2_rt.h` を書き出し、元ファイルは上書きしません）。
- 同梱の `fascon` 実行ファイルは、ほかのエディタからも `fascon lsp` で使えます。

### 設定

| 設定 | 既定 | 説明 |
|---|---|---|
| `fascon.language` | auto | 表示言語: `auto` / `ja` / `en` |
| `fascon.outDir` | build | C・ELF・画面の出力先 |
| `fascon.extraPath` | [] | QEMU や C コンパイラを動かすときに `PATH` へ足すフォルダ |
| `fascon.screenshotFrames` | 3 | 「Fascon: QEMU で起動して画面を保存（ヘッドレス・ターゲットは設定に従う）」で保存する画面の数 |
| `fascon.qemuTimeout` | 90 | 画面なしの QEMU 実行の制限時間（秒） |
| `fascon.ptrBits` | 64 | ターゲットのポインタ幅 |
| `fascon.cacheLine` / `fascon.barrierPolicy` / `fascon.isrCapacity` | | 0.1 エンジン用の設定 |
| `fascon.serverPath` | （空） | 別の fascon 実行ファイルを使う場合 |
| `fascon.target` | portable | `portable`（`#if` で両方。QEMU は BIOS なしの AArch64）、`x86_64`（`in` / `out`。QEMU pc + Fascon の仮想 BIOS）、`aarch64`（ポートを MMIO へ写す。QEMU virt + Fascon の仮想 BIOS） |
| `fascon.reframing` | auto | `auto`（各ファイル先頭の `#![reframing(rust)]` に従う）、`rust`（常にふつうの Rust 構文を受け付ける）、`off` |
| `fascon.emitStrictOnBuild` | false | C を生成するとき `<名前>.strict.fsc`（厳格な Fascon）も書き出す |

### 正直な限界

コンパイル時の検査で、解放後使用・二重解放・ぶら下がり参照・宣言したリージョンへの範囲外書き込み・古い Id の使用は起こらなくなります。ただしハードウェアの故障（宇宙線による RAM のビット反転など）、BSP の C・アセンブリのバグ、`mmio` 宣言に書いた番地そのものの誤りは防げません。世代が一周しそうなスロットは再利用せずに引退させるので、アリーナは有限です（約 21〜43 億回の確保まで）。詳しくは `docs/SPEC-1.0.ja.md` を参照してください。
