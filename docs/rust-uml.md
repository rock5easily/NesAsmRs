# Rust版のUML図

`crates/nesasm-core`、`crates/nesasm-cli`、`crates/nesasm-mcp` の実装をもとにした図です。
C#版とテスト用の型は対象に含めません。Mermaid対応のMarkdownビューアやGitHubで表示できます。

Rustのstructをクラス、enumを列挙型、traitをインターフェースとして表します。
`*--` は値としての所有、`-->` は借用・参照、`..>` は呼び出し・生成などの依存、
`..|>` はtraitの実装です。`+` は公開、`-` は非公開を表します。
`pub(crate)` と `pub(super)` の型は内部型として扱います。
フィールド・操作は主要なものを抜粋し、ライフタイムとderiveによるtrait実装は省略しています。
モジュールの図示に使う `<<module>>` は説明用の表現で、実在するstructではありません。

## 1. クレートとモジュールの依存関係

UMLのパッケージ間依存をMermaidのフローチャートで表現しています。
矢印は利用側から利用先へ向きます。外部ライブラリは主なものだけを示します。

```mermaid
flowchart TB
    subgraph cli["nesasm-cli / nesasm"]
        cli_main["main.rs：引数・実行・watch・表示"]
    end
    subgraph mcp["nesasm-mcp"]
        mcp_main["main.rs：Server・ツール・stdio中継"]
    end
    subgraph core["nesasm-core"]
        api["lib.rs：公開データ型・build・reference"]
        subgraph engine["engine/"]
            engine_mod["mod.rs：Engine・行処理・シンボル定義"]
            directive["directive.rs：Directive・疑似命令"]
            instruction["instruction.rs：命令エンコード"]
            macros["macros.rs：マクロ定義・展開"]
            conditional["conditional.rs：条件アセンブル"]
            procedure["procedure.rs：PROC・CALL"]
            listing["listing.rs：リスト出力"]
            rom["rom.rs：ROMイメージ・バイト出力"]
        end
        source["source.rs：入力解決・ソース読み込み"]
        expr["expr.rs：式評価"]
        opcode["opcode.rs：Mnemonic・命令コード表"]
        image["image.rs：PCX・CHR変換"]
        state["state.rs：メモリ配置の定数・Pass・Section"]
        error["error.rs：AsmError"]
        output["output.rs：成果物・S-record"]
    end
    cli_main --> api
    mcp_main --> api
    api --> engine_mod
    api --> output
    engine_mod --> directive & instruction & macros & conditional & procedure & listing & rom
    engine_mod --> source & expr & state & error
    directive --> image
    instruction --> opcode
    expr --> api
    expr --> error
    source --> api
    output --> api
    output --> source
    mcp_main --> rmcp["rmcp：MCP"]
    mcp_main --> tokio["tokio：非同期処理"]
    source --> encoding["encoding_rs：SJIS変換"]
    output --> encoding
```

`lib.rs` は `assemble`、`assemble_with_cancel`、`build`、`write_artifacts`、`reference`
と公開データ型を提供します。CLIとMCPはこの公開APIだけを利用します。
`assemble` は入力を読み込み、結果をメモリ上に作ります。`build` はアセンブルに続けて
成果物を書き込み、書き込みの失敗を `E_OUTPUT` 診断として結果に加えます。
JSONスキーマの導出（`schemars`）は `schema` featureで有効になり、MCPだけが使います。

## 2. 公開APIのクラス図

出典：[lib.rs](../crates/nesasm-core/src/lib.rs)、[output.rs](../crates/nesasm-core/src/output.rs)。

