# Rust 実装コードレビュー結果

- 対象ブランチ: `rustdev`（2cea647 時点）
- 対象: `crates/nesasm-core`, `crates/nesasm-cli`, `crates/nesasm-mcp`, テスト, CI
- 実施日: 2026-10-08
- 方法: 5 観点（正当性 / C# 互換性 / 堅牢性 / 設計・性能 / CLI・MCP・テスト・セキュリティ）ごとにサブエージェントがレビュー。ほぼすべての指摘は小さな `.asm` 入力で CLI / MCP を実際に動かして再現済み（未検証のものは明記）。
- C# 版との比較は、Roslyn csc でビルドした C# 版バイナリと Rust 版バイナリに同じ入力を与えて行った（`tools/compat/compare.py` は未使用）。
- `docs/rust.md` に「意図した差分」として記載されている項目は指摘から除外している。

## 前提: 既存チェックの状態

| チェック | 結果 |
|---|---|
| `cargo test --workspace` | 全件成功（cli unit 2 / cli 1 / core unit 1 / core assembly 19 / mcp stdio 1）。ただし `symlink_escape_is_rejected` は `#[cfg(unix)]` のため Windows では未実行 |
| `cargo fmt --all -- --check` | 問題なし |
| `cargo clippy --workspace --all-targets -- -D warnings` | 問題なし |
| `cargo clippy --all-targets -- -W clippy::pedantic` | 警告約 120 件（大半はキャスト系の意図的な切り捨て） |

以下の問題は、いずれも既存テストでは検出されない。

---

## 1. 最優先: セキュリティ・panic・黙った誤出力・DoS

> **対応状況**: S1, P1–P3, D1–D3 はすべて修正済み（回帰テスト追加）。
> - S1: MCP（root 指定時）の出力を `.nes`/`.bin` とその派生ファイルに限定し、隠しパスを拒否。
> - P1: `.if` の結果をパス間で比較し、変化したらエラー。`define`/EQU/RS の `unwrap` を除去。
> - P2: リスト値を行の生成時に埋める方式に変更（`insert_listing_value` を削除）。
> - P3: PROC 内ではバンク繰り上げをせず、`checked_sub` で size を計算。名前なし PROCGROUP の
>   自動名がパスごとに変わる不具合も修正。
> - D1: 式関数の呼び出しを 1 回のアセンブルで 100 万回までに制限。
> - D2: `Line.trace` を `Rc<[SourceLocation]>` で共有（32KB 入力で 1.2GB → 約 100MB）。
> - D3: INCCHR を約 6MiB に制限。`assemble_with_cancel` と MCP の `--timeout`（既定 30 秒）を追加。

### S1【高】MCP `assemble` でルート配下の任意ファイルを任意内容で上書き・新規作成できる
- **場所**: `crates/nesasm-core/src/output.rs:26-98`（`write_artifacts`）、`crates/nesasm-mcp/src/main.rs:34, 119`
- **問題**:
  - 出力先として拒否されるのは「今回のアセンブルの依存ファイル」と「ディレクトリ」だけ。
  - 拡張子の制限がなく、既存ファイルも保護されず、`.git` などの隠しディレクトリも除外されない。
  - `create_dir_all` で任意のディレクトリを作れる。
  - `.db` で出力内容を自由に決められるため、実質「ルート内に任意のバイト列を書ける」。
- **再現（検証済み）**:
  - `a.asm` に `.db "#!/bin/sh",10,"echo pwned",10` を書き、`assemble {"input":"a.asm","output":".git/hooks/pre-commit","options":{"raw":true,"list_level":0}}` を呼ぶと、`.git/hooks/pre-commit` が新規作成された。
  - `output:"README.md"` を指定すると、既存の README.md が上書きされた。
- **影響**: プロンプトインジェクションを受けた LLM が、git hook や `.vscode/tasks.json`、`build.rs` などに書き込むことでコード実行に至る。
- **修正案**:
  - 出力の拡張子を `.nes` / `.bin` / `.lst` / `.s28` に限定する。
  - 先頭が `.` のパス要素を拒否する。
  - 既存ファイルの上書きは、明示的な `overwrite: true` がある場合か、自分が生成した成果物の場合に限る。
  - 起動時に出力先ディレクトリ（`--out-dir`）を指定させる。

### P1【高】`.if` 条件がパス間で食い違うと `unwrap` で panic する
- **場所**: `engine.rs:836`（EQU）、`engine.rs:891`（RS）。根本原因は `engine.rs:775` と `engine.rs:318-329`、`define()`（`engine.rs:420`）
- **問題**:
  - Layout パスでは `allow_undefined=true` なので、前方参照のシンボルは 0 として評価される。
  - そのため Emit パスで初めて有効になるブロックができる。
  - そのブロック内で `=` / `.rs` を使うと、`define()` は symbols に存在しないキーに対して `Ok` を返し、直後の `get_mut(..).unwrap()` が panic する。
  - 通常のラベルの場合は panic せず、シンボル表から黙って抜け落ちる。
- **再現（検証済み、debug / release とも panic）**:
  ```
   .if later
  foo = 5        ; ".rs" 版: "bar .rs 1" → 891 行で panic
   .endif
  later = 1
  ```
- **修正案**:
  - IF 条件を `strict_value` で評価する。または、Layout と Emit で条件の結果が異なれば Phase error にする。
  - `define()` で、Emit パスに未登録のキーが来たら `Err` を返す。
  - `unwrap` を `ok_or(...)?` に置き換える。

### P2【高】`-l0` で `.list` の後に `=` / EQU / IF が続くと範囲外スライスで panic する
- **場所**: `engine.rs:1704-1709`（`insert_listing_value`）。原因は `engine.rs:1719` の早期 return
- **問題**:
  - `list_level==0` のとき `list_line` は何も書かない。
  - それでも `insert_listing_value` は、直前の行に 26 文字のプレフィックスがある前提で `replace_range(start+7..start+14)` などを実行する。
  - 直前の行がパスにマルチバイト文字を含む `#[1] path` ヘッダの場合は、char 境界違反で panic する。
  - MCP からも `list_level=0` で到達する（この場合は E_INTERNAL になる）。
