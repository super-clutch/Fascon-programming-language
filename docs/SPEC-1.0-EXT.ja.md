# Fascon 1.0 拡張仕様書 — 型推論・`@` なし・2 パス解析・bump_arena・port_io・own_alloc・自動リフレーミング・厳格形・仮想 BIOS・どんな型でも持てる own フィールド・コールバック

English: [SPEC-1.0-EXT.md](SPEC-1.0-EXT.md) · 1.0 の基本仕様: [SPEC-1.0.ja.md](SPEC-1.0.ja.md) · 使い方: [USAGE-1.0.ja.md](USAGE-1.0.ja.md)

この文書は Fascon 1.0（バージョン名は 1.0 のまま）に加えた拡張の仕様です。10 章で、どんな値の型でも持てる own フィールドと、関数ポインタのコールバック（stack_only・F0520）を加えました。どれも Fascon の既存の構文解析ルール
（ジェネリクスなし・ライフタイム注釈なし・第二級参照）を守ったまま、書きやすさ（人間ファースト）と、
x86_64 / AArch64 のベアメタル・OS（GUI を含む）・一般アプリの記述を広げます。

**変わらないもの**

- 型（`Id<a>`）と世代タグによる安全性: アリーナの Id は着色された名目型で、別のアリーナの Id は渡せない（F2101）。
  解放・再利用されたスロットの Id は世代が合わず、None（ルックアップ）か安全な停止（`a[id]`）になる。
- タスク走査の速さ: 既存のプログラム（`examples/*.fsc`・`tests/**/*.fsc`）の生成 C は、注釈を除いて 1.0.0 と 1 文字も
  変わりません（tests で比較しています）。`@` を書かない新しい書き方でも、名前解決が同じ式に戻すので同じ C になります。

## 0. 処理の流れ

```text
字句解析 ─→（自動リフレーミング: Rust の見た目 → Fascon の形）
        ─→ 1 周目: 名前の事前登録（構造体・アリーナ・バンプアリーナ・MMIO・ポート・関数・static・型の別名）   src/prescan.rs
        ─→ 2 周目: 構文解析（本体を読む）                                                              src/parser.rs
        ─→ 名前解決（`@` なしの名前 → リージョン、厳格形の脱糖衣）                                     src/resolve.rs
        ─→ 意味解析（型推論: 関数ごとに必要なら 2 周）                                                 src/sema.rs
        ─→ 流れ解析（移動・借用・効果・生存証明・範囲）                                                 src/flow.rs
        ─→ C 生成（バンプアリーナ・呼び出しリージョン・I/O ポート・own_alloc）                         src/codegen.rs
        ─→（--emit-strict-fascon: 厳格な Fascon の書き出し）                                           src/strict.rs
```

## 1. 型推論の完全自動化（F0282 の根絶）

`let` に型注釈は要りません。

```rust
let task = create_process(100);        // Id<pcb_table>（戻り値から）
let mut best = None;                   // Option<Id<tasks>>（後の `best = Some(id)` から）
let mut acc;                           // i32（後の `acc = 0` から）
let ids = [a, b, c];                   // [Id<tasks>; 3]
```

- 型変数（`Ty::Var`）を union-find で解き、代入・比較・呼び出しの引数・戻り値・アリーナの添字・メソッドの受け手から決めます。
- 関数ごとに型検査を 2 周します。1 周目で決まった `let` の型（seed）を 2 周目の前提にするので、型が後ろの行で
  決まる変数を、それより前の行でフィールド・メソッド・添字の土台に使っても通ります（例: `tasks[best.unwrap()]`）。
- 何にも使われない整数・`None` は i32 に既定化します（エラーにしません）。
- F0282 が残るのは、2 周しても型が決まらないまま、フィールド・メソッド・添字の土台に使われた場合だけです。
  `--emit-strict-fascon` で推論の結果（すべての `let` の型）を見られます。

## 2. `@` の廃止（省略可）と bump_arena

### 2.1 `@` なしの名前

```rust
pcb_table[id].id = 100;                 // 世代タグで検証する .get_mut(id) と同じ
for t in tasks { .. }                   // アリーナを回す
let x = pcb_table.alloc(Pcb { .. });
type ProcId<a: Pcb> = Id<a>;            // 型の別名の引数にも `@` は要らない
```