```mermaid
classDiagram
    class AssembleRequest {
        +PathBuf input
        +PathBuf working_directory
        +Vec~PathBuf~ include_paths
        +Option~PathBuf~ allowed_root
        +AssembleOptions options
    }
    class AssembleOptions {
        +SourceEncoding encoding
        +bool raw
        +bool auto_zp
        +ListLevel list_level
        +bool macro_listing
        +bool srec
        +bool warning_disabled
    }
    class ListLevel {
        <<enumeration>>
        Off
        Brief
        Normal
        Full
        clamped(level)
    }
    class SourceEncoding {
        <<enumeration>>
        Utf8
        Sjis
    }
    class AssembleResult {
        +bool success
        +Vec~u8~ binary
        +Vec~u8~ map
        +Vec~u8~ header
        +Vec~Diagnostic~ diagnostics
        +BTreeMap symbols
        +BTreeMap regions
        +Vec~BankUsage~ banks
        +RamUsage ram
        +Vec~PathBuf~ dependencies
        +Option~String~ listing
        +Option~String~ srec
        +error_count()
        +push_error(diagnostic)
    }
    class Diagnostic {
        +Severity severity
        +DiagnosticCode code
        +String message
        +SourceLocation location
        +Vec~SourceLocation~ expansion_trace
        +error(code, message, location)
    }
    class DiagnosticCode {
        <<enumeration>>
        Io
        Assembly
        Symbol
        Macro
        Conditional
        Include
        Procedure
        Limit
        Cancelled
        Output
        Timeout
        Internal
        BankOverflow
    }
    class Severity {
        <<enumeration>>
        Error
        Warning
    }
    class SourceLocation {
        +PathBuf file
        +usize line
        +Option~usize~ column
    }
    class Symbol {
        +String name
        +u32 value
        +BankRef bank
        +Option~usize~ page
        +SourceLocation location
        +bool public
        +usize size
        +Option~DataType~ data_type
    }
    class BankRef {
        <<enumeration>>
        Rom(u8)
        Constant
        Procedure
        number()
    }
    class DataType {
        <<enumeration>>
        Bytes
        Binary
        Characters
    }
    class Region {
        +String name
        +Option~usize~ begin
        +Option~usize~ end
        +Option~i64~ size
    }
    class BankUsage {
        +usize bank
        +Option~String~ name
        +usize used
        +usize capacity
        +Vec~Segment~ segments
    }
    class Segment {
        +SectionKind section
        +usize start
        +usize size
    }
    class RamUsage {
        +usize zero_page_end
        +usize bss_end
    }
    class ReferenceTopic {
        <<enumeration>>
        Index
        Instructions
        Directives
        Expressions
        Options
        text()
    }
    class Artifact {
        +ArtifactKind kind
        +PathBuf path
        +usize size
    }
    class ArtifactKind {
        <<enumeration>>
        Rom
        Listing
        Srec
    }
    AssembleRequest "1" *-- "1" AssembleOptions : options
    AssembleOptions *-- SourceEncoding : encoding
    AssembleOptions *-- ListLevel : list_level
    AssembleResult "1" *-- "0..*" Diagnostic : diagnostics
    AssembleResult "1" *-- "0..*" Symbol : symbols
    AssembleResult "1" *-- "0..*" Region : regions
    AssembleResult "1" *-- "0..*" BankUsage : banks
    AssembleResult *-- RamUsage : ram
    BankUsage "1" *-- "0..*" Segment : segments
    Diagnostic *-- Severity : severity
    Diagnostic *-- DiagnosticCode : code
    Diagnostic "1" *-- "1" SourceLocation : location
    Diagnostic "1" *-- "0..*" SourceLocation : expansion_trace
    Symbol *-- BankRef : bank
    Symbol "1" *-- "1" SourceLocation : location
    Symbol "1" *-- "0..1" DataType : data_type
    Artifact *-- ArtifactKind : kind
```

`symbols` は `BTreeMap<String, Symbol>`、`regions` は `BTreeMap<String, Region>` です。
`Artifact` は `AssembleResult` のフィールドではなく、`build`・`write_artifacts` が返す成果物情報です。
`binary` はiNESヘッダを含まないROMペイロードで、`header` と分けて保持します。
JSONでは互換性のため、`DiagnosticCode` を `"E_IO"` などの文字列、`BankRef` を数値
（ROMバンク、定数は240、未配置のプロシージャは241）、`ListLevel` を0〜3の数値で表します。

