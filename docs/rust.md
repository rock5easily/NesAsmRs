# Rust版の構成と検証

## 構成

Cargo workspaceは以下の3クレートで構成します。

| クレート | 役割 |
|---|---|
| nesasm-core | 2パスのアセンブラ、入力解決、画像変換、構造化結果 |
| nesasm-cli | 既存オプション、出力ファイル生成、watch |
| nesasm-mcp | rmcpによるstdio接続、3つのツールと入出力スキーマ |

`assemble(&AssembleRequest) -> AssembleResult` は標準出力、プロセスの作業ディレクトリ変更、
成果物の書き込みを行いません。要求ごとに新しい状態を使います。
`write_artifacts` が成功結果からROM、リスト、S-recordを書き込みます。
既存成果物の上書きには一時ファイルからのrenameを使い、入力ファイルの上書きを拒否します。
すべての一時ファイルを書き終えてからrenameを開始し、失敗時や巻き戻し時の
一時ファイルの後片付けは所有権と`Drop`で管理します。複数成果物のrename全体を
単一トランザクションとして扱うものではありません。

内部のセクション、処理パス、include復帰イベントはenumで表します。
Rust APIの重大度・データ種別・成果物種別もそれぞれ`Severity`、`DataType`、
`ArtifactKind`で表し、JSONでは従来の`"error"`、`"DB"`、`"rom"`などの文字列表現を維持します。
MCP応答への変換は結果の所有権を移し、シンボル表などのコピーを避けます。
ソースの読み込みは上限付きで行い、`.INCBIN`は必要な範囲だけをseek/readします。

`binary` はiNESヘッダを含まないROMペイロードで、C#の `ResultBinary` に対応します。
`header` が16バイトのiNESヘッダ（raw／S-record時は空）、`map` が既存のバンク配置情報です。
`-srec` はC#版と同様に `.nes` の代わりに `.s28` を出力します。
シンボルには名前・値・バンク・ページ・定義位置・公開属性・データサイズを含めます。
診断にはコード、重大度、ファイルと行、include／マクロの呼び出し元を含めます。
字句位置を特定していないため、現状の `column` はnullです。

## 互換性と意図した差分

既存NESASM構文、6502命令、C#拡張、マクロ・式・プロシージャ再配置、PCX/CHR、
iNES/raw/S-record、リスト出力、watchを提供します。PCE専用機能は対象外です。

- 既定文字コードは全OSでUTF-8です。SJISはWindows-932互換として明示指定します。
- パス区切りは各OSに従います。CLIの `NES_INCLUDE` はWindowsで`;`、他OSで`:`区切りです。
  入力探索は作業ディレクトリ、その後にincludeパスを使います。
- C#の未使用ROM・mapのゼロ初期値を維持します。MCPのバンク使用量は別途記録した
  書き込み位置から集計するため、未使用mapの値に依存しません。
- `.DEFCHR` はC#版のバイトポインタの挙動を再現します。8個の32ビット引数を
  little-endianバイト列として扱い、その先頭8バイトを各行に使います。
  元C版に由来する「8個の引数を8行として扱う」方式へは変更していません。
- マクロの `\#` もC#版の挙動を維持します。9番目の引数が空の場合は9を返します。
- C#版はPCXの初回ロードと名前付きBANKで未初期化文字列を参照して例外になります。
  Rust版ではPCXを正常に読み込み、BANK名を保存・検証します。
  この2つの例外を発生させる動作は再現しません。PCXはRustの独立したテストで検証します。
- エラーは結果として返し、MCPサーバのプロセスを終了しません。展開深度、式の複雑さ、
  ソースサイズ、ROM範囲の制限を超えた場合も診断を返します。
- watchはH/R/Q + Enterでヘルプ、手動再生成、終了を操作します。依存ファイル変更は
  自動検知します。MCPではwatchを起動せず、各要求時にファイルを読み込みます。

ローカルMCPは `--root` を必須とし、正規化した依存ファイルと出力パスをこの範囲に制限します。
includeパスは各要求の `include_paths` で渡します。サーバは `NES_INCLUDE` を暗黙に使いません。
stdoutにはMCP通信だけを出力します。ROMバイト列は応答に含めず、成果物パスを返します。
`check` は同じ処理を実行して結果だけを返します。

## ビルドとテスト

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo build --workspace --release --locked
```

CIはWindows x64、Linux x64、macOS ARM64、macOS Intelで同じ検証を実施します。
成功したバイナリをOS別のActions artifactとして保存します。
ワークフローはpush、PR、手動実行に対応します。公開先へのアップロードは行いません。

WindowsでC#との比較を実行するには.NET SDKとPythonが必要です。
比較ハーネスは.NET Framework 4.8を対象に、元のAssemblerソースを変更せずリンクして
ビルドします。参照アセンブリはNuGetから取得するため、古い4.5.2開発環境は不要です。

```powershell
dotnet build tools/compat/Oracle.csproj --configuration Release
cargo build --workspace --locked
python tools/compat/compare.py --oracle tools/compat/bin/Release/net48/NesAsmOracle.exe --rust target/debug/nesasm.exe
```

各fixtureを別プロセスで実行し、成功・失敗、ROM全バイト、配置map、ヘッダ、
シンボル値・バンク・データサイズ、領域サイズ、S-record、リストを比較します。
リストは入力パスと改行を正規化します。失敗時の診断文言は完全一致を求めません。
比較結果は `target/compat/cases` に保存し、CI失敗時にはartifactとして回収します。

既存fixtureに加え、全命令・アドレッシング形式、式、マクロ引数、前方参照、
プロシージャ・グループ、バンク境界と復帰、リストのレベル、文字コードを検証します。
Rustの統合テストはMCPの実際のstdio通信、失敗後の復帰、成果物生成、watch、
入力・出力パス制限、PCX変換と不正画像も対象とします。