- 名前だけの式は、同じ名前のローカル変数がスコープに無く、1 周目の表でリージョン（アリーナ・バンプアリーナ・MMIO・
  ポート）なら、そのリージョンを指します（`ExprKind::ArenaRef`）。以降は以前の `@pcb_table` と同じ道を通るので、
  世代検査の静的な省略・ラッチ・列の走査（集計の特殊化）はそのままです。
- 古い `@名前` は「いつもリージョン」という明示の印として使えます（同じ名前のローカルがあっても）。
- リージョンの名前は関数・static・型と同じ名前空間を使います（重なると F0428）。

### 2.2 `pcb_table[id]` の意味

`R[i]`（R はアリーナ・バンプアリーナ）は、厳格形の `R.get_mut(i).unwrap()`（書き込み・`&mut`）/ `R.get(i).unwrap()`
（読み出し専用。通して書くと F0596）と**同じ式**です。名前解決が厳格形を `R[i]` に戻すので、どちらで書いても生成 C は
同じです。古い Id の扱いは 1.0 のまま（生存を証明できない場合は番兵スロット + ラッチ、次のコミット点で安全に停止）。

### 2.3 bump_arena

```rust
bump_arena scratch: Node[256];          // または bump_arena @scratch: Node[256];

fn sum_list(n: i64) -> i64 {            // この関数が scratch の「呼び出しリージョン」
    let mut head = None;
    for i in 0..n {
        head = scratch.alloc(Node { v: i, next: head });   // 先頭（top）を 1 進めるだけ
    }
    ..
}                                       // 出口で top を入口の印へ戻す 1 回の格納 = O(1) の一括解放
```

| 項目 | 内容 |
| --- | --- |
| 表 | `.bss.fascon_arena`（2 MiB 境界）: `st`（top・peak・allocs、128 B を占有）\| `gen[2^K]`（64 ビット）\| `slot[2^K + 1]`（番兵込み） |
| Id | 64 ビット `[世代 64-K \| 添字 K]`（`Option<Id<b>>` は 0 = None で 8 バイト） |
| 確保 | `i = top; if i >= CAP → None; g = gen[i] + 1; gen[i] = g; top = i + 1; slot[i] = v` |
| 検査 | `i < top && gen[i] == g && g != 0`（分岐なしの形はラッチへ、書き込みは番兵スロットへ） |
| 解放 | 呼び出しリージョン（その関数）の**すべての出口**（末尾・`return`）で `top = 入口の印`（1 回の格納） |
| 走査 | `for id in b` は `0..top` の直線ループ。`b.len()` は今の top |
| 昇格 | B の Id を返す関数、`&mut` 引数で B の Id を外へ渡す関数の確保は、呼び出し元のリージョンへ昇格（その関数では戻さない） |
| 世代 | 1 スロット 2^(64-K) - 1 回まで。使い切ったスロットからは確保しない（世代は一周しない = ABA なし） |

**安全性**: 巻き戻した後の古い Id は「添字 ≥ top」か「世代違い」なので、None（ルックアップ）か番兵 + ラッチ（`b[id]`）
になり、後から同じスロットに置かれた他人のデータには届きません。呼び出し先は自分の入口の印（≥ 呼び出し元の top）まで
しか戻さないので、呼び出し元が持つ Id は呼び出しの後も有効です。

**静的な規則**

| コード | 内容 |
| --- | --- |
| F2401 | bump_arena に `free` / `remove` は無い（1 つずつは解放しない） |
| F2402 | 長く生きる場所（ふつうのアリーナ・別のバンプアリーナの要素、static）に B の Id を置けない |
| F2103 | bump_arena の要素は own を持てない（一括解放は解放処理を走らせない） |

**生成 C（`tests/ok/bump.fsc` の `depth`）**

```c
static int64_t fsc_depth(int64_t n) {
    const uint32_t _bm_scratch = arena_scratch.st.top; /* 呼び出しリージョン: 入口の印 */
    ...
    fsc_Id_scratch _t2 = arena_scratch_alloc(&(fsc_Node){ .v = n });
    ...
    _rv1 = fascon_add_i64(arena_scratch_at_r(id, &_flt)->v, r);
    F20_COMMIT(_flt, "bump.fsc:48:1");
    arena_scratch.st.top = _bm_scratch; /* 一括解放（1 回の格納） */
    return _rv1;
}
```

