# Fascon 1.0 の使い方

English: [USAGE-1.0.md](USAGE-1.0.md) · 言語の仕様: [SPEC-1.0.ja.md](SPEC-1.0.ja.md) · 拡張の仕様: [SPEC-1.0-EXT.ja.md](SPEC-1.0-EXT.ja.md) · 0.1 の使い方: 各プラットフォームのアーカイブの `docs/USAGE.ja.md`（ソースツリーにはありません）

## 1. 何が入っているか

| ファイル | 役割 |
| --- | --- |
| `fascon`（Windows は `fascon.exe`） | 1.0 のコンパイラ・言語サーバー・QEMU 起動ツール。1 つのファイルで動き、ほかに入れるものはありません |
| `fascon-core`（`fascon-core.exe`） | 0.1.0 のエンジン。`fascon` と同じフォルダに置くと、0.1 だけの構文のファイルを自動でこちらに回します |
| `include/fascon2_rt.h` | 1.0 の生成 C が使うランタイム（ヘッダ 1 つだけ。`fascon build` が出力先にも書き出します） |
| `bsp/qemu-virt-aarch64/` | QEMU virt（AArch64）の起動コード（`start.S`・`bsp.c`・`kernel.ld`、仮想 BIOS の `firmware.S`・`firmware.ld`） |
| `bsp/qemu-x86_64-bios/` | QEMU pc（x86_64）の仮想 BIOS（`bios.S`: リセットベクタ → ロングモード、`bsp.c`、`bios.ld`） |
| `bsp/host/fascon2_plat_host.c` | 生成 C を PC 上で動かすためのプラットフォーム層 |
| `examples/` | 1.0 の例（`kernel_core.fsc`・`gui_desktop.fsc`・`vruntime_scan.fsc`・`gui_dual.fsc`・`bios_dual.fsc`・`rust_style.rs`・`app_wordfreq.fsc`）、エラーの例（`errors/`）、0.1 の例（`*.rs`） |

## 2. 準備（最初の 1 回だけ）

`fascon doctor` で、足りないものとその入れ方が表示されます。

| 用途 | Windows | macOS | Linux（Debian / Ubuntu） |
| --- | --- | --- | --- |
| QEMU で起動する（`fascon qemu`・`fascon bios`） | `winget install --id LLVM.LLVM -e` と `winget install --id SoftwareFreedomConservancy.QEMU -e` | `brew install llvm lld qemu` | `sudo apt install clang lld qemu-system-arm qemu-system-x86` |
| PC で実行する（`fascon run`） | MSYS2 の gcc | `xcode-select --install` | `sudo apt install gcc` |

`fascon` は `PATH` に加えて、よくあるインストール先（`C:\Program Files\qemu`、`C:\Program Files\LLVM\bin`、`C:\msys64\ucrt64\bin`、Homebrew）も探します。

## 3. VS Code で使う

1. 拡張機能ビュー（Ctrl+Shift+X）の「…」→「VSIX からのインストール…」で `fascon-1.0.0.vsix` を入れます。
2. コマンドパレット（Ctrl+Shift+P）で「Fascon:」と打つと、次のコマンドが出ます。

