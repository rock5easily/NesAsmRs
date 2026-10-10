# AGENTS.md

コーディングエージェント向けの作業ガイドです。利用者向けの説明は [README.md](README.md)、
設計・C#版との差分・検証手順は [docs/rust.md](docs/rust.md)、型の関係は [docs/rust-uml.md](docs/rust-uml.md) を参照してください。

## 概要

NESアセンブラ "nesasm"（2.51p beta3系）のRust版です。C#版 [NesAsmSharp](https://github.com/rock5easily/NesAsmSharp)
からの移植で、**C#版と同じ出力（ROM・map・シンボル・リスト・S-record）を生成すること**が最重要の要件です。
PCE専用機能は対象外です。

## リポジトリ構成

- `crates/nesasm-core`: アセンブラ本体（ライブラリ）
  - `engine/`: 2パスのアセンブラ。`mod.rs`（行処理・シンボル、`assemble`／`assemble_with_cancel`）、
    `directive.rs`、`instruction.rs`、`macros.rs`、`conditional.rs`、`procedure.rs`、`listing.rs`、`rom.rs`
  - `expr.rs`（式）、`opcode.rs`（命令表）、`state.rs`（メモリ配置の定数）、`source.rs`（入力解決）、
    `image.rs`（PCX/CHR）、`output.rs`（`write_artifacts`）、`inspect.rs`（ROM検査・逆アセンブル）、
    `lib.rs`（公開API、`build`、リファレンス文書）
- `crates/nesasm-cli`: `nesasm` コマンド（従来のオプション、watch）
- `crates/nesasm-mcp`: `nesasm-mcp`（rmcpによるstdio接続のMCPサーバ）
- `docs/reference/*.md`: 命令・ディレクティブ・式・オプションのリファレンス（英語）。
  `nesasm-core/src/lib.rs` が `include_str!` で埋め込み、MCPの `get_reference` と resources で配信します。
  **ファイル名の変更・移動はしないでください。** 内容の変更はそのままバイナリに入ります。
- `tools/compat/`: C#版との比較ハーネス（`compare.py`、`check_golden.py`、`Oracle.csproj`／`Oracle.cs`）、
  記録済みの結果 `golden.json`、fixtures

## ビルドと検証

変更後は、CIと同じ次のコマンドがすべて通ることを確認してください。

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

- Rustツールチェーンは `rust-toolchain.toml` で固定しています（edition 2024）。`--locked` を付け、
  依存の追加・更新は必要な場合だけにしてください。
- CIはWindows・Linux・macOS（ARM64／Intel）でclippyとテストを実行します。パス区切り、
  Windowsの `\\?\` 接頭辞、ジャンクション／シンボリックリンク（`#[cfg(unix)]` のテスト）など、
  OSに依存するコードは全OSで動くようにしてください。

## コーディング規約

- `unsafe` は禁止です（workspace lintで `forbid`）。`dbg!`／`todo!` も残さないでください。
- `assemble()` は標準出力への出力、作業ディレクトリの変更、ファイルの書き込みをしません。
  成果物の書き込みは `write_artifacts`／`build` に限ります。
- 入力が原因のエラーでpanic（`unwrap` 等）しないでください。`DiagnosticCode` の診断として返し、
  MCPサーバのプロセスを終了させません。
- 公開型（`Severity`、`DiagnosticCode`、`DataType`、`ArtifactKind`、`BankRef`、`ListLevel` など）と
  そのJSON表現（`"error"`、`"E_IO"`、`"DB"` など）は互換性のため維持してください。
- 上限（ネスト、行数、式のトークン数など）を追加・変更したら
  [docs/reference/directives.md の Limits](docs/reference/directives.md#limits) を更新してください。
- 周辺のコードの書き方・命名・コメントの量に合わせてください。

## C#版との互換性

- 挙動を変更するときは、C#版との出力の一致を崩さないでください。
  `cargo test` の `golden` テスト（`crates/nesasm-core/tests/golden.rs`）が `tools/compat/golden.json` と比較します。
- `golden.json` はC#版を実行して `compare.py --write-golden` で生成するものです。**手で編集しないでください。**
  C#版を実行できない場合は、golden.json の更新が必要なことを報告してください。
- C#版と意図的に異なる動作にする場合（C#版の不具合を再現しない、Rust版の拡張など）は、次をすべて行います。
  - `tools/compat/compare.py` の `KNOWN_DIFFERENCES`（シンボル単位なら `KNOWN_SYMBOL_DIFFERENCES`）に理由を追記
  - `docs/rust.md` の「互換性と意図した差分」または「Rust版の拡張仕様」に追記
  - Rustのテストで新しい動作を検証
- 比較ケースは `tools/compat/fixtures/general/*.asm`（ケース名はファイル名）と、`compare.py` の
  `GENERATED`（インラインのソース）に追加します。`AdditionalDirectiveTest/`・`AdditionalFunctionTest/` は
  C#版のテストデータのコピーなので変更しないでください。
- C#版との比較の実行手順（Windows、.NET SDK、Python）は [docs/rust.md](docs/rust.md#ビルドとテスト) を参照してください。

## テスト

| 対象 | 場所 |
|---|---|
| アセンブラの動作 | `crates/nesasm-core/tests/assembly.rs` |
| C#版との互換（golden.json） | `crates/nesasm-core/tests/golden.rs` |
| CLI | `crates/nesasm-cli/tests/cli.rs` |
| MCP（実際のstdio通信） | `crates/nesasm-mcp/tests/stdio.rs` |

不具合を修正したら回帰テストを追加してください。

## MCPサーバの安全上の制約

次の制約は緩めないでください（詳細は [docs/rust.md](docs/rust.md)）。

- 入力・依存ファイル・出力先は `--root` の中に限ります（シンボリックリンク・ジャンクション経由の脱出も拒否）。
- `assemble` が書き込めるのは `.nes`／`.bin` のROMと、その名前から派生する `.lst`／`.s28` だけです。
  `.` で始まるファイル・ディレクトリを含むパスには書き込みません。
- stdoutはMCP通信専用です。ログなどを出力しないでください。
- 1回のアセンブルには時間制限（既定30秒、`E_TIMEOUT`）があります。

## ドキュメント

- `README.md` と `docs/rust.md`・`docs/rust-uml.md` は日本語、`docs/reference/` は英語で書きます。
- CLIオプション・ディレクティブ・式・MCPツールの挙動を変えたら、`docs/reference/`、`docs/rust.md`、
  必要に応じて `README.md` と `docs/rust-uml.md` を更新してください。

## その他

- 既定の入力文字コードはUTF-8です。`-e SJIS` はWindows-932互換です。
  テストデータの文字コード・改行コードは変換しないでください（`.lst`／`.s28` もバイト単位で比較します）。
- `*.nes`／`*.lst`／`*.s28` と `tools/compat` の `bin/`・`obj/` はコミットしません（`.gitignore` 対象）。
- コミットメッセージは英語で、短い命令形の件名にします。必要に応じて `mcp:`・`docs:`・`ci:` などの接頭辞を付けます。