戻り値は巻き戻しの前に求め終えます（戻り値の式がバンプアリーナを読んでもよい）。

## 3. I/O ポート: `port_io`（x86_64 の in / out と、AArch64 の MMIO の分離）

```rust
port_io com1 at_port 0x3f8;                 // 型を省くと u8
port_io com1_lsr at_port 0x3fd;
port_io regs: [u8; 7] at_port 0x3f9;        // 連続するレジスタの配列（添字は境界検査付き）
#[mmio_map(0x0900_0000)] port_io dev: u32 at_port 0x5000;   // AArch64 での写し先を指定

com1 = c;                                    // 書き込み
let s = com1_lsr;                            // 読み出し
regs[2] = 0x03;
```

| ターゲット | `com1 = c;` | `let s = lsr;` |
| --- | --- | --- |
| `--target x86_64` | `mov dx, 0x3f8; out dx, al`（`fascon_outb`、"Nd" 制約のインライン asm） | `in al, dx` |
| `--target aarch64` | コンパイラが MMIO に写す: COM1 → PL011 の DR（`0x0900_0000`） | LSR ← PL011 の FR から作る |
| PC 実行（`fascon run`） | プラットフォーム層: COM1 の送信は標準出力 | LSR = 送信可 |
| 既定（portable） | 上の 3 つを `#if` で選ぶ | |

AArch64 での写し方（`src/target.rs`）:

| x86 のポート | AArch64 |
| --- | --- |
| COM1（0x3f8〜0x3ff） | PL011 で 16550 を真似る（`f20_com1_in` / `f20_com1_out`）: THR / RBR → DR、LSR ← FR（受信あり = !RXFE、送信可 = !TXFF）、DLAB の間の 0x3f8 は除数として覚えるだけ、IIR = 1、MSR = 0xB0 |
| COM2〜4 | つながっていない（LSR = 0x60、書き込みは捨てる） |
| isa-debug-exit（0xf4 / 0x501） | 終了（`(値 << 1) \| 1`。QEMU x86 と同じ終了コード） |
| それ以外 | QEMU virt の PCI I/O 窓（`0x3eff_0000 + ポート番号`）の MMIO |
| `#[mmio_map(番地)]` 付き | その番地の MMIO |

規則: 要素は u8 / u16 / u32、番号は 0〜0xFFFF（F2205）。重なるポートは F2203。ポートへの参照は作れない（F2204）。
ポートの読み書きは外への作用なので、その直前がコミット点になり、評価の順序は書いた順のまま保たれます。

**1 つのソースから両アーキテクチャの BIOS**: `fascon bios app.fsc` は `--target x86_64` と `--target aarch64` の C をそれぞれ作り、
`build/app.x86_64.bios.bin` と `build/app.aarch64.bios.bin` を出力します（§7）。

## 4. own_alloc(T): ジェネリクスなしの動的確保

```rust
let data = own_alloc(Block);       // 0 で埋めた own Block。スコープの終わりで自動的に解放
```

- 引数は構造体の**型名**（式ではない）。ジェネリクスの `<T>` は使いません（構文規則はそのまま）。
- own の子（Option でない own のフィールド。配列・値として埋め込んだ構造体の中も）も 0 で作り、**同じ所有リージョンに
  詰めます**（確保 1 回・解放 1 回）。`Option<own T>` は None から始まります。
- どこへも移動しない `let` ならフレームリージョン（ポインタを進めるだけ）、移動するならヒープの所有リージョン。
- 規則: 構造体以外は F2301、own の子が終わりなく続く型（`A { b: own B }`, `B { a: own A }`）は F2304
  （戻る道を `Option<own T>` に）、16 バイトを超える整列は F2032。

```c
const size_t _sz2 = FASCON_OWN_SPAN(fsc_Tree) + FASCON_OWN_CHILD_SPAN(fsc_Pair) + 5u * FASCON_OWN_CHILD_SPAN(fsc_Leaf) + ...;
char *_blk3 = (char *)fascon_own_alloc(_sz2, "own_alloc.fsc:21:17");   /* Tree と own の子 8 個を 1 つの所有リージョンに */
__builtin_memset(_blk3, 0, _sz2);
(void)fsc_zfill_Tree((fsc_Tree *)_blk3, _blk3 + FASCON_OWN_SPAN(fsc_Tree));
```

## 5. 2 パス解析: 宣言順の完全自由化（F2104 の根絶）

