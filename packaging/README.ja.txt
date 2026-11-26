Fascon v1.0.0 — ライフタイム注釈のない Rust 風の構文 → 読みやすく最適化しやすい C
(English: README.en.txt)

1.0 の新機能: 世代タグ付き 32 ビット Id のアリーナ（Id<@a> = 世代 32-K | 添字 K、K は容量から決まる）、一括で解放される own T の
ツリー、コンパイル時に境界を検査する固定番地の MMIO リージョン、QEMU での BIOS なし AArch64 起動。
0.1 の機能だけを使うファイルはこれまでどおり動きます（fascon が同梱の 0.1.0 エンジン fascon-core に回します）。
2026-11 の改修（版は同じ）: 分岐のない表の走査（生存ビットマップ + ctz、マスク付きの個数・総和・最小 / 最大）、
#[repr(align(N))]、キャッシュラインを意識した表の配置、2 MiB 境界のアリーナ（BSP は 1 GiB / 2 MiB ブロック、Linux は madvise）、
外へ出ない own のためのフレームリージョン。詳しくは docs/PERF-1.0.ja.md。
列（版は同じ）: `for id in @a` の集計は、その集計が読む値だけの列（32 バイトのスロットではなく 1 タスク 8 / 4 バイトか 1 ビット）
を読むので、総和・条件付き総和・個数・最小 / 最大の選択が 1 タスク 1 クロック未満になります。列は書き込みのたびに同期します。
--columns off で前の表に戻ります。fascon run はこの CPU 向けにコンパイルします（--cpu baseline で基本の命令だけ）。
修正（版は同じ）: 構造体・配列・Option<own T> の場所の全体を match / if let で束縛すると（例: `match @w[id].p { q => .. }`）
壊れた C が出ていた不具合を直しました。束縛は場所を指します。参照を通して配列全体を値として使うと配列全体を写します。
own の場所の束縛を通した own の差し替えはエラー F0507 です。詳しくは VS Code 拡張機能の CHANGELOG.md にあります。
拡張（版は 1.0 のまま）: 型推論（`let task = create_process(100);` に型は不要）、`@` の廃止（`pcb_table[id].id = 100;` は
世代タグで検証する .get_mut(id)）、宣言の順序は自由（2 パス解析）、bump_arena（関数の出口で O(1) の一括解放）、
port_io（x86_64 では out dx, al、AArch64 では MMIO へ写す）、own_alloc(型名)、ふつうの Rust 構文の自動リフレーミング
（#![reframing(rust)] / --reframing=rust）、機能安全証明形態の厳格な Fascon（--emit-strict-fascon）、x86_64 と AArch64 の
Fascon 仮想 BIOS（1 つのソースから 2 つの -bios の像）。既存のプログラムの生成 C は同じです。詳しくは docs/SPEC-1.0-EXT.ja.md。

■ 中身
  fascon(.exe)          1.0 のコンパイラ・言語サーバー・QEMU 起動ツール（1 ファイル）
  fascon-core(.exe)     0.1 だけの構文のファイル用の 0.1.0 エンジン（fascon と同じフォルダに置く）
  include/fascon2_rt.h  1.0 の生成 C が使うランタイムのヘッダ（fascon build が build/ にも書き出す）
  include/fascon_rt.h   0.1 の生成 C が使うランタイムのヘッダ
  bsp/qemu-virt-aarch64/
                        QEMU virt AArch64 の 1.0 の起動コード（start.S・bsp.c・kernel.ld）と仮想 BIOS（firmware.S・firmware.ld）
  bsp/qemu-x86_64-bios/ QEMU pc（x86_64）の Fascon 仮想 BIOS（bios.S・bsp.c・bios.ld）: 4 コア・LAPIC タイマー・
                        タスクごとの CR3 の Ring 3 のプロセス・IPI の撃ち落とし・NMI によるパニック時の全コア停止
  bsp/user-aarch64/・bsp/user-x86_64/
                        ユーザープログラム（EL0 / Ring 3）の起動コード（fascon build --user で使う）
  bsp/portable-stub/    移植用の参照プラットフォーム層（ARMv7・MIPS64・RISC-V64 ほか。tools/cross_check.py）
  bsp/host/fascon2_plat_host.c
                        1.0 の生成 C を PC で動かすためのプラットフォーム層
  bsp/generic-aarch64/  実機の像のための汎用 AArch64 BSP（start.S・bsp.c。fascon build --board）
  boards/               ボードのパッケージ: raspberrypi3b（スピンテーブル・VideoCore のメールボックス）、qemu-virt-generic（PSCI）、
                        qemu-virt-riscv64（自前の start.S / bsp.c）と、装置の窓の定義（*.toml）
  lib/                  純粋な Fascon の標準ライブラリ（sync・collections・net・fs・ipc・loader・pm・hal）
  data/                 エラー辞書・解説の規則・修正規則（JSON。FASCON_DATA で上書き）
  examples/             kernel_core.fsc（ミッション1）、gui_desktop.fsc（ミッション2）、gui_dual.fsc（両アーキテクチャの GUI）、
                        bios_dual.fsc（両アーキテクチャの BIOS）、rust_style.rs（ふつうの Rust 構文）、app_wordfreq.fsc（一般アプリ）、
                        vruntime_scan.fsc、errors/（エラーコードごとの例）、0.1 の例（*.rs）
  runtime/<target>/libmas_runtime.a、bsp-examples/、lsp-config/
                        0.1.0 から変わらず（0.1 のファイル用。docs/USAGE.ja.md を参照）
  docs/                 SPEC-1.0.ja.md（仕様書）、SPEC-1.0-EXT.ja.md（拡張の仕様）、USAGE-1.0.ja.md（使い方）、PERF-1.0.ja.md（2026-11 の改修）、
                        0.1 の説明書。英語版は *.md
                        SPEC-1.0-KERNEL.ja.md と kernel-design/ はカーネル拡張 Step 1〜4（実装済み。この版のコンパイラで通ります）
                        SPEC-1.0-SMP.ja.md・SPEC-1.0-UNIVERSAL.ja.md は マルチコア（SMP）拡張（Fascon 1.0）（実装済み。配布物の版の表記は 1.0.0 のまま）
                        SPEC-1.0-INTEGRATION.ja.md / .md: 割り込み・タスク・ユーザー空間つきのマルチコアのカーネル、4 KiB の要求時ページング、
                        パニック時の全コア停止、fascon-gc の根、マルチコアの x86_64 仮想 BIOS（2026-11）

