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
    `image.rs`（PCX/CHR）、`output.rs`（`write_artifacts`／`write_disassembly`）、`inspect.rs`（ROM検査・逆アセンブル表示）、
    `disasm/`（注釈に基づく逆アセンブル、ソース生成、往復検証）、
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

## 開発の流れ

機能追加・バグ修正などは、次の流れで進めます。

1. **Issue**: GitHub Issue を立て、背景（現象・目的）、方針、作業のチェックリストを書きます。
2. **ブランチ**: 最新の `main` から機能ブランチ（`feature/<内容>` など）を切って開発します。
   `main` に直接コミットしないでください。
3. **PR**: 変更をコミット・pushし、`main` へのPRを出します。本文には概要、変更内容、テスト結果、
   互換性の注意を書き、`Closes #<Issue番号>` で Issue と関連付けます。
4. **レビュー**: PRの変更をレビューします（外部のレビューエージェントへの依頼を含みます）。
5. **指摘への対応**: 指摘は一つずつコードで成否を確かめ、反映するものは修正して回帰テストを追加し、
   同じブランチにコミット・pushします。反映しないものは理由を説明します。PRの範囲外の既存の問題は
   別の Issue にします。修正後は、対応すべき指摘がなくなるまでレビューを繰り返します。
6. **マージ**: 対応すべき指摘がなく、CIがすべて成功したらマージします。
   マージ方法は**マージコミットの作成**です（squash・rebase はしません）。
7. **後片付け**: マージ後、ローカルの `main` を最新に更新し、ローカルとリモートの機能ブランチを削除します。

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

### 置き場所

テストは領域別のファイルに分け、共通のヘルパーは各クレートの `tests/common/mod.rs` に置きます
（各テストファイルから `mod common;` で使います）。新しいテストは内容に合うファイルに追加してください。

| クレート | ファイル | 対象 |
|---|---|---|
| nesasm-core | `expressions.rs` | 演算子・優先順位、リテラル、式関数（FUNC・BANK など）、式のエラー |
| | `directives.rs` | ディレクティブ、セクション・バンク、条件アセンブル、行の継続・コメント、入力パス |
| | `instructions.rs` | 命令のエンコード、アドレッシング形式と拡張、全オペコードの逆アセンブル往復 |
| | `macros_procedures.rs` | マクロ、プロシージャの配置と CALL |
| | `images.rs` | PCX（INCCHR）の読み込みと検証 |
| | `api.rs` | 公開API（serde表現、`build`、`write_artifacts`、`write_disassembly`、`inspect`） |
| | `disasm.rs` | 逆アセンブル（コード追跡、注釈の適用と誤り、ソース生成、往復検証、問い合わせ） |
| | `assembly.rs` | 既存の総合テスト（新規は上の各ファイルへ） |
| | `golden.rs` | C#版との互換（`tools/compat/golden.json`） |
| nesasm-cli | `options.rs`／`reports.rs`／`watch.rs` | 引数・環境変数と出力ファイル／診断・JSON・使用量表／watch モード |
| | `cli.rs` | 既存の総合テスト |
| nesasm-mcp | `arguments.rs`／`tools.rs`／`stdio.rs` | サーバ引数／各ツールの詳細／プロトコル・パス制限・リソース |

`src/` 内の `#[cfg(test)]` は、非公開の関数でしか検証できない場合（`output.rs` の一時ファイルなど）に限ります。

### 書き方

- 不具合を修正したら回帰テストを追加してください。
- 期待値は仕様（docs）とコードから導いてください。テストが失敗したときは、実装の不具合か期待値の誤りかを
  確かめ、**実際の出力をそのまま期待値にしないでください**。
- エラーのテストは失敗したことだけでなく、メッセージ（必要なら診断コードと行番号）まで確認します。
  core では `Temp::fails(ソース, "メッセージの一部")`、成功時のバイト列は `Temp::bytes` を使います。
- golden.json で C#版の挙動を確認できない副次的な挙動（最初のエラーに続く連鎖エラーなど）は固定しないでください。
  主となるエラーだけを確認し、気になる挙動は報告します。
- CLI と MCP は実際のバイナリ（`env!("CARGO_BIN_EXE_…")`）を起動し、終了コード・stdout／stderr の書式・
  書き出したファイルを確認します。ホストの環境に左右されないよう、CLI では `NES_INCLUDE` を消して起動します。
- タイミングに依存させないでください。
  - 固定の sleep で待たず、出力行を待ちます。
  - watch のテストでは、監視中のファイルを変更する前に `Watch::ready()` を呼びます。H を送ってヘルプ行を待ち、
    監視ループが状態を記録し終えたことを確かめます。
  - 時間制限のテストは、制限より十分に長くかかる入力を使います（debug ビルドで数十秒など）。
- 全OSで通るようにしてください（改行コードは `cfg!(windows)` で分ける、パスは `\\?\` なしで比較する、など）。
- テストのためだけに依存クレートを増やさないでください（例: CLI の SJIS データはバイト列で書きます）。

### カバレッジ

CIでは計測しません。テストの穴を探すときは [cargo-llvm-cov](https://github.com/taiki-e/cargo-llvm-cov) を使います。

```sh
cargo llvm-cov -p nesasm-core --locked --summary-only
```

- 子プロセスとして起動したバイナリも計測されますが、プロセスが正常に終了した場合に限ります。
  MCP のテストクライアントは、終了時に標準入力を閉じて正常終了を待ちます（強制終了しないでください）。
- Windows では `std::process::exit` で終わる CLI の実行（引数エラー、アセンブル失敗）は記録されません。
- 入力から到達できない防御的なコードや I/O の競合は、無理にテストしなくて構いません。

## MCPサーバの安全上の制約

次の制約は緩めないでください（詳細は [docs/rust.md](docs/rust.md)）。

- 入力・依存ファイル・出力先は `--root` の中に限ります（シンボリックリンク・ジャンクション経由の脱出も拒否）。
- `assemble` が書き込めるのは `.nes`／`.bin` のROMと、その名前から派生する `.lst`／`.s28` だけです。
  `.` で始まるファイル・ディレクトリを含むパスには書き込みません。
- `export_disassembly` が書き込めるのは `.asm`（とバンクファイル）と同じ名前の `.chr` だけで、
  往復検証が一致した場合に限ります。既存ファイルは、生成後に編集されていない生成ファイルだけを上書きします。
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