1 周目（`src/prescan.rs`）でトークン列を走り、すべての構造体・アリーナ・バンプアリーナ・MMIO・ポート・関数・static・
型の別名の名前を登録します。2 周目の構文解析・名前解決はこの表を見るので、`main` を先頭に書き、構造体やアリーナを
ファイルの最後で宣言してもかまいません。F2104 は、その名前がファイルのどこにも宣言されていないときだけ出ます。

## 6. 自動リフレーミングモード（ふつうの Rust 構文、ON / OFF）

ファイル先頭の `#![reframing(rust)]`、または `--reframing=rust`（環境変数 `FASCON_REFRAMING=rust`、VS Code の
`fascon.reframing`）でオン。`#![reframing(off)]` / `--reframing=off` でオフ。字句解析の直後に、トークン列のまま
Fascon の本来の形へ書き換えます（`src/reframe.rs`）。意味はすべて Fascon の規則（世代タグ・境界検査・own）で検査されます。

| Rust の見た目 | Fascon の形 |
| --- | --- |
| `#[link_section = ".uart"] static mut UART: [u8; 8] = [0; 8];` | `port_io UART: [u8; 8] at_port 0x3f8;`（x86: COM1、AArch64: PL011） |
| `#[link_section = ".mmio.0x0900_0000"] static mut U: [u32; 7] = ..;` | `mmio U: [u32; 7] at 0x0900_0000;` |
| `#[link_section = ".port.0x3f8"] static mut COM1: u8 = 0;` | `port_io COM1: u8 at_port 0x3f8;` |
| `#[link_section = ".fb.0x4800_0000"] static mut FB: [[u32; 800]; 600] = ..;` | `#[framebuffer] mmio FB: .. at 0x4800_0000;` |
| `arena!(pcb: Pcb[64]);` / `bump_arena!(tmp: Node[256]);` | `arena pcb: Pcb[64];` / `bump_arena tmp: Node[256];` |
| `port_io!(com1, 0x3f8);` / `mmio!(uart: [u32; 18], 0x0900_0000);` | `port_io com1 at_port 0x3f8;` / `mmio uart: .. at ..;` |
| `let data = own_alloc!(Block);` | `let data = own_alloc(Block);` |
| `println!("pid {} = {:#x}", a, b)` / `{name}` | `{ put_str("pid "); put(a); put_str(" = "); put_hex(b); print_str(""); }` |
| `pcb_table[task].state = 1;` | 世代タグで検証する `.get_mut(task)`（§2.2） |
| `unsafe { .. }` / `pub` / `use ..;` / `extern "C"` / `#[no_mangle]` / `#![no_std]` | 取り除く |
| `'A'` / `b'A'` / `&'a T` / `&'static str` / `char` | `65` / `65` / `&T` / `Str` / `u32` |
| `while let P = e { .. }` | `loop { if let P = e { .. } else { break; } }` |
| `for b in s.bytes() { .. }` / `for &b in s.as_bytes() { .. }` / `for b in v.iter()` | `{ let rf_bytes1 = (s); for rf_i1 in 0..rf_bytes1.len() { let b = rf_bytes1.byte(rf_i1); .. } }` |
| `s.as_bytes()[i]` / `s.as_bytes().len()` / `let v = s.as_bytes(); v[i]` | `s.byte(i)` / `s.len()` / `v.byte(i)`（境界検査付き） |
| `&[u8]` / `b"..."` | `Str`（読み出し専用のバイト列。Fascon にスライスは無い） |
| `-> !` / `const fn` / `struct S;` / `#[panic_handler] fn ..` | 戻り値なし / `fn` / `struct S {}` / 取り除く |
| `fn main` が無い | `kernel_main` / `kmain` / `_start` / `rust_main` を main にする |

impl・トレイト・ジェネリクス・クロージャは書き換えません（Fascon の規則どおりエラー）。例: `examples/rust_style.rs`。

### 6.1 厳格形の書き出し（`--emit-strict-fascon`）

`fascon build app.rs --emit-strict-fascon` は `build/app.strict.fsc` に「機能安全証明形態」の Fascon を書きます。