- **再現（検証済み）**:
  - 次の入力を `nesasm -l0 --check t.asm` で実行すると `range end index 20 out of range` で panic する。
    ```
     .list
    X = 1
    ```
  - `.if 1` / `.endif` でも同様に panic する。
  - `日本語/a.asm` では char boundary の panic になる。
- **修正案**:
  - `list_line` が行を書いたかどうかを返し、書いたときだけ書き換える。
  - 根本的には、プレフィックスを生成する時点で値を埋める方式に変える（設計 §5 の L 参照）。

### P3【高】PROC 内でちょうど 8KiB 境界に達すると size の減算がオーバーフローする（debug は panic、release は ROM を黙って重ね書き）
- **場所**: `engine.rs:1579`（`p.size = end - p.base`）。原因は `engine.rs:1341-1347`
- **問題**:
  - `emit_buffer` は offset が 8192 に達すると、手続き内でも bank+1、offset=0 に繰り上げる。
  - その結果、ENDP 時点の offset が base を下回ることがある。
  - PROCGROUP 内の場合:
    - debug ビルドは panic する。
    - release ビルドは巨大値にラップし、誤った「Procedure too large」と「Missing ENDP」を出す。
  - トップレベルの PROC の場合は size=0 と記録され、次の手続きが同じアドレスに再配置される。
- **再現（検証済み）**:
  - 8192 バイトの `b8192.bin` を使った次の入力で、`-S` の出力に `01:A000 bar` と `01:A000 foo` が重なって表示される。
    ```
    foo .proc
     .incbin "b8192.bin"
     .endp
    bar .proc
     rts
     .endp
     call foo
     call bar
    ```
  - `.procgroup` 内では、8191 バイトのファイルで debug ビルドが panic する。
- **修正案**:
  - frames が空でないときは bank を繰り上げない。
  - `end.checked_sub(p.base).ok_or(...)?` にする。

### D1【高】式関数（`.func`）のネスト展開が指数時間になる
- **場所**: `expr.rs:290-295`（深さ上限は 32 だが、評価回数には上限がない）
- **問題**: 関数本体から前段の関数を k 回呼ぶと、評価回数は k^depth になる。
- **計測（release）**: 4^8 で 0.5 秒、4^9 で 2.1 秒、4^10 で 7.9 秒。4^12 は 60 秒を超えてタイムアウトした。ソースは約 300 バイト。
- **再現（検証済み）**:
  ```
  f0 .func \1
  f1 .func f0(\1)+f0(\1)+f0(\1)+f0(\1)
  ...
  f12 .func f11(\1)+f11(\1)+f11(\1)+f11(\1)
   .db f12(1)&255
  ```
- **修正案**: Context に関数呼び出し回数の予算カウンタ（`Cell<usize>`）を持たせ、上限（例: 10 万回）を超えたら `Err` にする。

### D2【高】マクロ / 自己 include で Line ごとに trace を複製するため、小さな入力でメモリが枯渇する
- **場所**: `engine.rs:561-564`、`source.rs:107`（上限は深さ 32 と 100 万ステップのみ）
- **問題**:
  - 深さ優先で展開するため、キューに「本体行数 × 32」本の Line が溜まる。
  - 各 Line は最大 32 個の PathBuf を所有する。
- **計測（release、PeakWorkingSet、検証済み）**:
  - マクロ版: N=2000 行で 305MB、N=8000 行（入力 32KB）で 1.2GB。
  - 自己 include 版: N=8000 で 925MB。
  - 1 行あたり約 150KB で線形に増えるため、1MiB の入力では数十 GB に達する計算になる。
- **修正案**:
  - trace を `Rc` で共有する親リンク方式にする（設計 §5 の F 参照）。
  - 展開済み行数の総量に上限を設ける。
  - include したファイルの内容をキャッシュする。

### D3【中】MCP サーバにリソース制限・タイムアウト・キャンセルがない
- **場所**: `crates/nesasm-mcp/src/main.rs:105-148`（`gate` mutex と `spawn_blocking`）、`engine.rs:1166`（INCCHR）
- **問題**:
  - `gate` を保持したまま D1 / D2 のような終わらない処理を実行するため、以降の呼び出しがすべて永久に待たされる。
  - `notifications/cancelled` を受けても処理は止まらない。
  - INCCHR は `fs::read` でファイル全体をサイズ上限なしに読む（INCBIN は対策済み）。
  - panic は JoinError から E_INTERNAL に変換されるので、そこは許容範囲。
- **修正案**:
  - INCCHR は `File::take(上限)` で読む。PCX は最大 1024x768 なので、上限は 128 + 768×2048×4 程度でよい。
  - 1 回のアセンブルで読む合計バイト数に上限を設ける。
  - `tokio::time::timeout` と、エンジン側の協調キャンセル（AtomicBool）を入れる。

---

## 2. 誤ったコードの生成・正当性

> **対応状況**: C1–C5 を修正済み（回帰テスト追加）。同時に C8, C9, C10, C14 も解消。
> - C1: EQU/RS 定数と RAM ラベル（`RESERVED_BANK`）をリロケーション対象から除外。
> - C2/C3: `(` の扱いを C# に合わせた。autozp なしは常に式、autozp ありは `(zp,X)`／`(zp),Y` のみ間接
>   （`(addr),Y` が 255 超なら abs,Y）。autozp なしの `(<..` はエラー。`instructions.md` を修正。
> - C4: 即値では `<`/`>` を単項 low/high として評価。
> - C5: pass2 でアドレスラベルのバンク不一致を `Bank mismatch` エラーに。
> - C8: `[zp].tag` の tag が 255 超ならエラー。C9: tag を大文字化せずに評価。C10: `], y` の空白を許容。
> - C14: 先行エラーで中断したときは `Missing ENDIF/ENDP` を出さない。
> - C6 は P1（パス間で条件が変わればエラー）により無言の誤出力が解消されたため、警告の追加は未実施。
> - 修正前後の全フィクスチャ（`Tests/` 30 本 × 4 オプション）で出力が一致することを確認済み。