| コマンド | 内容 |
| --- | --- |
| Fascon: 1.0 の例を開く（カーネル・GUI・両アーキテクチャ・BIOS・Rust 風・アプリ・コールバック） | カーネル・GUI・両アーキテクチャの GUI・両アーキテクチャの BIOS・Rust 風・一般アプリ・vruntime 集計の例を保存して開く |
| Fascon: QEMU で起動（ターゲットは設定に従う。portable = BIOS なし AArch64） | Ctrl+Alt+Q。`fascon.target` が portable なら BIOS なし AArch64、x86_64 / aarch64 なら Fascon の仮想 BIOS。画面は QEMU のウィンドウ、キー入力は VS Code のターミナル |
| Fascon: QEMU で起動（x86_64・Fascon の仮想 BIOS） | Ctrl+Alt+X |
| Fascon: QEMU で起動（AArch64・Fascon の仮想 BIOS） | フラッシュの 0 番地から起動 |
| Fascon: x86_64 と AArch64 の BIOS の像を作る（1 つのソースから） | `build/<名前>.x86_64.bios.bin` と `build/<名前>.aarch64.bios.bin` |
| Fascon: 厳格な Fascon（機能安全証明形態）を表示 | `build/<名前>.strict.fsc` を書き出して横に開く |
| Fascon: このワークスペースで自動リフレーミング（ふつうの Rust 構文）を切り替え | ふつうの Rust 構文を受け付けるか（`fascon.reframing`） |
| Fascon: QEMU で起動して画面を保存（ヘッドレス・ターゲットは設定に従う） | ウィンドウを出さずに起動し、`FASCON-GUI-READY` の行ごとに PNG を保存して開く |
| Fascon: この PC で実行（画面は PNG に保存） | 生成 C を PC 用にビルドして実行し、画面を `build/<名前>.png` に保存 |
| Fascon: C コードを生成（build/<名前>.fascon.c） | `build/<名前>.fascon.c` と `fascon2_rt.h` を書き出す（元ファイルは上書きしない） |
| Fascon: エラーを解説（タイムライン・修正コード・数理的解説） | Ctrl+Alt+E。根本原因・直し方・数理的解説 |
| Fascon: リージョン確定結果を表示 | アリーナ・own・MMIO と、静的に省いた検査の数 |
| Fascon: 辞書を検索（キーワード・エラーコード・第二級参照） | Ctrl+Alt+D。0.1 と 1.0 の全項目 |
| Fascon: バージョンと必要なツールを確認 | `fascon version` と `fascon doctor` の結果 |

設定（`fascon.*`）:

| 設定 | 既定 | 内容 |
| --- | --- | --- |
| `fascon.language` | auto | 表示言語（auto / ja / en） |
| `fascon.outDir` | build | C・ELF・画像の出力先（ワークスペースからの相対パス） |
| `fascon.extraPath` | [] | QEMU や C コンパイラを動かすときに PATH へ足すフォルダ |
| `fascon.screenshotFrames` | 3 | 「Fascon: QEMU で起動して画面を保存（ヘッドレス・ターゲットは設定に従う）」で保存する枚数 |
| `fascon.qemuTimeout` | 90 | 画面なしの QEMU 実行の制限時間（秒） |
| `fascon.ptrBits` | 64 | ターゲットのポインタ幅 |
| `fascon.serverPath` | （空） | 別の `fascon` 実行ファイルを使う場合 |
| `fascon.columns` | true | `for id in @a` の集計ごとに、その集計が読む値だけの列を表に足す（false で 1.0.0 と同じ表。「Fascon: C コードを生成（build/<名前>.fascon.c）」に反映するには言語サーバーを再起動） |
| `fascon.cpu` | native | 「Fascon: この PC で実行（画面は PNG に保存）」の C を、この PC の CPU 向け（`native`）か、アーキテクチャの基本の命令だけ（`baseline`）でコンパイルする |
| `fascon.target` | portable | I/O ポート（port_io）の形と QEMU の起動先: `portable`（両方を `#if` で、QEMU は BIOS なし AArch64）/ `x86_64`（`in` / `out` 命令、QEMU pc + 仮想 BIOS）/ `aarch64`（MMIO へ写す、QEMU virt + 仮想 BIOS） |
| `fascon.reframing` | auto | 自動リフレーミング: `auto`（ファイル先頭の `#![reframing(rust)]` に従う）/ `rust`（常にオン）/ `off` |
| `fascon.emitStrictOnBuild` | false | 「Fascon: C コードを生成（build/<名前>.fascon.c）」で `<名前>.strict.fsc`（厳格な Fascon）も書き出す |