- すべての `let` に型、すべての整数リテラルに型の接尾辞（`100u32`）
- アリーナの添字は `R.get_mut(i).unwrap()`（書き込み）/ `R.get(i).unwrap()`（読み出し）
- ポートは `p.read()` / `p.write(v)` / `p.read_at(i)` / `p.write_at(i, v)`
- 型の別名は展開、配列の長さは数。`@` は同じ名前のローカルがリージョンを隠す所にだけ付く
- 呼び出しリージョンの注釈と、リフレーミングで書き換えた箇所の記録（行番号付き）

**往復の保証**: 厳格形をもう一度コンパイルすると、元のソースと同じ C になります（注釈と行番号を除く）。
`tests/run_tests.py` が tests/ok と examples の全ファイルで確かめています。

## 7. 仮想 BIOS（x86_64 と AArch64）

SeaBIOS / UEFI / U-Boot を使わず、Fascon 自身の起動コードが QEMU の `-bios` の像になります。

**x86_64**（`bsp/qemu-x86_64-bios/`）: QEMU pc は像を 4 GiB の直下に置き、CPU はリアルモードで 0xFFFF_FFF0 から走ります。

1. 16 ビット: GDT を読み込み、プロテクトモードへ
2. 32 ビット: 0x1000〜0x6FFF にページ表（0〜4 GiB の恒等マップ、2 MiB ページだけ、3〜4 GiB はキャッシュ無効）→ PAE・
   EFER.LME・ページングでロングモードへ。CR0.CD / NW を下ろしてキャッシュ有効、CR4.OSFXSR で SSE 有効
3. 64 ビット: ROM の中のカーネル像（.text / .rodata / .data）を RAM の 1 MiB へ写し、.bss とアリーナを 0 で埋めて `bsp_main`
4. `bsp_main`: COM1 を 115200 8N1 に、例外 32 個の IDT（番号・RIP・CR2 を表示して終了）、MMIO 表の検査、
   `#[framebuffer]` があれば ramfb を fw_cfg（I/O ポート 0x510 / 0x511、DMA 0x514 / 0x518）で設定、own ヒープ 8 MiB と
   フレーム 1 MiB を登録して `fascon2_main()`
5. 終了は isa-debug-exit（ポート 0xf4、QEMU は `(コード << 1) | 1` で終わる。`fascon qemu` は元のコードを返す）

ROM の大きさはカーネル像から決めます（1 回目のリンクで測り、2 回目で像を起動コードの直前に置く。64 KiB の倍数）。

```text
qemu-system-x86_64 -M pc -m 2G -bios app.x86_64.bios.bin -nic none -no-reboot \
  -device isa-debug-exit,iobase=0xf4,iosize=0x04 -serial stdio [-vga none -device ramfb]
```

**AArch64**（`bsp/qemu-virt-aarch64/firmware.S` / `firmware.ld`）: QEMU virt は像をフラッシュ（0 番地）に置き、CPU0 は
0 番地から走ります。フラッシュの中のカーネル像を RAM の 0x4008_0000 へ写し、`-kernel` のときと同じ `_start`（start.S）へ。

```text
qemu-system-aarch64 -M virt -cpu cortex-a72 -m 256M -nic none -semihosting -bios app.aarch64.bios.bin -nographic
```

フレームバッファ（`0x4800_0000`）はどちらのアーキテクチャでも RAM の中なので、GUI のソースも 1 つで済みます
（`examples/gui_dual.fsc`: キー入力は port_io の COM1）。

## 8. 変更したソース