### C1【高】PROC 内の EQU / RS 定数がリロケーションされ、Phase error になる
- **場所**: `engine.rs:417-419`（frames が空でなければ無条件で `symbol_proc` に登録する）、`engine.rs:1627-1640`（relocate）
- **問題**:
  - 定数にも `value += org - base` の補正がかかるため、2 番目以降の proc で値がずれる。
  - C# 版（`SymbolProcessor.cs:320-325`）は、アドレスラベルかつ CODE セクションの場合だけ Proc を設定している。
- **再現（検証済み）**: 次の入力で `Phase error for symbol 'CONST'` になり、さらに誤った `Missing ENDP` も出る。
  ```
   .bank 0
   .org $8000
   call bar
   rts
   .proc foo
   nop
   nop
   rts
   .endp
   .proc bar
  CONST = 5
   lda #CONST
   rts
   .endp
  ```
- **修正案**: `define` に `addr: bool` 引数を追加し、`symbol_proc` への登録はアドレスラベル（`section == Code`）に限定する。

### C2【高】括弧付きオペランドの判定が C# と異なる
- **場所**: `engine.rs:1395-1422`（`parens = starts_with('(') && (self.auto_zp || name == "JMP")`）
- **問題 1（autozp なし）**:
  - `jmp ($1234)` が間接 JMP になる。
  - 出力は C# が `4C 34 12`、Rust が `6C 34 12` で、どちらもエラーは出ない。
  - C# 版（`CodeProcessor.cs:699-724`）では、autozp でなければ `(` は式の括弧として扱われる。
- **問題 2（`-autozp` あり）**:
  - `foo=$10` で `lda (foo)` / `sta (foo)` を組むと、Rust は `B2 10 92 10` を出力する。これは 65C02 の (zp) 命令で、NES では JAM（CPU 停止）になる。
  - C# 版（`CheckPreindexed`、`CodeProcessor.cs:1129-1202`）では `A5 10 85 10` になる。
- **関連**:
  - `lda (foo+1)*2,x` は、Rust だけ `Invalid indirect operand` エラーになる。
  - `foo=$1000` での `lda (foo),y` は、C# は ABS_Y にフォールバックするが、Rust は `Operand size error` になる。
- **修正案**:
  - 間接として扱うのは、括弧の外側に `,X)` / `),Y` がある場合だけにする（C# と同じ）。
  - それ以外は式全体を abs / zp / indexed として扱う。
  - JMP の特別扱いを残すなら、オプションにして文書化する。

### C3【高〜中】autozp なしの `lda (<$12),y` が abs,Y として黙って出力される
- **場所**: `engine.rs:1395`、`docs/reference/instructions.md:9-11`
- **問題**:
  - ドキュメントは autozp なしでも `(<$12),y` を間接インデックスとして案内している。
  - 実際の出力は `B9 12 00`（LDA $0012,Y）になる。
  - C# 版はこの入力を式エラーとして拒否する。
- **修正案**: エラーにするか、間接として扱う。あわせてドキュメントを修正する。

### C4【中】即値の `#>` / `#<` が単項 high / low として効かない
- **場所**: `engine.rs:1443-1446` と `expr.rs:166-175` の不整合
- **問題**: `instruction()` が、Imm モードでも先頭の `<` / `>` を ZP / ABS 強制の記号として取り除いてしまう。
- **再現（検証済み）**:
  - `zpv=$12` で `lda #>zpv` を組むと `A9 12` になる（`db >zpv` は `00` なので、`00` が正しい）。
  - `lda #<$1234` は `Operand size error` になる。
- **修正案**: Imm モードでは `<` / `>` を取り除かずに式へ渡す。

### C5【中】pass2 でラベルのバンク不一致を検査しない（`BANK()` が黙って誤る）
- **場所**: `engine.rs:420-425`
- **問題**: C# 版（`SymbolProcessor.cs:312-316`）は pass2 で "bank mismatch" を検出するが、Rust は値しか比較しない。
- **再現（検証済み）**: 次の入力は成功扱いになり `A9 00` を出力する（`A9 01` が正しい）。
  ```
   .bank 0
   .org $c000
   lda #BANK(foo)
   .bank BNK
   .org $8000
  foo: nop
  BNK = 1
  ```
- **修正案**: pass2 でバンクも比較する。または `.bank` のオペランドを `strict_value` で評価する。

### C6【中】`.if` 条件中の未定義シンボルを警告なしで 0 として扱う
- **場所**: `engine.rs:765-786, 775`
- **問題**:
  - C# 版（`AssembleProcessor.cs:421-424`）は「Undefined label in IF condition.」と警告する。
  - Rust では pass1 と pass2 で分岐が変わっても、エラーも警告も出ずに出力が変わる（P1 の根本原因でもある）。
- **再現（検証済み）**: `.if FOO` / `nop` / `.endif` / `rts` / `FOO = 1` を組むと、`EA 60` が無言で出力される。
- **修正案**: layout パスで未定義のシンボルを検出したら警告を出し、`-wd` で抑止できるようにする。

### C7【中】DB 文字列の非 ASCII 文字が `as u8` で切り捨てられる

> **対応済み**: ソースの文字コードのバイト列で出力する（UTF-8 / SJIS）。C# 版も下位 1 バイトに
> 切り捨てるため、比較ハーネスでは既知の差分 `db_non_ascii` として扱う。
- **場所**: `engine.rs:1021`（`bytes.push(c as u8)`）
- **再現（検証済み）**:
  - `.db "あ"` は、UTF-8 でも `-e SJIS` でも `0x42` を出力する。
  - C# 版は SJIS 指定時に `82 A0` を出力する。
- **修正案**: ソースのエンコーディングで再エンコードしたバイトを出力するか、エラーにする。