## 3. アセンブラ内部のクラス図

出典：[engine/](../crates/nesasm-core/src/engine/)、[state.rs](../crates/nesasm-core/src/state.rs)、
[source.rs](../crates/nesasm-core/src/source.rs)、[error.rs](../crates/nesasm-core/src/error.rs)。

```mermaid
classDiagram
    class Engine {
        -AssembleRequest request_ref
        -AssembleResult result
        -Pass pass
        -Position position
        -Rc~str~ scope
        -Section section
        -BTreeMap saved
        -RomImage rom
        -Listing listing
        -Procedures procs
        -Macros macros
        -Conditions conditions
        -SourceCache cache
        -run(lines)
        -parse(text) Statement
        -execute(statement, directive, line)
        -define(name, value, line, public)
        -value(text)
        -instruction(op, operand)
        -emit(bytes)
        -relocate()
        -finish() AssembleResult
    }
    class Position {
        -usize bank
        -usize page
        -usize offset
        pc()
        linear()
    }
    class Cursor {
        -Position position
        -Rc~str~ scope
    }
    class Statement {
        -Option~String~ label
        -String op
        -String operand
    }
    class Directive {
        <<enumeration>>
        If・Macro・Include・Equ・Org・Bank
        Db・Dw・Ds・Incbin・Proc・Call ほか
        parse(op)
        is_conditional()
    }
    class RomImage {
        -Vec~u8~ binary
        -Vec~u8~ map
        -Vec~u64~ occupied
        +usize max_bank
        write(address, bytes, map_byte)
        bank_usage(bank, name)
        into_parts(len)
    }
    class Listing {
        +String text
        +bool enabled
        +bool requested
        +bool macros
        +usize line_number
        shows(pass, line)
    }
    class Procedures {
        -Vec~Procedure~ list
        -HashMap index
        -Vec~ProcFrame~ frames
        -BTreeMap symbols
        -HashMap calls
        inside()
        register_symbol(name)
    }
    class Macros {
        -HashMap definitions
        -usize counter
    }
    class Conditions {
        -Vec~Conditional~ stack
        -Vec~bool~ results
        -BTreeSet undefined
        active()
    }
    class SourceCache {
        -HashMap sources
        -HashMap tiles
    }
    class Line {
        String text
        Rc~Path~ file
        usize line
        Rc~[SourceLocation]~ trace
        bool expanded
        location()
    }
    class AsmError {
        String message
        bool fatal
    }
    class Pass {
        <<enumeration>>
        Layout
        Emit
    }
    class Section {
        <<enumeration>>
        ZeroPage
        Bss
        Code
        Data
        map_byte(page)
    }
    Engine "1" *-- "1" Position : position
    Engine "1" *-- "0..*" Cursor : saved
    Cursor *-- Position
    Engine *-- RomImage
    Engine *-- Listing
    Engine *-- Procedures
    Engine *-- Macros
    Engine *-- Conditions
    Engine *-- SourceCache
    Engine *-- Pass
    Engine *-- Section
    Engine ..> Statement : parse
    Engine ..> Directive : 1回だけ分類
    Engine ..> AsmError : 行エラー
    Macros "1" *-- "0..*" Line : 定義行（展開時に共有）
```

図中の `request_ref` は、実際のフィールド `request: &AssembleRequest` を表します。
`Position` は物理位置だけを持つ `Copy` 型で、ローカルラベルの所属先（`scope`）は別に保持します。
セクション・バンクの切り替えでは、位置と `scope` を `Cursor` として保存・復元します。
各行は `Directive::parse` で1回だけ分類し、別名（`DB`/`BYTE`、`MACRO`/`MAC` など）は同じ値になります。
`SourceCache` は読み込んだソースとPCXの変換結果を2つのパスで共有し、同じファイルを再読込しません。
`Line` はファイルパスを `Rc<Path>`、展開・includeの経路を `Rc<[SourceLocation]>` で共有します。
`AsmError` の `fatal` はバンクあふれや上限超過など、そのパスを中断するエラーを表します。