| ファイル | 内容 |
| --- | --- |
| `src/parser.rs` | 2 周目の構文解析: 1 周目の表を見て `@` なしの名前を解釈、`bump_arena` / `port_io ... at_port` / `own_alloc(T)` の構文、型の別名の引数・`Id<名前>` の引数を表で分類、`for id in 名前`、リフレーミング時の `while let` の展開 |
| `src/sema.rs` | 型推論（型変数・union-find・関数ごとの 2 周・seed・既定化、F0282 は 2 周目だけ）、bump_arena の規則（F2401 / F2402 / F2103）と呼び出しリージョンの計算（昇格を含む）、ポートの宣言・重なり・読み書き（F2205 / F2203 / F2204）、own_alloc（F2301 / F2304 / F2032）、`put` 組み込み、F0428 |
| `src/codegen.rs` | バンプアリーナの表と alloc / get / at / at_r / validate、呼び出しリージョンの印と巻き戻し（すべての出口）、`for id in b` の直線ループ、ポートの関数（hosted / x86 in・out / AArch64 MMIO を `#if` で、`--target` で片方だけ）、ポートの読み書きとコミット点、own_alloc の 0 埋めと子の詰め込み（`fsc_zfill_T`）、`put` |
| `src/lexer.rs` | `bump_arena` / `port_io` のキーワード、リフレーミング用の文字リテラル・ライフタイムの処理 |
| `src/prescan.rs`（新規） | 1 周目: 名前の事前登録 |
| `src/resolve.rs`（新規） | `@` なしの名前 → リージョン（スコープを見る）、厳格形 `R.get(_mut)(i).unwrap()` / ポートの `read` / `write` の脱糖衣 |
| `src/reframe.rs`（新規） | 自動リフレーミング |
| `src/strict.rs`（新規） | 厳格形の書き出し |
| `src/target.rs`（新規） | `--target` と、AArch64 での I/O ポートの写し方 |
| `src/flow.rs` | ポート（参照不可・書き込み）・own_alloc・読み出し専用の厳格形（F0596） |
| `src/types.rs` / `src/ast.rs` | 型変数、バンプアリーナ・ポートの表、構文木の項目 |
| `src/driver.rs` / `src/main.rs` | 処理の流れ、`--target` / `--reframing` / `--emit-strict-fascon` / `fascon bios`、リージョン報告 |
| `src/qemu.rs` / `src/assets.rs` | x86_64 / AArch64 の仮想 BIOS のビルド（ELF → ROM の像）と QEMU の起動、`fascon doctor` |
| `src/lsp.rs` / `src/dict.rs` | `@` なしの名前のホバー、補完、辞書の項目 |
| `runtime/fascon2_rt.h` | `f20_bid_t`・`F20_DEFINE_BUMP`・`f20_bump_validate`、`fascon_inb/outb/..`、`f20_com1_in/out`（AArch64） |
| `bsp/qemu-x86_64-bios/`（新規） | `bios.S`・`bsp.c`・`bios.ld` |
| `bsp/qemu-virt-aarch64/firmware.S` / `firmware.ld`（新規） | AArch64 の仮想 BIOS |
| `bsp/host/fascon2_plat_host.c` | I/O ポートの真似（COM1・isa-debug-exit・64 KiB のポート空間） |

## 9. テスト

`python3 tests/run_tests.py target/release/fascon --qemu` は、従来の 52 件に加えて次を確かめます（合計 113 件）。

- エラー検出: F2401・F2402（表・static）・F2103・F2304・F2205・F2204・F0596（`.get().unwrap()` への書き込み）・F2301
- 実行結果: 型推論・`@` なしと宣言順・bump_arena（昇格・再帰・古い Id の検出・満杯）・port_io（PC での真似）・own_alloc（子の詰め込み・解放漏れなし）・リフレーミング（Str をバイト列として使う書き方を含む）
- 厳格形の往復（21 ファイル、元と同じ C）と、厳格形の中身
- `--target x86_64` の C は `out dx, al`、`--target aarch64` の C は PL011 への写し
- `fascon bios`（1 つのソースから 2 つの像、64 KiB の倍数）
- 仮想 BIOS での起動（x86_64 / AArch64 の両方で同じ出力）: bump・own_alloc・型推論・`@` なし・フレーム・リフレーミング・vruntime 走査・bios_dual・rust_style・kernel_core
- GUI（gui_dual.fsc）を x86_64 と AArch64 の両方で起動し、ramfb の画面 3 枚とキー入力（port_io の COM1）
- 既存の例とテストの生成 C が 1.0.0 と同じ（注釈を除く）

## 10. どんな型でも持てる own フィールドとコールバック（版は 1.0 のまま）

### 10.1 own フィールド: `own i32`・`own [u8; N]`・`own Str`

```rust
struct MyContext {
    buffer: own i32,          // 所有権付きポインタ（C では int32_t *）。構造体の解放と連動して解放される
    size: i32,
}

fn grow(c: &mut MyContext) { *c.buffer += c.size; }       // 構造体でない own の中身は `*` で読み書きする
fn consume(c: MyContext) -> i32 { *c.buffer * 10 + c.size } // 値で受け取る: 戻るときに c.buffer も解放

fn main() {
    let mut ctx = MyContext { buffer: own 38, size: 4 };
    grow(&mut ctx);                // *ctx.buffer == 42
    ctx.buffer = own 7;            // own ごと差し替え: 古い own はその場で解放
    let moved = ctx;               // 構造体まるごとの移動: ctx は空になる（もう一度使うと F0382）
    print_int(consume(moved) as i64);
    let buf = own_alloc([u8; 4096]); // 0 で埋めたヒープの配列
}
```

