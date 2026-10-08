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
- `get_reference`: 命令・ディレクティブ・式・オプションの説明を取得します。

`check`／`assemble`／`inspect_rom` は、ファイルの代わりにソースの文字列を `sources` で受け取れます
（`{"input":"main.asm","sources":{"main.asm":"...","lib.asm":"..."}}`）。ディスク上のファイルより優先し、
書き込みはしません。`check`／`assemble` に `lines` を指定すると、各行のファイル・行番号・バンク・
CPUアドレス・出力バイトを、ファイル・行範囲・バンク・アドレス範囲で絞り込み、ページ単位で返します。

同じリファレンス文書をMCPのresourcesとしても公開します（`nesasm://reference/index`、
`instructions`、`directives`、`expressions`、`options`、いずれも `text/markdown`）。

`check` / `assemble` の引数例: `{"input":"main.asm","options":{"encoding":"sjis","auto_zp":true}}`。
`assemble` には `output`、両ツールには `include_paths` も指定できます。
MCPの入力・依存ファイル・出力先は起動時の `--root` 内に制限されます。
出力先は `.nes`／`.bin` のROMとその付随ファイルに限られ、`.git` などの隠しディレクトリには書き込みません。
1回のアセンブルは既定30秒で打ち切られます（`--timeout <秒>` で変更）。

## ドキュメント

- [Rust版の詳細](docs/rust.md): 構成、C#版との互換性と意図した差分、比較検証の手順
- [UML図](docs/rust-uml.md)
- [リファレンス](docs/reference/index.md): 命令・ディレクティブ・式・CLIオプション・上限

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