## 4. 式評価と命令・画像変換

出典：[expr.rs](../crates/nesasm-core/src/expr.rs)、[opcode.rs](../crates/nesasm-core/src/opcode.rs)、
[instruction.rs](../crates/nesasm-core/src/engine/instruction.rs)、[image.rs](../crates/nesasm-core/src/image.rs)。

```mermaid
classDiagram
    class Engine
    class ExprModule {
        <<module>>
        evaluate(text, ctx)
    }
    class Context {
        BTreeMap symbols_ref
        BTreeMap regions_ref
        BTreeMap functions_ref
        str global_ref
        u32 pc
        bool allow_undefined
        Cell function_calls_ref
    }
    class Parser {
        -Vec~Token~ tokens
        -usize pos
        -Context ctx_ref
        -usize depth
        -expr(min)
        -binary(op, left, right)
        -function(name, parens)
    }
    class Token {
        <<enumeration>>
        Number(u32)
        Name(str)
        String(str)
        Op(Op)
        End
    }
    class Op {
        <<enumeration>>
        Add・Sub・Mul・Div・Mod・Shl・Shr
        Or・Xor・And・Complement・Not
        Eq・Ne・Lt・Le・Gt・Ge
        Open・Close・Comma
        precedence()
    }
    class Mnemonic {
        <<enumeration>>
        Adc・And・Asl・… ・Tya
        parse(name)
        opcode(mode)
    }
    class Mode {
        <<enumeration>>
        Acc・Imm・Zp・Zpx・Zpy
        Zi・Zix・Ziy・Abs・Ax・Ay
        Ind・Ix・Imp・Rel
    }
    class Operand {
        -str expr
        -Mode mode
        -Option~u8~ auto_increment
        -Option~u8~ auto_tag
    }
    class ImageModule {
        <<module>>
        packed_tile(rows, validate)
        pcx_tiles(data, args)
    }
    Engine ..> Context : 式評価時に生成
    Engine ..> ExprModule : 式評価
    ExprModule ..> Parser : 字句解析後に生成
    Parser "1" *-- "1..*" Token : tokens
    Token ..> Op
    Engine ..> Operand : オペランド解析
    Engine ..> Mnemonic : 命令コード取得
    Mnemonic ..> Mode
    Engine ..> ImageModule : DEFCHR・INCCHR
```

`Token` の名前と文字列は式テキストのスライスを借用し、トークンごとに `String` を作りません。
`Context` の `_ref` は参照フィールドで、シンボル表・領域表・関数表を所有しません。
`allow_undefined` は通常の式評価ではLayoutパス中に有効になり、未定義シンボルを0として扱います。
`strict_value` はパスに関係なく未定義シンボルを許可しません。
`function_calls` は1回のアセンブル全体での式関数の呼び出し回数で、上限を超えると致命的エラーです。

## 5. CLI・MCPと成果物出力

出典：[CLI main.rs](../crates/nesasm-cli/src/main.rs)、[MCP main.rs](../crates/nesasm-mcp/src/main.rs)、
[output.rs](../crates/nesasm-core/src/output.rs)。