- `own τ` は値の型ならなんでも所有できます: 整数・`bool`・`Str`・Id・配列・Option・構造体。参照・`()`・`own own T`・関数ポインタは所有できません（F0515 / F2301）。`own_alloc(T)` も同じ型を受け付け（`own_alloc(i32)`・`own_alloc([u8; 4096])`）、own の子をすべて 0 で作ります。`own i32` のフィールドも作るので、NULL のままになることはありません。
- own（own への参照を含む）に付けた `*e` は own の中身を指します: `*ctx.buffer = 7;`・`*ctx.buffer += 1;`・`let v = *ctx.buffer;`・`&mut *ctx.buffer`。参照に付けた `*` は今までどおり何もしない（参照は自動で辿られる）ので、既存のプログラムは変わりません。構造体でない own へのメソッド呼び出しと `match` / `if let` は中身に対して働きます（`b.take()`・`buf.len()`）。
- own フィールドを持つ構造体は移動専用です。移動すると C では移動元を空にする（`__builtin_memset(&c, 0, sizeof c)`）ので、その解放は何もしません。スコープの終わり・上書き・`drop(x)`・アリーナの free で解放処理が走り、中の own をすべて解放します（`fsc_drop_MyContext` → `fsc_own_drop_T_i32(p->buffer)` → `fascon_own_free`）。深い複製は `.clone()`（`fsc_own_clone_T_i32`）。1 つのブロックへの詰め込み（own_alloc、構造体の子を持つ `own S { .. }`）とフレームリージョンは今までどおりです。
- F0382 は、構造体まるごとの移動の後のフィールドの読み出し・書き込み・`*` を通した書き込みを、すべての経路（分岐・ループ・`break`・`return`・関数ポインタ）で拒否します。F0507 は own フィールドの取り出し（`let b = c.buffer;`）と、`*b` からコピーできない値を取り出すことを拒否します。この 2 つで二重解放は書けなくなります。

### 10.2 関数ポインタ・高階関数・stack_only なコールバック

```rust
fn for_each(a: &mut [i32; 4], f: fn(&mut i32)) {
    for i in 0..4 { f(&mut a[i]); }          // a[i] への参照を 1 回の呼び出しの間だけ貸す
}
fn add_one(x: &mut i32) { *x += 1; }

struct Handler { name: Str, on_click: fn(&mut Button) }
static OPS: [fn(&mut i32); 2] = [add_one, square];

for_each(&mut a, add_one);                  // 関数の名前を値として使うと関数ポインタ
(h.on_click)(&mut b);                       // フィールドの関数ポインタは括弧で囲んで呼ぶ
OPS[1](&mut a[0]);
```

- 型: `fn(T1, &mut T2) -> R`。引数は引数の位置なので参照を書けます。戻り値には書けません（F0515）。配列は値渡し・値返しできません（F2040）。関数ポインタはコピーできる値で、ローカル・引数・構造体のフィールド・`Option<fn(..)>`（C では NULL = None）・static の表に置けます。C では型ごとに typedef（`typedef void (*fsc_fn1)(int32_t *);`）になります。
- 呼び出し: 関数ポインタ型のローカル・引数・static は名前で呼びます（`f(x)`。同じ名前の関数を隠すのは、そのローカルが関数ポインタのときだけ）。それ以外の式は `(式)(引数)` / `ops[i](x)` で呼びます。呼ばれる式 → 引数 → コミット点 → 呼び出しの順に評価し、`&mut @a[id]` を渡したら直接の呼び出しと同じく直後に列の写しを作り直します。
- 候補（`src/callback.rs`）: 関数ポインタの値は関数の名前からしか生まれないので、間接呼び出しが届く関数は「その型で値として使われた関数」に必ず含まれます。引数 `f: fn(..)` については、直接の呼び出しで実際に渡される関数に絞ります（最小不動点）。効果・F2502・バンプアリーナの呼び出しリージョンはこの候補を使います。高階関数が参照を貸しているアリーナをコールバックが触りうるとき、F2502 はその関数を名指しします（`the callback f (which may be reap) may free slots of tasks ...`）。
- stack_only と F0520（`src/escape.rs`）: 参照の引数を持ち、値として使われた関数はコールバックで、受け取った参照を自分のフレームの中で消費しなければなりません。関数と参照引数ごとに、逃げ道の要約を呼び出しグラフ上の最小不動点として求めます。参照から作った参照（`&mut x.f`・その先の場所の束縛）、包んだ値（`Some(x)`・構造体や配列のリテラル・`own x`）、束縛したローカル、呼び出し先を通る道を追い、戻り値・アリーナ・static・MMIO・ポート・別の参照の先・own の所有リージョン・（関数や関数ポインタの）値渡しの引数へ流れれば F0520 とし、流れ全体を根本原因のタイムラインとして示します。同じ場所の F0515 / F0308 は F0520 にまとめます。参照の先から取り出した所有権（`x.next.take()`）は参照ではないので保存できます。