## 4. コマンドで使う

```text
fascon build <file> [--out-dir D]      C を生成（D/<名前>.fascon.c と D/fascon2_rt.h）
fascon check <file> [--json]           検査だけ（エラーがあれば終了コード 1）
fascon explain <file> [--line N] [--code F2101]
fascon fix <file> [--in-place]         検証済みの自動修正（既定の出力: <名前>.fixed.fsc）
fascon regions <file>                  リージョンの確定結果、アリーナごとの Id の分け方（K）と寿命、検査の扱い
fascon run <file> [--screenshot out.png] [--cpu native|baseline]
fascon qemu <file> [--target x86_64|aarch64] [--headless] [--screenshot out.png] [--frames N] [--timeout 秒] [--build-only] [--cc gcc|clang]
fascon bios <file> [--target x86_64|aarch64]   1 つのソースから x86_64 と AArch64 の仮想 BIOS の像（*.bios.bin）を作る
fascon example [kernel|gui|dual|bios|rust|app] [--out <file>]
fascon bsp [--out <dir>]               起動コード一式（AArch64 と x86_64 の仮想 BIOS）を書き出す
fascon runtime-header [--out <dir>]
fascon dict [語] [--json|--markdown]   仕様辞書（名前・別名の完全一致を先に出す。`--` で始まる語は `fascon dict -- --target`）
fascon doctor
fascon lsp                             言語サーバー（stdio）
```

共通オプション: `--lang ja|en`、`--engine auto|1|0.1`、`--out-dir <dir>`、`--ptr-bits 32|64`、`--columns on|mirrored|off`、`--nontemporal auto|on|off`、`--fast-math`、`--gpu=on|off`、`--target x86_64|aarch64|portable|rtos|generic-os|hpc`、`--topology cores=N,smt=K,numa=M`、`--reframing rust|off`、`--emit-strict-fascon`（`--名前=値` の形でも書けます）。

- `--target x86_64|aarch64|portable`（既定 portable。環境変数 `FASCON_TARGET`）: `port_io` の読み書きの C の形。`x86_64` は `in` / `out` 命令（`com1 = c;` → `out dx, al`）、`aarch64` はコンパイラが MMIO へ写す（COM1 → PL011）、`portable` は両方と PC 実行を `#if` で選ぶ。`fascon qemu` では起動先も決まります（x86_64 / aarch64 は Fascon の仮想 BIOS）。
- `--reframing rust|off`（既定: ファイル先頭の `#![reframing(rust)]` に従う。環境変数 `FASCON_REFRAMING`）: ふつうの Rust の構文を Fascon へ書き換えてからコンパイルする（docs/SPEC-1.0-EXT.ja.md §6）。
- `--emit-strict-fascon`（`build` / `check` / `qemu` / `bios`）: 機能安全証明形態の厳格な Fascon を `<出力フォルダ>/<名前>.strict.fsc` に書く。

