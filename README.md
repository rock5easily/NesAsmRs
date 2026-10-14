# NesAsmRs

NESアセンブラ "nesasm" のRust版です。アセンブラ本体（ライブラリ）、CLI、stdio接続のMCPサーバで構成します。
C#版 [NesAsmSharp](https://github.com/rock5easily/NesAsmSharp) を移植元とし、その出力との互換性を検証しています。
Windows・Linux・macOS（Apple Silicon／Intel）をGitHub Actionsでビルド・テストします。

```sh
cargo build --workspace --release --locked
cargo test --workspace --locked
cargo run -p nesasm-cli -- example.asm
cargo run -p nesasm-cli -- -e SJIS example.asm
cargo run -p nesasm-cli -- --check --json example.asm
cargo run -p nesasm-mcp -- --root /absolute/path/to/project
```

実行ファイルは `target/release/nesasm` と `target/release/nesasm-mcp`
（Windowsでは `.exe`）です。Rustのバージョンは `rust-toolchain.toml` で固定しています。
**既定の文字コードはUTF-8**です。従来のShift-JIS入力には `-e SJIS` を指定してください。

前方参照で `.IF` の条件がパス間で変わるソースには `--compat-if` を指定すると、
NesAsmSharpと同様に警告で処理を続行します。既定ではエラーです。
MCP／公開APIでは `options.compat_if: true` を指定します。

## MCPサーバ

MCPクライアントの設定例（実行ファイルとプロジェクトのパスは環境に合わせて変更）:

```json
{
  "mcpServers": {
    "nesasm": {
      "command": "C:/path/to/nesasm-mcp.exe",
      "args": ["--root", "C:/path/to/project"]
    }
  }
}
```

- `check`: 入力を検証し、診断・シンボル・バンク／領域使用量を返します。ファイルは書き込みません。
- `assemble`: 同じアセンブラでROMと指定された付随ファイルを生成します。
- `inspect_rom`: ROMのiNESヘッダ・割り込みベクタ・起動しない原因の警告を返し、指定範囲の
  16進ダンプと逆アセンブル（NESASM構文、シンボル名付き）も返します。対象はルート内のROMファイル、
  またはメモリ上でアセンブルしたプロジェクトです。
- `disassemble`: ROM（`.nes`、または生の `.bin`）をNESASMソースに逆アセンブルします。
  ルート内の注釈ファイル（JSON）にラベル・シンボル・コード／データ範囲・コメントを記録し、
  割り込みベクタとエントリポイントからコードを追跡します。経路ごとの定数とバンク切替（MMC3）を追い、
  切り替えたバンクへの呼び出し、間接ジャンプのポインタに書かれた入口、戻りアドレスを使うジャンプ表も
  たどります。到達しないバイトは命令にせず、ジャンプ表などの注釈の書き方を課題で示します。要約、注釈で解決すべき課題、
  再アセンブルして元のROMと一致するかの検証結果、指定範囲のソース行・相互参照・ルーチン・
  RAM使用を返します。ファイルは書き込みません。ROM Hackでは注釈の `base` にベースROMを指定すると、
  差分の範囲（とその判断材料）を返し、メインファイルでベースROMを1回 `.incbin` で読み込み、
  差分だけを逆アセンブルして上書きするソースを生成します。
- `export_disassembly`: 元のROMと一致することを確かめたうえで、生成したソース（`.asm`、
  大きなROMではバンクごとの `<stem>_bankXX.asm`（XXは大文字2桁の16進数）、CHR ROMの `.chr`）を
  書き込みます。命令とディレクティブはタブ1個で字下げします。CHR全体は（ROM Hackでも）`.chr` に抽出し、
  メインファイルの末尾で1回読み込みます。mapper 4では注釈の `options.mapper4_fixed_prg_16k` で最終2個のPRGバンクを
  $C000-$FFFF の連続した範囲として解析し、`<stem>_bankXX_YY.asm` にまとめて出力できます。
- `annotate`: 逆アセンブルの注釈ファイル（`*.disasm.json`）を、項目の追加・置換（`upsert`）、
  削除（`remove`）、オプションの変更（`set`）の操作で更新します。結果を検証し、往復検証が一致して
  ファイルが読んだ時から変わっていない場合だけ、正規の書式で書き込みます。推定より優先される項目には
  根拠（`evidence`）が必須で、項目の `evidence` に残し、操作とともに `<名前>.disasm.log.jsonl` にも
  記録します。未確認の項目は `status: "hypothesis"` で置け、`hypotheses: true` を指定した解析でだけ適用します。
  `disassemble` の課題は、この操作の形で修正案（`suggestions`）を返します。
- `get_reference`: 命令・ディレクティブ・式・オプション・逆アセンブラの説明を取得します。

`check`／`assemble`／`inspect_rom` は、ファイルの代わりにソースの文字列を `sources` で受け取れます
（`{"input":"main.asm","sources":{"main.asm":"...","lib.asm":"..."}}`）。ディスク上のファイルより優先し、
書き込みはしません。`check`／`assemble` に `lines` を指定すると、各行のファイル・行番号・バンク・
CPUアドレス・出力バイトを、ファイル・行範囲・バンク・アドレス範囲で絞り込み、ページ単位で返します。

同じリファレンス文書をMCPのresourcesとしても公開します（`nesasm://reference/index`、
`instructions`、`directives`、`expressions`、`options`、`disassembler`、いずれも `text/markdown`）。

`check` / `assemble` の引数例: `{"input":"main.asm","options":{"encoding":"sjis","auto_zp":true}}`。
`assemble` には `output`、両ツールには `include_paths` も指定できます。
MCPの入力・依存ファイル・出力先は起動時の `--root` 内に制限されます。
出力先は `.nes`／`.bin` のROMとその付随ファイル、逆アセンブルの `.asm`／`.chr`、
注釈の `.disasm.json` とその記録 `.disasm.log.jsonl` に限られ、
`.git` などの隠しディレクトリには書き込みません。逆アセンブルの出力は、生成後に編集されていない
生成ファイルだけを上書きします。
1回のアセンブルは既定30秒で打ち切られます（`--timeout <秒>` で変更）。

## ドキュメント

- [MCPとLLMによる逆アセンブル手順](docs/disassembly-workflow.md): 調査、根拠付き注釈の追加と再解析、ソース生成・検証
- [Rust版の詳細](docs/rust.md): 構成、C#版との互換性と意図した差分、比較検証の手順
- [UML図](docs/rust-uml.md)
- [リファレンス](docs/reference/index.md): 命令・ディレクティブ・式・CLIオプション・上限、
  [逆アセンブラ](docs/reference/disassembler.md)

## C#版との互換性検証

`tools/compat/golden.json` に記録したC#版の結果と、`cargo test` が全OSで比較します。
CIの互換性ジョブは、NesAsmSharp（develop/2.x）の元のC#ソースをビルドして直接比較し、
記録が最新であることも確認します。手順は [Rust版の詳細](docs/rust.md) を参照してください。

## 由来とライセンス

NesAsmSharp（C#版）からの移植です。NesAsmSharpは、C言語で書かれたNESアセンブラ "nesasm" の
バージョン2.51p beta3（nesasm_x86 2.51 with auto-zeropage,
http://www.2a03.jp/~minachun/nesasm/nesasm_x86.html ）をC#に移植したものです。
開発の経緯（2026年10月までの履歴）はNesAsmSharpの `rustdev` ブランチから引き継いでいます。

[MIT License](LICENSE)