```mermaid
classDiagram
    class CliModule {
        <<module>>
        parse(args)
        run(args)
        print_regions(result)
        print_segment_usage(result, detail)
        main()
    }
    class Arguments {
        -AssembleRequest request
        -Option~PathBuf~ output
        -bool json
        -bool check
        -bool watch
        -usize usage
    }
    class ServerHandler {
        <<interface>>
        get_info()
    }
    class Server {
        -PathBuf root
        -Duration timeout
        -Arc gate
        -ToolRouter tool_router
        -execute(input, write, output)
        -assemble(input)
        -check(input)
        -inspect_rom(input)
        -get_reference(input)
        list_resources()
        read_resource(uri)
    }
    class StdioRelay {
        <<module>>
        stdio_relay()
    }
    class Report {
        -bool success
        -Vec~Diagnostic~ diagnostics
        -BTreeMap symbols
        -BTreeMap regions
        -Vec~BankUsage~ banks
        -Vec~PathBuf~ dependencies
        -Vec~Artifact~ artifacts
    }
    class CoreApi {
        <<module>>
        assemble(request)
        assemble_with_cancel(request, cancel)
        build(request, output, cancel)
    }
    class OutputModule {
        <<module>>
        write_artifacts(result, input, output, base, root, options)
        srec(binary, map)
    }
    class StagedArtifact {
        -PathBuf path
        -PathBuf temp
        -write(kind, path, data)
        -commit()
        drop()
    }
    CliModule ..> Arguments : parse・run
    CliModule ..> CoreApi : check時はassemble、それ以外はbuild
    Server ..|> ServerHandler
    Server ..> StdioRelay : 解析エラー応答・出力の直列化
    Server ..> CoreApi : spawn_blocking・タイムアウトで中断
    Server ..> Report : 構造化応答
    CoreApi ..> OutputModule : build内で書き込み
    OutputModule ..> StagedArtifact : 一時ファイル作成
```

`ServerHandler` はrmcpのtraitです。MCPツールはRust上では非公開メソッドですが、
マクロによってMCPの `assemble`・`check`・`get_reference` として公開されます。
`gate` の型は `Arc<tokio::sync::Mutex<()>>` で、checkとassembleの実行を直列化します。
時間制限を超えると `assemble_with_cancel` に渡したフラグを立て、エンジンは次の行で停止します。
`stdio_relay` はstdinとrmcpの間で行を中継し、JSONとして解析できない行に `-32700` を返します。
MCPでは `allowed_root` を指定するため、出力先は `.nes`／`.bin` と派生ファイルに限られます。

## 6. アセンブルと成果物生成のシーケンス図

CLIとMCPに共通する流れです。MCP固有のMutex・非同期タスク・応答変換は省略しています。

```mermaid
sequenceDiagram
    actor Caller as CLI / MCP
    participant API as core::build
    participant E as Engine
    participant C as SourceCache
    participant X as expr / opcode / image
    participant O as output::write_artifacts
    participant FS as ファイルシステム

    Caller->>API: build(&request, output, cancel)
    API->>E: 要求ごとのEngineを生成
    E->>FS: 入力パスの解決・root内か確認
    E->>C: load(入力)
    C->>FS: 初回のみ読み込み・デコード
    loop Layout → Emit（エラー時は中断）
        E->>E: reset()・run(lines)
        loop ソース行・マクロ展開・include
            E->>E: parse・Directive分類・条件判定
            E->>X: evaluate / opcode / 画像変換
            X-->>E: 値 / バイト列 / AsmError
            E->>E: 位置・シンボル更新、Emit時にROMへ書き込み
            Note over E: 行エラーは報告して続行、fatalならパスを中断
        end
        opt Layout終了・エラーなし
            E->>E: relocate()・予約シンボル更新
        end
    end
    E-->>API: finish()でAssembleResultを生成
    alt 成功
        API->>O: write_artifacts(result, paths, options)
        O->>O: 出力先を検証（root内・拡張子・入力の上書き禁止）
        O->>FS: 一時ファイルへ書き込み後rename
        O-->>API: Vec of Artifact / エラー
        Note over API,O: 書き込み失敗はE_OUTPUT診断として結果に追加
    end
    API-->>Caller: (AssembleResult, Vec of Artifact)
```

Layoutパスは配置とシンボルを計算し、その後 `relocate` がプロシージャの配置を確定します。
EmitパスはそのROMとmapを生成します。Layoutパスでは `INCBIN` のファイル本体を読まず、
サイズだけで位置を進めます。失敗結果では `binary` と `map` を空にします。
一時ファイルは `Drop` で後片付けします。複数成果物のrenameは順に実行されるため、
途中の失敗で既に確定した成果物を元に戻すトランザクションにはなっていません。