- `--columns on|mirrored|off`（既定 on。環境変数 `FASCON_COLUMNS=off` / `mirrored` で既定を変えられる）: `on` では、`for id in @a` の集計（総和・条件付き総和・条件付きの個数・最小 / 最大の選択）が、スロットではなく、その集計が読む値だけを並べた列を読みます。条件を満たすフィールドはスロットから外れて列が唯一の正本になり（完全列化、True SoA: 書き込みは 1 回）、ほかは写しの列で、その値に届く書き込みが列を作り直します（書き込みの多いフィールドには写しの列を作りません）。`mirrored` はすべて写しの列（完全列化の前の 1.0 と同じ C）、`off` で 1.0.0 と同じ表と走査になります。列ごとの True-SoA (single-write) / Mirrored-SoA (dual-write) と、完全列化しなかった理由は `fascon regions` に出ます（docs/PERF-1.0.ja.md のフェーズ 4・5）。
- `--nontemporal auto|on|off`（既定 auto。環境変数 `FASCON_NONTEMPORAL`）: 完全列化の列への一括上書きのループ（本体がすべて `@a[id].f = 値;`）で非時間的ストア（x86-64 の MOVNTI など）を使うか。`auto` は列が 8 MiB 以上で、表が割り込みハンドラ・タスクと共有されず、後ろの文（次のループまで）が同じ列を読まないときだけ。`on` はいつも（大きな表の列の走査にプリフェッチも）。どちらもループの出口に `F20_SFENCE()` を置きます。`off` は使いません（docs/PERF-1.0.ja.md の 5-4）。
- `--fast-math`（環境変数 `FASCON_FAST_MATH=1`）: プログラム全体を `#[fast_math]` として扱う。浮動小数点の総和と最小 / 最大の選択が、f32 / f64 の完全列化の列の上の並列レーン（4 本のベクトル累計・IEEE-754 の順序の鍵）で走ってよくなります。付けなければ `#[fast_math]` を付けたループと関数だけで、ほかは書いた順のままビット一致です（docs/SPEC-1.0-HPC.ja.md §1）。
- `--gpu=on|off`（既定 on。環境変数 `FASCON_GPU=off`）: `off` では `gpu_arena` の dispatch が呼んだコアでその場で走り（`FASCON_GPU_KERNEL(番号)` で登録した C の関数。無ければ何もしない）、すぐ完了します。生成 C は `fascon_plat_gpu_*` を呼びません（警告 F2707）。`fascon run` はソースの隣の `<名前>.gpu.c` を一緒にビルドします（docs/SPEC-1.0-HPC.ja.md §3）。
- `--target hpc`（または `// fascon-target: hpc`）と `--topology cores=N,smt=K,numa=M[,layout=adjacent]`（または `// fascon-topology: ..`。既定 cores=128 smt=2 numa=8）: HPC のプロファイル。64 ビット Id、どの表も 2 MiB 境界（MADV_HUGEPAGE）、main と spawn_on の入口でコアを物理コアへピン留めし、コアごとのメモリをそのコアの NUMA ノードへ固定し、F3901〜F3903 を検査します。`fascon regions` に計算ノードの形とコアごとの役割が出ます（docs/SPEC-1.0-HPC.ja.md §4）。
- `--cpu native|baseline`（`fascon run`、既定 native）: `native` は生成 C を `-march=native`（x86-64）/ `-mcpu=native`（AArch64）でコンパイルし、列の走査がその PC の最も広いベクトル（AVX2 / AVX-512 / NEON）を使います。フラグを受け付けないコンパイラなら、フラグなしでやり直します。`baseline` はアーキテクチャの基本の命令だけ（x86-64 なら SSE2）で、古い CPU でも動く実行ファイルになります。

### どちらのエンジンで処理されるか

`arena` / `mmio` / `own` / `type` / `@名前` / `Id` / `Str` / `if let` / `[T; N]` のどれかを使うファイル、または先頭に `#![fascon(1)]` と書いたファイルは 1.0 エンジンが処理します。それ以外は `fascon-core`（0.1.0）に回します。`--engine 1` / `--engine 0.1` で固定できます（プレビュー版の `--engine 2` も 1.0 の意味で使えます）。`run` と `qemu` はいつも 1.0 エンジンです。

### 1 つのソースから x86_64 と AArch64 の BIOS（`fascon bios`）

```text
fascon bios examples/bios_dual.fsc
  → build/bios_dual.x86_64.bios.bin   （QEMU pc の -bios。リセットベクタ → ロングモード → カーネル）
  → build/bios_dual.aarch64.bios.bin  （QEMU virt の -bios。フラッシュ → RAM 0x4008_0000 → カーネル）
fascon qemu examples/bios_dual.fsc --target x86_64     # 作って起動
fascon qemu examples/gui_dual.fsc --target aarch64 --screenshot shot.png --frames 3
```

