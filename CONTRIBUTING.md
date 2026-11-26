# Contributing / 貢献について

## English

- **Pull requests are not accepted.** Fascon 1.0 is developed by its author in this repository.
- **Forks and extensions are welcome.** The license (MIT OR Apache-2.0, at your option; `LICENSE-MIT`, `LICENSE-APACHE`)
  lets you fork, modify and redistribute Fascon.
- Most extensions need no change to the compiler. Drivers, libraries and boards are written in pure Fascon:
  modules and the standard HAL under `lib/`, board packages under `boards/<name>/` (`board.toml` and `board_hal.fsc`),
  and the data files `data/dictionary.json`, `data/rules.json`, `data/fixes.json`. See sections 6 and 7 of
  [docs/SPEC-1.0-UNIVERSAL.md](docs/SPEC-1.0-UNIVERSAL.md).
- In a fork, `python3 tests/run_tests.py target/release/fascon` runs the test suite, and `python3 tools/compare_c.py`
  checks that the generated C of the existing programs has not changed.

## 日本語

- **プルリクエストは受け付けていません。** Fascon 1.0 は作者がこのリポジトリで開発しています。
- **フォークと拡張は歓迎します。** ライセンス（MIT OR Apache-2.0 のどちらか好きな方。`LICENSE-MIT`・`LICENSE-APACHE`）の
  範囲で、自由にフォーク・改変・再配布できます。
- たいていの拡張はコンパイラを変えずに足せます。ドライバ・ライブラリ・ボードは純粋な Fascon で書けます:
  `lib/` のモジュールと標準 HAL、`boards/<名前>/` のボードのパッケージ（`board.toml` と `board_hal.fsc`）、
  データファイル `data/dictionary.json`・`data/rules.json`・`data/fixes.json`。
  [docs/SPEC-1.0-UNIVERSAL.ja.md](docs/SPEC-1.0-UNIVERSAL.ja.md) の 6 節と 7 節を見てください。
- フォークでは `python3 tests/run_tests.py target/release/fascon` でテスト一式が走り、`python3 tools/compare_c.py` で
  既存のプログラムの生成 C が変わっていないことを確かめられます。