■ はじめの一歩
  fascon doctor                                       入っているもの・足りないものを表示
  fascon qemu examples/kernel_core.fsc                ミッション1: BIOS なしで起動し FASCON-KERNEL-OK を表示
  fascon qemu examples/kernel_user.fsc --headless     カーネル拡張 Step 4: EL0 のタスク 4 本とシステムコール（FASCON-USER-OK）
  fascon qemu examples/smp_kernel.fsc --headless      SMP 拡張: 4 コア（-smp 4）で FASCON-SMP-OK。fascon run なら PC のスレッドで動く
  fascon qemu examples/gui_desktop.fsc                ミッション2: QEMU の ramfb にボタン 2 個のウィンドウ
                                                      （ターミナルでキーを押すと Cancel、終了は QEMU のウィンドウを閉じる）
  fascon qemu examples/gui_desktop.fsc --screenshot shot.png --frames 3
                                                      ウィンドウなしで shot-1.png〜shot-3.png を保存
  fascon run examples/gui_desktop.fsc                 この PC で実行し、画面を build/gui_desktop.png に保存
  fascon build examples/kernel_core.fsc               build/kernel_core.fascon.c と build/fascon2_rt.h
  fascon regions examples/kernel_core.fsc             アリーナ・own・MMIO と、静的に省いた検査の数
  fascon explain examples/errors/f2101_color.fsc      根本原因・直し方・数理的解説
  fascon qemu examples/bios_dual.fsc --target x86_64  x86_64 の Fascon 仮想 BIOS から起動（COM1 = out dx, al）
  fascon qemu examples/bios_dual.fsc --target aarch64 AArch64 の Fascon 仮想 BIOS から起動（同じソース）
  fascon bios examples/bios_dual.fsc                  1 つのソースから build/bios_dual.x86_64.bios.bin と .aarch64.bios.bin
  fascon qemu examples/gui_dual.fsc --target x86_64   同じ GUI を x86_64 で（--target aarch64 で AArch64）
  fascon run examples/app_wordfreq.fsc                一般アプリ（単語の出現回数。bump_arena・own_alloc・型推論）
  fascon run examples/rust_style.rs                   ふつうの Rust の構文のまま実行（自動リフレーミング）
  fascon build examples/rust_style.rs --emit-strict-fascon
                                                      build/rust_style.strict.fsc に厳格な Fascon（すべての型を明示）
  fascon --lang en ...                                英語で表示（環境変数 FASCON_LANG=en でも可）

■ 自分で入れるツール
  QEMU での起動には QEMU と LLVM（clang + ld.lld）が要ります:
    Windows   winget install --id SoftwareFreedomConservancy.QEMU -e
              winget install --id LLVM.LLVM -e
    macOS     brew install llvm lld qemu
    Linux     sudo apt install clang lld qemu-system-arm qemu-system-x86
  PC での実行（fascon run）には C コンパイラが要ります: Windows は MSYS2 の gcc、macOS は xcode-select --install。
  fascon は C:\Program Files\qemu、C:\Program Files\LLVM\bin、C:\msys64\ucrt64\bin、Homebrew も探します。

■ VS Code
  fascon-1.0.0.vsix を入れてください（拡張機能ビュー →「…」→ VSIX からのインストール）。Windows・Linux・macOS 用の
  実行ファイルが入っているので、VS Code で使うだけならこのアーカイブのほかのファイルは要りません。

■ 正直な限界（詳しくは docs/SPEC-1.0.ja.md の「保証の範囲と限界」）
  コンパイル時の検査の対象は Fascon で書いたコードです。ハードウェアの故障（RAM のビット反転）、BSP とランタイムの
  手書きの C・アセンブリ、実機と食い違う mmio 宣言は対象外です。スタックの深さは制限していません。
  アリーナは有限（約 21〜43 億回の確保まで）で、証明できない Id のフォールトはアクセスの場所ではなく
  次のコミット点（呼び出し・出力・戻りの直前）で止まります。
  bump_arena は容量を超えると確保が None になり、巻き戻した後の古い Id は None か安全な停止になります。
  仮想 BIOS は QEMU（pc / virt）用です。実機の起動には、その機械に合わせた起動コードが要ります。

■ macOS
  実行ファイルにはコード署名がありません。「開発元を検証できません」と出たらターミナルで:
    xattr -d com.apple.quarantine ./fascon ./fascon-core
■ Linux
  静的リンク（musl）なので、どのディストリビューションでも動きます。x64 版と arm64 版があります。
  先に `export PATH=$PWD/linux-x64:$PATH` を実行してください（ARM の PC は linux-arm64）。