### C8【中】インダイレクト自動タグ `[zp].tag` の値が範囲チェックなしで切り捨てられる
- **場所**: `engine.rs:1408`（`self.value(tag)? as u8`）
- **再現（検証済み）**: `lda [$10].300` がエラーにならず、`a0 2c b1 10` を出力する。
- **修正案**: 255 を超えたらエラーにする。

### C9【低】`[ptr].tag` の tag 式が大文字化されてから評価される
- **場所**: `engine.rs:1400, 1405-1408`
- **再現（検証済み）**:
  - `tg=3` で `lda [$10].tg` を組むと `Undefined symbol 'TG'` になる。
  - `TG` が別に定義されていれば、その値が黙って使われる。
- **修正案**: 大文字化する前の文字列から tag を取り出す。

### C10【低】`lda [$10], y` のように `],` の後に空白があるとエラーになる
- **場所**: `engine.rs:1400-1416`
- **問題**: C# 版は空白を読み飛ばすが、Rust は `Invalid indirect operand` になる。`,Y++` も同様。
- **修正案**: 判定の前に tail から空白を除去する。

### C11【低】`.if` / `.else` / `.endif` 行のラベルが定義されない
- **場所**: `engine.rs:451-478`
- **再現（検証済み）**: `foo: .if 1` / `nop` / `.endif` / `dw foo` を組むと `Undefined symbol 'foo'` になる（C# の `DoIf` はラベルを定義する）。

### C12【低】PC 記号 `*` の直後の `%` を 2 進リテラルと誤認する
- **場所**: `expr.rs:56-59`
- **再現（検証済み）**: `db * % 256` が `Invalid numeric literal` になる（C# は剰余として扱う）。

### C13【低】REGIONSIZE の負値が `as u32` でラップする（未検証。コードからの導出）
- **場所**: `expr.rs:247`
- **問題**: BEGINREGION より前に ENDREGION があると 0xFFFFFFxx になり、`.db` の負数許容範囲を通過して黙って出力されることがある。
- **修正案**: 負値なら「Region end precedes begin」エラーにする。

### C14【低】最初のエラーで中断した後に、誤った追加エラーが出る
- **場所**: `engine.rs:179-184`
- **問題**: `run` が break した後も、`conditions` / `frames` の残りを検査するため、C1 の例では本来のエラーに続いて `Missing ENDP/ENDPROCGROUP` が出る。
- **修正案**: 先にエラーがあれば残りの検査を省く。

---

## 3. C# 版との互換性

> **対応状況**: X1–X14 すべて対応済み（回帰テスト追加）。
> - X1 `.page` を実装。X2 `0x` と `%1100_0011` を受理。X7 文字列内の `\"` を受理。X8 `lda.l`/`lda.h` を受理。
> - X3 1 桁目の語は常にラベル（マクロ名と同名のラベルはエラー）。未使用になった `is_operation` を削除。
> - X4 行単位のエラーは続行し、致命的エラーのみパス中断。パス2のエラー行はパス1のサイズ分だけ進めて
>   位相エラーの連鎖を防止。CLI の終了コードはエラー件数。成果物を残さない点は意図的差分として文書化。
> - X5 リストのタブを 8 桁展開、`.lst`/`.s28` は OS 既定の改行。X6 INCBIN/INCCHR/DS はアドレスのみ。
> - X9 `HIGH`/`LOW`/`BANK`/`PAGE` 等を前置演算子として受理、定数の `PAGE` は -1（`Symbol.page` は `Option`）。
> - X10 RS 行は値列、FUNC 行はアドレスなし、RAM セクションのバンクは `--`。X11 `.opt l+`/`m` を C# に合わせた。
> - X12/X13 は Rust 版の仕様として `docs/rust.md` に記載済み。
> - X14 リージョン情報と `-s`/`-S` を C# の書式で表示（`BankUsage` にバンク名・セグメント、結果に `ram` を追加）。
>   `pass N` 表示、診断の出力先、`-?` の内容は意図的差分として文書化。
> - 修正前後の全フィクスチャでバイナリは一致。差分は X4 による追加エラー報告（7 本）のみ。

