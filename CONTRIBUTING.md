# Contributing / 貢献について

## English

- **Pull requests are generally not accepted.** Fascon 1.0 is developed by its author in this repository, and whether a
  change comes into it is decided by the author alone.
- **The compiler core is maintained by the author.** The main compiler logic (`src/`) is designed and maintained by the
  author, so please do not send changes to it. This is a statement of the project's intent, not an extra condition of the
  license: MIT OR Apache-2.0 still lets you change your own copy. If you publish a modified compiler, please say clearly
  that it is a modified version and not the official Fascon.
- **Forks and extensions are welcome.** The license (MIT OR Apache-2.0, at your option; `LICENSE-MIT`, `LICENSE-APACHE`)
  lets you fork, modify and redistribute Fascon.
- Most extensions need no change to the compiler. Drivers, libraries and boards are written in pure Fascon:
  modules and the standard HAL under `lib/`, board packages under `boards/<name>/` (`board.toml` and `board_hal.fsc`),
  and the data files `data/dictionary.json`, `data/rules.json`, `data/fixes.json`. See sections 6 and 7 of
  [docs/SPEC-1.0-UNIVERSAL.md](docs/SPEC-1.0-UNIVERSAL.md).
- In a fork, `python3 tests/run_tests.py target/release/fascon` runs the test suite, and `python3 tools/compare_c.py`
  checks that the generated C of the existing programs has not changed.

## 日本語

- **プルリクエストは原則として受け付けていません。** Fascon 1.0 は作者がこのリポジトリで開発しており、どの変更を取り込むかは
  作者だけが判断します。
- **コンパイラの中核は作者が保守します。** コンパイラの主要なロジック（`src/`）は作者が設計・保守しているので、変更を送らない
  でください。これはプロジェクトの趣旨の表明で、ライセンスに条件を足すものではありません。MIT OR Apache-2.0 のもとで、
  手元のコピーを改変することはできます。改変したコンパイラを公開する場合は、公式の Fascon ではなく改変版であることを
  はっきり書いてください。
- **フォークと拡張は歓迎します。** ライセンス（MIT OR Apache-2.0 のどちらか好きな方。`LICENSE-MIT`・`LICENSE-APACHE`）の
  範囲で、自由にフォーク・改変・再配布できます。
- たいていの拡張はコンパイラを変えずに足せます。ドライバ・ライブラリ・ボードは純粋な Fascon で書けます:
  `lib/` のモジュールと標準 HAL、`boards/<名前>/` のボードのパッケージ（`board.toml` と `board_hal.fsc`）、
  データファイル `data/dictionary.json`・`data/rules.json`・`data/fixes.json`。
  [docs/SPEC-1.0-UNIVERSAL.ja.md](docs/SPEC-1.0-UNIVERSAL.ja.md) の 6 節と 7 節を見てください。
- フォークでは `python3 tests/run_tests.py target/release/fascon` でテスト一式が走り、`python3 tools/compare_c.py` で
  既存のプログラムの生成 C が変わっていないことを確かめられます。