x86_64 の起動は `qemu-system-x86_64 -M pc -m 2G -bios <bin> -nic none -no-reboot -device isa-debug-exit,iobase=0xf4,iosize=0x04`
（画面があれば `-vga none -device ramfb`）。プログラムの終了コードは isa-debug-exit を通して QEMU が `(コード << 1) | 1` で返し、
`fascon qemu` は元のコードに戻して返します。

### `fascon qemu` がすること

1. `build/<名前>.fascon.c` を生成し、`build/bsp-qemu-virt-aarch64/` に起動コードを書き出す。
2. clang + ld.lld（無ければ aarch64 の gcc）で `build/<名前>.elf` を作る。
3. `qemu-system-aarch64 -M virt -cpu cortex-a72 -m 256M -nic none -semihosting -kernel <elf>` で起動する。`#[framebuffer]` があれば `-device ramfb` を足す。
4. `--screenshot` を付けると QEMU を画面なしで動かし、UART に `FASCON-GUI-READY` が出るたびに QMP の screendump で画面を保存してから、キー入力（`n`）を送ります。

プログラムが `exit(code)` するか `main` から戻ると、QEMU は semihosting で終了し、その終了コードを返します。

### 生成 C の切り替え

| 切り替え | 効果 |
| --- | --- |
| `FASCON_NO_HUGEPAGE=1`（環境変数、Linux の `fascon run`） | 1 MiB 以上のアリーナに 2 MiB ページを頼まない（既定は `madvise(MADV_HUGEPAGE)` を掛ける） |
| `-DFASCON2_PICK_BRANCHLESS`（C コンパイラ） | マスク付き集計の最小 / 最大の更新も条件付き選択（CSEL / CMOV）にし、データに依存する分岐をなくす。平均では遅くなる（docs/PERF-1.0.ja.md） |
| `-DFASCON2_FRAME_SIZE=<バイト数>`（C コンパイラ） | PC 上での、外へ出ない `own` のためのフレームリージョンの大きさ（既定 64 KiB。QEMU の BSP はリンカスクリプトの 1 MiB の `.frame` を使う） |
| `-DF20_VECTOR_BYTES=16\|32\|64`（C コンパイラ） | 列の走査のベクトルの幅（既定はターゲット本来の幅: AVX-512VL で 64、AVX2 で 32、それ以外は 16） |
| `-DF20_VEC_I64_CMP=0\|1`（C コンパイラ） | 鍵の列の最小 / 最大の選択に、64 ビットのベクトル比較を使うか（既定は SSE4.2 / AVX / AArch64 で 1。0 はスカラの CMOV / CSEL の鎖 4 本） |
| `FASCON_HPC_NO_PIN=1`（環境変数、PC の `--target hpc`） | コアのピン留めと NUMA ノードへの固定をしない（どちらも助言なので、拒まれてもプログラムは止まらない） |
| `-DFASCON2_GPU_KERNELS_MAX=<n>`（C コンパイラ） | 登録簿に置ける `FASCON_GPU_KERNEL(番号)` のカーネルの数（既定 64） |
| `-DFASCON2_BOARD_GPU` / `-DFASCON2_BOARD_HPC`（C コンパイラ、ベアメタル） | ボードのパッケージが `fascon_plat_gpu_*` / HPC のフックを自分で持つ（無ければ、ベアメタルでは dispatch はその場で走り、ピン留めはしない） |

## 5. 困ったとき