| # | 重大度 | 差異 | Rust 側 | C# 側 | 修正案 |
|---|---|---|---|---|---|
| X1 | 高 | `.page` ディレクティブが未実装。`.page 7` が `Unknown instruction or addressing mode 'PAGE'` になる | `engine.rs:1778-1830`, `899-1249` | `CommandProcessor.cs:372-400` | proc 内は FatalError、7 を超えたらエラー、`position.page` を設定、リストに `value<<13` を出す |
| X2 | 高 | 数値リテラル `0x1F` / `0X..` と、2 進の `_` 区切り（`%1100_0011`）が通らない | `expr.rs:54-77` | `ExprProcessor.cs:486, 520` | 10 進トークンの先頭 `0x` を 16 進として読み、基数 2 では `_` を読み飛ばす |
| X3 | 中 | 1 桁目の語の扱い。C# は常にラベルとみなす。1 桁目 `nop` + `\trts` の出力は C# が `60`、Rust が `EA 60`。1 桁目の `.bank` やマクロ呼び出しも C# はエラー、Rust は通る | `engine.rs:751-757` | `AssembleProcessor.cs:150-176` | 1 桁目の語はラベルとして解析する。拡張として残すなら警告を出す |
| X4 | 中 | エラー時の挙動。Rust は最初のエラーで break し、exit=1、`.lst` は残らない。C# は全エラーを出し、exit=エラー数、途中までの `.lst` を残し、メッセージは stdout に出す | `engine.rs:465,515,540,546,671,696`、`cli/main.rs:140-149, 256-258` | `NesAssembler.cs:96-115, 283-287` | 行単位のエラーは続行し、パスの終わりで中断する。終了コードを合わせるか、差分として文書化する |
| X5 | 中 | リストでタブを展開しない（桁揃えがずれる）。改行は LF 固定（C# は Windows で CRLF）。S-record も LF | `engine.rs:1762-1764`, `output.rs:186` | `InputProcessor.cs:206-211` | 8 桁境界でタブを展開し、改行は OS 既定にする。そうしないなら文書化する |
| X6 | 中 | `.incbin` / `.ds` のリストに全データバイトが出る。`.incbin "d.bin",0,4000` で C# は 9 行、Rust は 1343 行 | `engine.rs:1722-1735` | `CommandProcessor.cs:618-621, 720, 1095-1106` | INCBIN / INCCHR / DS ではアドレスだけを出す |
| X7 | 中 | 文字列中の `\"` を扱えない。`.db "a\"b"` が unterminated エラーになり、`\"` の後の `;` で行が途中から切り捨てられる | `source.rs:124-170, 196-202` | `CommandProcessor.cs:171-199` | クォート内では `\` の次の 1 文字を読み飛ばす（3 関数とも） |
| X8 | 低〜中 | `lda.l` / `lda.h` の命令拡張が拒否される | `engine.rs:1352-1355` | `AssembleProcessor.cs:296-321` | 拡張 `L` / `H` を ext として扱う |
| X9 | 低 | 括弧なしの `HIGH foo` がエラーになる。定数に対する `PAGE()` は C# が 255、Rust が 0 | `expr.rs:148-152`, `engine.rs:838` | `ExprProcessor.cs:636-640, 672, 749-752` | キーワードを前置単項演算子として扱い、定数の page を -1 として保持する |
| X10 | 低 | リストのアドレス列 / 値列。`.rs` の行（C# は rs の値、Rust は PC）、`.func` の行にアドレスが付く、ZP / BSS のバンクが `--` ではなく `00` になる | `engine.rs:1741-1756` | `CommandProcessor.cs:1006-1030` | C# に合わせる |
| X11 | 低 | `.opt` の扱い。`-m` 指定時に `.opt m-` が効かない。`.opt l+` だけで `.lst` が作られる | `engine.rs:1219-1223` | `CommandProcessor.cs:105-112, 1297-1305` | `m` は flag をそのまま反映し、`l+` では `any_list` を立てない |
| X12 | 低 | マクロ名の大文字小文字（C# は区別し、Rust は区別しない）。ネスト上限も C# の 7 に対して Rust は 32 | `engine.rs:507,517,520` | `MacroProcessor.cs:83-117` | キーに元の名前を使い、上限の違いは文書化する |
| X13 | 低 | 文書化されていない拡張。`.ds n, fill`、`x` / `y` / `a` をシンボル名に使う、`.db "a;b"`、単項 `<` / `>`、`==`、行末 `\` による行継続 | `source.rs:93-117` ほか | `ExprProcessor.cs:619-628` ほか | `docs/rust.md` に追記するか、互換モードでは拒否する |
| X14 | 低 | CLI の標準出力。pass 表示、`-s` / `-S` の書式、Region の表示、`-?` の内容が C# と異なる | `cli/main.rs:150-180, 201` | `NesAssembler.cs:280-300, 682` | 互換が必要なら C# の書式に合わせる。不要なら差分として明記する |

### 対応済み: C# のバグを互換のために再現していた箇所
- **DEFCHR**（`image.rs`）: C# 版は `defchr $11111111` を 8 行与えると `03`×8 を出力する。本来は `FF`×8。
- **`\#`**（`engine.rs`）: C# 版は引数が 2 個でも 0 個でも 9 を返す。
- **決定**: C# のバグは再現せず、Rust 版では本来の動作とする。
  - DEFCHR は 1 引数を 1 行として扱う。
  - `\#` は空でない最後の引数の番号を返す。
  - 仕様は `docs/rust.md` に記載した。互換比較のフィクスチャ `macro_types` からは `\#` を外し、`defchr_uses_one_row_per_argument` と `macro_argument_count` テストで検証する。
- X13 の拡張は、`docs/rust.md` の「Rust版の拡張仕様」に Rust 版の仕様として記載した（X12 のマクロ名の大文字小文字とネスト上限も含む）。

### 一致を確認した項目
- オペコード表（65C02 由来の形式も含む）、分岐の範囲判定、DB / DW のオーバーフローの閾値
- 式の演算子と優先順位（単項 `!` の差のみで、実害なし）、除算ゼロとシフト量の扱い
- `LOW` / `HIGH` / `BANK` / `SIZEOF` / `DEFINED`、マクロ引数 `\1` / `\?n` / `\@`、`.func`、`.if` 系
- `.rsset` / `.rs`、`.zp` / `.bss` / `.ds`、`.incbin` の offset / size、INCBIN のページ計算、CALL のスタブ
- iNES ヘッダ、`-raw`、`-srec`（改行コード以外）、include のヘッダ `#[n]`
- 式パーサの堅牢性（256 トークン上限、127 段の括弧、`i32::MIN/-1`、ゼロ除算）
- PCX デコーダの境界チェック、1MiB のソース上限、出力のステージング書き込み
- CLI のパスチェック（`..`、絶対パス、symlink）

---

## 4. CLI・MCP・診断