```text
error[F0520]: 参照のエスケープ制約違反: コールバック `stash` が受け取った参照 `h: &mut Holder` を外へ逃がしています — アリーナ `keep` に格納される（スロットは解放されるまで、呼び出しより長く生きる）
  --> f0520_callback_arena.fsc:8:5
   = 17:30: `stash` は高階関数 `for_each_holder` の引数 `f` へコールバック（`fn(&mut Holder)`）として渡される
   = 7:10: `h: &mut Holder` は第二級参照: 高階関数が 1 回の呼び出しの間だけ貸す（stack_only: このフレームの中で読み書きして消費する）
   = 8:16: `Saved { .. }` のフィールド `p` に入る
```

- 参照は第二級なので、型の通るプログラムはもともと stack_only です。F0520 はそこから外れる書き方を、コールバックの文脈で指摘する独立した検査です。新しいエラーは F0520 と F0618（関数でないものの呼び出し）。F2301 は「own が所有できないものへの own」を意味するようになり、F0515 は参照の `own x` と関数ポインタ型の戻り値の参照も扱います。
- 既存のプログラムの生成 C は同じです（`tools/compare_c.py`、注釈・空白を除く）。

| ファイル | 変更点 |
| --- | --- |
| `src/ast.rs` / `src/parser.rs` | `TypeExpr::Fn`（`fn(..) -> R`）・`ExprKind::CallIndirect`（`(e)(args)`）・`*` の印（`Program::derefs`） |
| `src/types.rs` | `Ty::Fn`、どんな値の型でも持てる `own`（`Tables::ownable`）、`holds_ref` |
| `src/sema.rs` | どんな値の型でも持てる own、own への `*`、関数の値（`Res::Fn`）、関数ポインタを通した呼び出し（`CallKind::Indirect`）、構造体でない型の own_alloc、static より先に関数の型 |
| `src/callback.rs`（新規） | 間接呼び出しと関数ポインタ型の引数の候補 |
| `src/escape.rs`（新規） | stack_only のエスケープ解析と、タイムライン付きの F0520 |
| `src/flow.rs` | 間接呼び出し（移動・借用・効果・コールバックを名指しする F2502）、`*b` の F0382 / F0507 |
| `src/codegen.rs` | 関数ポインタの typedef・間接呼び出し・static の表・構造体でない own の確保 / 解放 / 複製 / 0 埋め・`*` |
| `src/strict.rs` / `src/dict.rs` / `src/driver.rs` | 新しい構文の厳格形・辞書（`fn(T) -> R`・`stack_only`・F0520・F0618）・F0520 の解説 |

テスト（`--qemu` 付きで 136 件）: `tests/ok/own_fields.fsc`（own i32 / 配列 / Str / Option のフィールド・移動・複製・own_alloc・アリーナの free・解放漏れなし）、`tests/ok/callbacks.fsc`（高階関数・フィールド・static の表・Option<fn>・アリーナのコールバック・コールバックによる own の子の取り外し）、`examples/own_callbacks.fsc`（PC・BIOS なし AArch64・x86_64 の仮想 BIOS）、エラー 16 件（F0520 の 6 つの形・F0382 3 件・F0507 2 件・コールバックを通した F2502・F0618・F0308・F0515・F2301）。新しい 2 本のプログラムは `-DFASCON2_HEAP_MALLOC` で AddressSanitizer / UBSan / LeakSanitizer の下でも実行しました（エラー・解放漏れなし）。