| 症状 | 対処 |
| --- | --- |
| `qemu-system-aarch64 が見つかりません` | QEMU を入れる。入れた場所が違うときは `fascon.extraPath` に追加 |
| `AArch64 向けの C コンパイラが見つかりません` | LLVM（clang と ld.lld）を入れる |
| `0.1 エンジン（fascon-core）が見つかりません` | `fascon-core` を `fascon` と同じフォルダに置くか、環境変数 `FASCON_CORE` で場所を指定 |
| macOS で「開発元を検証できません」 | `xattr -d com.apple.quarantine fascon fascon-core` |
| `FASCON PANIC 0x000d` | 解放済みのスロットを指す Id で `@a[id]` を使った（`if let Some(x) = @a.get(id)` なら None で受けられる） |
| `FASCON PANIC 0x000c` | 証明できなかった添字が範囲外だった |
| `FASCON PANIC 0x0001` | own のヒープ（既定 8 MiB）が足りない |
| `qemu-system-x86_64 が見つかりません` | QEMU を入れる（Linux は `qemu-system-x86` パッケージ） |
| `x86_64 向けの C コンパイラが見つかりません` | LLVM（clang と ld.lld）を入れる（1 つで x86_64 と AArch64 の両方に使える） |
| `CPU EXCEPTION 13 (#GP general protection)`（x86_64） | 仮想 BIOS の例外表示。RIP をもとに `build/<名前>.x86_64.elf` を `llvm-objdump -d` で調べる |
| F0282 | 型が決まらないままフィールド・メソッド・添字の土台に使われた。その let に型を 1 つ書く |
| F2401 / F2402 | bump_arena を 1 つずつ解放しようとした / その Id を長く生きる表・static に置こうとした（`arena` にする） |
| `FASCON PANIC 0x0014` | `gpu_arena` の dispatch ができなかった: その番号のカーネルが登録されていない（`<名前>.gpu.c` の `FASCON_GPU_KERNEL(番号)`）か、プラットフォームがスロットの配列を受け付けなかった |
| `[host] gpu: no kernel registered with id N` | ソースの隣の `<名前>.gpu.c` に `FASCON_GPU_KERNEL(N) { .. }` を足す（または `--gpu=off` で走らせる） |
| F2705 / F2706 | gpu_arena の要素に own・Id・Str・bool・Option・参照・関数ポインタがある、または別の gpu_arena のハンドル・引換券を使った |
| F3901 / F3902 / F3903（`--target hpc`） | 計算コアから I/O・動的な確保・写しの表ごとの作り直しが届く、または計算ノードの形の外のコア・NUMA ノード（`fascon regions` の HPC の節を見る） |

## 6. ソースからビルドする

```text
cargo build --release                       # target/release/fascon（Rust 1.74 以降、外部クレートなし）
python3 tests/run_tests.py target/release/fascon --qemu
```

`tests/run_tests.py` は、エラー検出・実行結果・列のテスト（`tests/cols/cols.fsc` を `--columns on`・`mirrored`・`off` で）・完全列化のテスト（`tests/ok/tsoa_*.fsc`。浮動小数点と条件付き一括更新のものも含め、`--columns` と `--nontemporal` のすべての組み合わせで）・条件付き一括更新の走査関数とスカラの模型の比較（-O2 と ASan / UBSan）・浮動小数点の出力（ビットの並み 40 万個）・gpu_arena の真似の ASan / UBSan と TSan・`--target hpc` の C と報告・`tests/cols/colfuzz.py` が作るランダムなプログラム（`--tsoa`・`--float`・`--rmw` のものも）をそれぞれの設定で走らせた比較（`--fuzz N`、既定 8 本）・例（PC での実行と、BIOS なし QEMU での起動）・厳格形の往復・`--target` ごとの C の形、`--qemu` では x86_64 と AArch64 の仮想 BIOS での起動（GUI の画面とキー入力を含む）を確かめます。既存のすべてのプログラム（例とテストを、列の既定・mirrored・off の 3 つのモードで）の生成 C が `tests/golden_c.json` に記録したものと同じか（注釈と空白を除く）は `python3 tools/compare_c.py` で、2 つの版の直接の比較は `python3 tools/compare_c.py <新しい fascon> <古い fascon> <file>...` で確かめられます。ベンチマークは `bench/RESULTS.md` を参照してください。