> **対応状況**: M1–M8 を修正済み（回帰テスト追加）。M9 は C# 互換のため現状維持。
> - M1: stdin と rmcp の間に中継層を置き、解析できない行に `-32700 Parse error` を返す（出力は単一タスクで直列化）。
> - M2: serverInfo を `nesasm-mcp` / パッケージのバージョンに。
> - M3: `list_level` に最大値 3、`topic` を enum 化、入力フィールドに説明を追加。
> - M4: 診断パスから `\\?\` を除去（MAX_PATH 未満のみ）。`Missing ENDIF/ENDP` は開始行、
>   プロシージャ配置エラーはあふれたプロシージャの位置で報告。
> - M5: include 探索はルート外の候補があっても続行。存在しないディレクトリ後の `..` は全 OS でエラー。
> - M6: ヘルプ判定を引数ループ内に移動、引数エラーは案内付きで終了コード 2、ヘルプはプレーンテキスト。
> - M7: `NES_INCLUDE` は全 OS で `;` 区切り（Unix は `:` も可）、10 個超は警告。
> - M8: watch は標準入力の終端・読み取りエラーで終了。
> - M9: C# 版も Region Info を stdout に出すため、X14 の C# 書式に合わせて stdout のまま。

| # | 重大度 | 内容 | 場所 | 修正案 |
|---|---|---|---|---|
| M1 | 中 | 不正な JSON に応答しない（-32700 もログも出ない） | `nesasm-mcp/src/main.rs:256` | -32700 を返すか stderr にログを出す。挙動をテストで固定する |
| M2 | 低 | serverInfo が `{"name":"rmcp","version":"3.5.1"}` になっている | `nesasm-mcp/src/main.rs:226-231` | `Implementation::from_build_env()` を使う |
| M3 | 低 | 入力スキーマの制約が足りない。`list_level` は 4〜255 を受け付けてからエラーになる。topic に enum がなく、description もない | `nesasm-mcp/src/main.rs:15-40` | `#[schemars(range(max = 3))]` を付け、topic を enum にし、doc コメントを追加する |
| M4 | 中 | 診断の位置情報。Windows でパスに `\\?\C:\...` 接頭辞が付く（エディタの problem matcher が反応しない）。`Missing ENDIF` / `Missing ENDP` が `:0` 行になる。relocate のエラーはファイル名が空になる | `source.rs:18-41`, `engine.rs:180, 183, 1619`, `cli/main.rs:140-149` | 表示には `dunce::simplified` 等を使う。IF / PROC の開始位置を保持する |
| M5 | 低 | `find_file` が最初の候補で root 外のエラーになると探索を打ち切る。存在しない中間ディレクトリを含む `..` の判定がプラットフォームによって異なる（Unix 側は未検証） | `source.rs:27-29, 54` | 候補ごとのエラーを記録して探索を続ける。事前に字句的に正規化する |
| M6 | 低 | CLI のヘルプ。`-?` / `--help` は引数のどこにあってもヘルプ扱いになる。エラー時に使い方の案内がない。ヘルプに Markdown がそのまま出る | `cli/main.rs:18, 204-206` | 判定をループ内に移す。使い方エラーは exit 2 にする |
| M7 | 低 | `NES_INCLUDE` の区切り文字が OS によって異なる（Unix は `:`）。先頭 10 件への切り詰めを黙って行う | `cli/main.rs:84-86` | `;` も受け付ける。切り詰めたら警告を出す |
| M8 | 低 | `-watch` が stdin の EOF で終了しない。読み取りエラーが続くと busy loop になる可能性がある | `cli/main.rs:210-258` | `Disconnected` で終了する |
| M9 | 低 | 「Region X: BEGINREGION not found」が stdout に出る | `cli/main.rs:150-167` | stderr に出すか、警告の診断にする |

---

## 5. 設計・保守性・性能

> **対応状況**: A–O すべて対応済み。修正前後で 684 ケース（全フィクスチャ・互換ケース・検証用入力 ×
> 6 オプション）の ROM・map・ヘッダ・シンボル・バンク・リスト・診断が完全に一致することを確認済み。
> - A: `engine.rs`（2100 行）を `engine/` の 8 モジュールに分割し、状態を `RomImage`・`Listing`・
>   `Procedures`・`Macros`・`Conditions`・`SourceCache` に分けた。`execute` は 474 → 225 行。
> - B: `Directive`（別名を統合）と `Mnemonic` の enum。各行を 1 回だけ分類。
> - C: `Position` を `Copy` に。ラベルのスコープは `Rc<str>` で別に保持（保存・復元は `Cursor`）。
> - D: マクロ本体を `Rc<[Line]>` で共有、引数種別を `ArgumentKind` enum で 1 回だけ計算、1 パス置換
>   （引数中の `\1` などを再置換しない）。
> - E: ソースと PCX 変換結果をパス間で共有。Layout パスの INCBIN はサイズのみ。
> - F: `Line` はファイルを `Rc<Path>`、経路を `Rc<[SourceLocation]>` で共有。
> - G: 公開 `DiagnosticCode` enum（JSON は従来の文字列）と内部 `AsmError { message, fatal }`。
>   致命的エラーの判定をメッセージ文字列の照合から型に変更。
> - H: `Symbol.bank` を `BankRef { Rom(u8), Constant, Procedure }` に（JSON は従来の数値）。
> - I: メモリ配置の定数を `state.rs` に集約。
> - J: 式のトークンを `Op` enum とスライス借用のゼロコピーに。
> - K: プロシージャを名前の索引と ID で参照（線形探索と clone を廃止）。
> - L: リスト行を列定数と固定幅の `Prefix` で 1 回だけ組み立て。
> - M: ROM バッファは書き込んだバンクまで確保、使用状況は bitset、出力はバンク境界までのスライス単位。
> - N: `ListLevel`・`ReferenceTopic` enum、`Diagnostic::error`、`AssembleResult::error_count`、
>   `build()` を追加し、CLI と MCP の重複処理を削除。
> - O: rmcp の client 系 feature と tokio の process を削除（`Cargo.lock` から 152 行減）、
>   schemars を `schema` feature に、`rust-version = 1.88` と workspace lints を追加。
> - 性能（release）: 大規模ソース（2.9 万行）0.56 → 0.17 秒・22 → 16 MB、マクロ再帰 1.59 → 0.08 秒・
>   102 → 45 MB、式関数のネスト 2.86 → 0.97 秒。

| # | 優先度 | 内容 | 場所 | 改善案 |
|---|---|---|---|---|
| A | 高 | `execute` が 474 行の文字列 `match` で、Engine は 30 以上のフィールドを持つ God object。`state.rs` は名前に反して状態を管理していない | `engine.rs:53-89, 813-1294` | `RomImage` / `ListingState` / `ProcState` / `MacroState` / `CondStack` のサブ構造体に分け、`directive/`・`macro_expand.rs`・`listing.rs`・`procedure.rs`・`instruction.rs` などのモジュールに分割する |
| B | 高 | ディレクティブを毎行の大文字化 `String` と線形探索で判定している。`is_operation` は 47 要素を探索し、`opcode()` を 4 回呼ぶ。`unreachable!()` も残っている | `engine.rs:451, 482, 618, 684, 765, 899, 1253, 1267, 1778-1833` | 1 回だけパースして `Directive` / `Mnemonic` の enum にする。opcode を const テーブルにして O(1) で引く |
| C | 高 | `Position` に `global: String` が含まれ、行ごと・emit ごとに clone される | `engine.rs:22-27, 677, 834, 877, 1310` | `Position` を `Copy` にし、スコープは別に保持する（`Rc<str>` / SymbolId） |
| D | 高 | マクロ展開で、呼び出しのたびにボディ全体を clone し、1 行あたり約 40 回アロケーションする。引数の種類は 0..=6 のマジックナンバー。置換結果に `\1` が含まれると二重置換される危険がある | `engine.rs:520, 560-611` | `Rc<[Line]>` にする。`enum MacroArgKind` にする。1 パスのスキャナで置換する |
| E | 高 | 2 パスごとに全行を clone し、INCLUDE / INCBIN / INCCHR をディスクから読み直す。パスの間にファイルが変わるとフェーズエラーになり得る | `engine.rs:175, 618-675, 1095-1168` | ソースとバイナリのキャッシュをパス間で共有する。Layout パスの INCBIN は `metadata().len()` だけで済ませる |
| F | 高 | `Line` が行ごとに `PathBuf` と `Vec<SourceLocation>` を所有する（D2 の原因） | `source.rs:9-14, 67, 106-115` | `FileId(u32)` と `Option<Rc<TraceNode>>` にする |
| G | 高 | エラーが `Result<_, String>` で、エラーコードも文字列リテラル | `lib.rs:95-101`, engine 全体 | `thiserror` による `AsmError` と、`DiagnosticCode` enum（serde で rename）にする |
| H | 中 | 番兵値のバンク（`0xF0` / `0xF1`）が `usize` に混在している | `state.rs:2-3`, `lib.rs:107`, `engine.rs:405-409, 597, 837, 891`, `expr.rs:270` | `enum BankRef { Rom(BankIndex), Constant, Procedure }` にする |
| I | 中 | 定数 `BANK_SIZE` があるのに `8192` を直書きしている（13 か所以上）。ほかに 128、`0x200`、32 などのマジックナンバーがある | engine 各所 | 定数を集約し、`Position::cpu_addr()` などのメソッドにまとめる |
| J | 中 | 式のトークンが `Token::Op(String)` で、比較のたびに String を生成する。ローカルラベルは毎回 `format!` する | `expr.rs:14-21, 149, 166, 231, 285` | `enum Op` とゼロコピーのトークンにする。ローカルシンボルは 2 段キーで引く |
| K | 中 | プロシージャを `Vec` の線形探索と clone で扱っている（O(P² + S·P)） | `engine.rs:1530, 1548-1652` | `HashMap<String, ProcId>` を併置し、ID で参照する |
| L | 中 | リストを固定オフセットの文字列書き換えで生成している（P2 の原因） | `engine.rs:1703-1766` | `ListingRow` 構造体を作り、確定してから 1 回だけ `write!` する |
| M | 中 | ROM 全体分（1MiB）の 3 バッファを常に確保し、emit を 1 バイトずつ処理している。バンク跨ぎの処理が重複している | `engine.rs:84, 123, 171-174, 1306-1350` | bitset と遅延確保を使う。スライス単位でコピーする |
| N | 中 | 公開 API の型が弱い。`list_level: u8`、bool が 5 個並ぶ、冗長な `success`、topic が文字列、`#[non_exhaustive]` がない | `lib.rs` | `enum ListLevel` / `enum ReferenceTopic`、`success()` メソッド、`Diagnostic::error()` を用意する。build 処理を一本化する |
| O | 低 | Cargo の設定。rmcp の client 系 features と tokio `process` が本番の依存に入っている。core が schemars を必須にしている。`[workspace.lints]` と `rust-version` がない | `Cargo.toml` 各所 | client 系は dev-dependencies に移す。schemars は optional feature にする。workspace lints を設定する |

---

## 6. テスト・CI

> **対応状況**: T1–T5 すべて対応済み。C# オラクルをローカルでビルドして比較スクリプトを実行し、
> 92/92 ケースが一致（既知の差分 6 件と 1 シンボル）。
> - T1: 終了コード（エラー件数）を期待するよう修正。Rust が書き出す `.nes`/`.s28`/`.lst` をバイト単位で比較、
>   失敗ケースはエラー行番号を比較、シンボルを両方向で比較（オラクルは定義済みシンボルのみ出力）。
>   §3 の互換項目を比較ケースに追加。意図的な差分は `KNOWN_DIFFERENCES` に理由付きで列挙。
>   `--write-golden` で C# の結果を `tools/compat/golden.json` に記録し、`cargo test` の `golden` テストで
>   全 OS で比較。CI では golden.json が最新かも確認。
> - 比較で見つかった差異を修正: `.if`/`.ifdef` 行のラベル定義（C11）、REGIONSIZE の引数エラーを
>   パス2で報告、不正なマクロ名では本体を読まない、未終了マクロはファイル末尾で致命的エラー。
> - レビューの記述の訂正: `lda.l`/`lda.h` は C# にはなく（X8）、即値の `#<`/`#>` も C# では構文エラー。
>   どちらも Rust の拡張として文書化した。C# は 2 番目以降の文字列引数の `\"` で失敗し、
>   `CATBANK` で到達したバンクを `_nb_bank` に数えない（いずれも Rust は正しい動作）。
> - T2: 条件のネスト、RS、CALL のトランポリン、FAIL、ネスト・サイズ上限、行継続、S-record の
>   チェックサム、`list_level` の検証、ディレクティブの別名・iNES ヘッダ、Windows のジャンクションのテストを追加。
> - T3: MCP の未知のツール・メソッド・引数、文字列 id、初期化前の呼び出し、古いプロトコル版、
>   ルート外のパス・出力先、並行呼び出し、出力スキーマとの整合のテストを追加。
> - T4: テストの一時ディレクトリを `tempfile` に、子プロセスは Drop で kill、MCP の読み取りスレッドの
>   エラーをテストへ伝える。
> - T5: push を対象ブランチに限定、fmt は 1 ジョブ、Actions を SHA で固定、ツールチェーンは
>   `rust-toolchain.toml` に一本化。ARM の Linux/Windows ランナーは追加していない。

### T1【中】C# 版とのゴールデン比較が不十分
- **場所**: `tools/compat/compare.py:30-75`, `.github/workflows/rust.yml:33-48`
- **問題**:
  - 常に `--check` で実行するため、出力ファイル（.nes / .lst / .s28）の中身と `-s` / `-S` の出力を比較していない。
  - 失敗するケースは success の一致しか見ていない。
  - シンボルは「Rust にあるものが C# にもあるか」の片方向でしか比較していない。
  - Windows でしか実行されない。
  - 既存の fixture にはタブと `.list` を併用するものがない（X5 を検出できない）。
- **修正案**:
  - 出力のバイト列も比較する。
  - 失敗するケースでは、エラーの行番号と件数を比較する。
  - シンボルを双方向で比較する。
  - 生成済みのゴールデン JSON をコミットし、Linux / macOS でも比較できるようにする。

### T2【中】core のテストカバレッジの穴
- **テストのない機能**:
  - `.if` / `.ifdef` / `.ifndef` / `.else` / `.endif`、`.equ`、`.rs` / `.rsset`
  - `.proc` / `.endp` / `.procgroup` / `.call`
  - `.fail`、`.opt`、`.mlist` / `.nomlist` / `.nolist`、`.bss` / `.code` / `.data`、`.byte` / `.word`
  - `.ineschr`、`.autozp`、`.mac`
  - E_LIMIT（展開行数、マクロのネスト、include のネスト）
  - 閉じていない行継続、1MiB を超えるソース、`-l0..3` のリストの内容、srec の内容
- **追加すべきテスト**:
  - (a) `.if 0` / `.else` のネスト、および `.endif` が欠けた場合
  - (b) `.rsset $10` → `a .rs 2` → `b .rs 1` で b == $12
  - (c) `.proc` / `.call` のバンク配置
  - (d) `.fail` で success=false になり、行番号が正しい
  - (e) 自己 include で E_LIMIT が出る
  - (f) 再帰マクロで E_LIMIT が出る
  - (g) 1MiB+1 バイトのソースでエラー
  - (h) 末尾が `\` の行でエラー
  - (i) srec のチェックサム
  - (j) `list_level` 4 でエラー
  - 本レビューの P1〜P3、C1〜C12 の再現入力を回帰テストにする

### T3【中】MCP 統合テストがプロトコル異常系とセキュリティ境界をほぼ検証していない
- **場所**: `crates/nesasm-mcp/tests/stdio.rs:90-152`
- **追加すべきテスト**:
  - (a) 不正な JSON を送った後も応答が続く
  - (b) 未知のツールで -32602、(c) 未知のメソッドで -32601
  - (d) 未知の引数フィールドで isError になる
  - (e) `ping` と、文字列 id がそのまま返ること
  - (f) 初期化前の `tools/call`
  - (g) 古い protocolVersion でのバージョン交渉
  - (h) `.include "../x"`、絶対パスの `.incbin`、`include_paths:["/"]` が拒否される
  - (i) 出力先が `main.asm` / `.git/x` の場合（S1 の修正後の期待値）
  - (j) 2 つの `tools/call` を並行して送り、どちらも完了する
  - (k) structuredContent が outputSchema に適合する
  - (l) Windows のジャンクション（`mklink /J`）経由でルート外に出られない

### T4【低】テストの脆さ
- **場所**: `cli/tests/cli.rs:11-12, 70`, `mcp/tests/stdio.rs:20, 34-37`
- **問題**:
  - 一時ディレクトリ名が PID だけで、事前のクリーンアップもない。
  - テストが panic しても、一時ディレクトリが削除されず、`-watch` の子プロセスも kill されない。
  - MCP のリーダースレッドが panic しても、10 秒のタイムアウトまで原因が表に出ない。
  - `-watch` のテストは mtime と 200ms のポーリングに依存している。
- **修正案**:
  - `tempfile::TempDir` を使う。
  - 子プロセスを、Drop 時に kill するガードで包む。
  - リーダースレッドのエラーを channel でテスト側に伝える。

### T5【低】CI の設定
- **場所**: `.github/workflows/rust.yml`
- **問題**:
  - `push` と `pull_request` の両方をトリガーにしているため、PR ではジョブが二重に走る。
  - fmt を 4 OS すべてで実行している。
  - Actions を SHA で固定していない。
  - toolchain の指定が `rust-toolchain.toml` と二重管理になっている。
  - Windows で symlink / ジャンクションのテストが実行されない。
- **修正案**:
  - push のブランチを限定する。
  - fmt は 1 OS だけで実行する。
  - Actions を SHA で固定する。
  - toolchain の指定を `rust-toolchain.toml` に一本化する。

---

## 7. 推奨する着手順

1. **S1**: MCP の書き込み先を制限する（公開前に必須）。
2. **P1〜P3**: panic と ROM の黙った重ね書きを修正する。
3. **C1, C2, C3, C4, C5**: 誤ったコードの生成を修正する（括弧オペランドの判定、PROC 内の定数、即値の `<` / `>`、バンク検査）。
4. **D1〜D3**: DoS 対策として評価予算・展開量の上限、INCCHR のサイズ上限、MCP のタイムアウトを入れる。
5. **X1〜X7**: 主要な C# 非互換（`.page`、`0x` リテラル、1 桁目のラベル、エラー時の挙動、リスト出力、`\"`）を解消する。
6. **T1〜T3**: 上記の回帰テストを追加し、C# との比較スクリプトを強化する。
7. **設計 B, C, F, G**: enum 化、`Position` の Copy 化、trace の共有、エラー型の導入。そのうえで `execute` を分割する（A）。
8. ~~判断事項: DEFCHR と `\#` の C# バグを残すか、拡張を文書化するか~~ → 対応済み（§3 参照）。
