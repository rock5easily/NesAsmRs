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
        cli_main["main.rs：引数・実行・watch"]
    end
    subgraph mcp["nesasm-mcp"]
        mcp_main["main.rs：Server・ツール・応答"]
    end
    subgraph core["nesasm-core"]
        api["lib.rs：公開データ型・reference"]
        engine["engine.rs：assemble・Engine"]
        source["source.rs：入力解決・行読み込み"]
        expr["expr.rs：式評価"]
        opcode["opcode.rs：命令コード表"]
        image["image.rs：PCX・CHR変換"]
        state["state.rs：Pass・Section"]
        output["output.rs：成果物・S-record"]
    end
    cli_main --> api
    cli_main --> engine
    cli_main --> output
    mcp_main --> api
    mcp_main --> engine
    mcp_main --> output
    engine --> api
    engine --> source
    engine --> expr
    engine --> opcode
    engine --> image
    engine --> state
    engine --> output
    expr --> api
    expr --> state
    source --> api
    output --> api
    output --> source
    mcp_main --> rmcp["rmcp：MCP・stdio"]
    mcp_main --> tokio["tokio：非同期処理"]
    source --> encoding["encoding_rs：SJIS変換"]
    output --> encoding
```

`lib.rs` は `assemble`、`write_artifacts`、`Artifact`、`ArtifactKind`、
`resolve_path` を再公開します。CLIとMCPはこの公開APIを利用します。
コアの `assemble` は入力を読み込み、結果をメモリ上に作ります。
成果物ファイルの書き込みは呼び出し側が `write_artifacts` で行います。

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
        +u8 list_level
        +bool macro_listing
        +bool srec
        +bool warning_disabled
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
        +Vec~PathBuf~ dependencies
        +Option~String~ listing
        +Option~String~ srec
    }
    class Diagnostic {
        +Severity severity
        +String code
        +String message
        +SourceLocation location
        +Vec~SourceLocation~ expansion_trace
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
        +usize bank
        +usize page
        +SourceLocation location
        +bool public
        +usize size
        +Option~DataType~ data_type
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
        +usize used
        +usize capacity
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
    AssembleResult "1" *-- "0..*" Diagnostic : diagnostics
    AssembleResult "1" *-- "0..*" Symbol : symbols
    AssembleResult "1" *-- "0..*" Region : regions
    AssembleResult "1" *-- "0..*" BankUsage : banks
    Diagnostic *-- Severity : severity
    Diagnostic "1" *-- "1" SourceLocation : location
    Diagnostic "1" *-- "0..*" SourceLocation : expansion_trace
    Symbol "1" *-- "1" SourceLocation : location
    Symbol "1" *-- "0..1" DataType : data_type
    Artifact *-- ArtifactKind : kind
```

`symbols` は `BTreeMap<String, Symbol>`、`regions` は `BTreeMap<String, Region>` です。
`Artifact` は `AssembleResult` のフィールドではなく、`write_artifacts` が返す成果物情報です。
`binary` はiNESヘッダを含まないROMペイロードで、`header` と分けて保持します。

## 3. アセンブラ内部のクラス図

出典：[engine.rs](../crates/nesasm-core/src/engine.rs)、[state.rs](../crates/nesasm-core/src/state.rs)、
[source.rs](../crates/nesasm-core/src/source.rs)。

```mermaid
classDiagram
    class Engine {
        -AssembleRequest request_ref
        -AssembleResult result
        -Pass pass
        -Position position
        -Section section
        -BTreeMap saved
        -BTreeMap macros
        -BTreeMap functions
        -Vec~Procedure~ procedures
        -Vec~ProcFrame~ frames
        -Vec~Conditional~ conditions
        -Vec~bool~ occupied
        -reset()
        -load(path, trace)
        -run(lines)
        -parse(text)
        -execute(label, op, operand, line)
        -value(text)
        -instruction(op, operand)
        -emit(bytes)
        -relocate()
        -error_at(loc, trace, code, message)
    }
    class AssembleRequest
    class AssembleResult
    class Position {
        -usize bank
        -usize page
        -usize offset
        -String global
    }
    class Procedure {
        -String name
        -usize base
        -usize bank
        -usize org
        -usize size
        -Option~String~ group
    }
    class ProcFrame {
        -String name
        -Position saved
        -bool group
    }
    class Conditional {
        -bool parent
        -bool condition
        -bool otherwise
    }
    class Pass {
        <<enumeration>>
        Layout
        Emit
        is_layout()
        is_emitting()
    }
    class Section {
        <<enumeration>>
        ZeroPage
        Bss
        Code
        Data
        index()
        is_ram()
        map_byte(page)
    }
    class Line {
        String text
        SourceLocation location
        Vec~SourceLocation~ trace
        bool expanded
    }
    class SourceLocation
    class PendingLine {
        <<enumeration>>
        Source(Line)
        ReturnFromInclude(depth, name)
    }
    Engine --> AssembleRequest : requestを借用
    Engine *-- AssembleResult : result
    Engine "1" *-- "1" Position : position
    Engine "1" *-- "0..*" Position : saved
    Engine *-- Pass : pass
    Engine *-- Section : section
    Engine "1" *-- "0..*" Procedure : procedures
    Engine "1" *-- "0..*" ProcFrame : frames
    Engine "1" *-- "0..*" Conditional : conditions
    Engine "1" *-- "0..*" Line : macrosの定義行
    ProcFrame *-- Position : saved
    Line "1" *-- "1" SourceLocation : location
    Line "1" *-- "0..*" SourceLocation : trace
    PendingLine "1" *-- "0..1" Line : Sourceバリアント
    Engine ..> PendingLine : run内の処理キュー
```

図中の `request_ref` は、実際のフィールド `request: &AssembleRequest` を表します。
`saved` は `(Section, usize)` をキーにした位置の保存、`macros` は名前から `Vec<Line>` への対応です。
`PendingLine` のキューは `run` のローカル変数で、Engineのフィールドではありません。
`ReturnFromInclude` はincludeから戻る際の深さと名前を保持します。

## 4. 式評価と命令・画像変換

出典：[expr.rs](../crates/nesasm-core/src/expr.rs)、[opcode.rs](../crates/nesasm-core/src/opcode.rs)、
[image.rs](../crates/nesasm-core/src/image.rs)。

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
    }
    class Parser {
        -Vec~Token~ tokens
        -usize pos
        -Context ctx_ref
        -usize depth
        -symbol(name)
        -expr(min)
        -expect(op)
        -function(name)
    }
    class Token {
        <<enumeration>>
        Number(u32)
        Name(String)
        String(String)
        Op(String)
        End
    }
    class Symbol
    class Region
    class OpcodeModule {
        <<module>>
        opcode(name, mode)
        known(name)
    }
    class Mode {
        <<enumeration>>
        Acc
        Imm
        Zp
        Zpx
        Zpy
        Zi
        Zix
        Ziy
        Abs
        Ax
        Ay
        Ind
        Ix
        Imp
        Rel
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
    Parser --> Context : ctxを借用
    Context --> "0..*" Symbol : シンボル表を借用
    Context --> "0..*" Region : 領域表を借用
    Engine ..> OpcodeModule : 命令コード取得
    OpcodeModule ..> Mode : アドレッシング形式
    Engine ..> ImageModule : DEFCHR・INCCHR
```

`Context` の `_ref` は実際には参照フィールドです。シンボル表・領域表・関数表を所有しません。
`allow_undefined` は通常の式評価ではLayoutパス中に有効になり、未定義シンボルを0として扱います。
`strict_value` はパスに関係なく未定義シンボルを許可しません。

## 5. CLI・MCPと成果物出力

出典：[CLI main.rs](../crates/nesasm-cli/src/main.rs)、[MCP main.rs](../crates/nesasm-mcp/src/main.rs)、
[output.rs](../crates/nesasm-core/src/output.rs)。

```mermaid
classDiagram
    class CliModule {
        <<module>>
        parse(args)
        run(args)
        fingerprint(paths)
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
    class AssembleRequest
    class AssembleOptions
    class AssembleResult
    class ServerHandler {
        <<interface>>
        get_info()
    }
    class Server {
        -PathBuf root
        -Arc gate
        -ToolRouter tool_router
        -new(root)
        -execute(input, write, output)
        -assemble(input)
        -check(input)
        -get_reference(input)
    }
    class AssemblyInput {
        -PathBuf input
        -Vec~PathBuf~ include_paths
        -AssembleOptions options
    }
    class BuildInput {
        -PathBuf input
        -Vec~PathBuf~ include_paths
        -AssembleOptions options
        -Option~PathBuf~ output
    }
    class ReferenceInput {
        -Option~String~ topic
    }
    class Report {
        -bool success
        -Vec~Diagnostic~ diagnostics
        -BTreeMap symbols
        -BTreeMap regions
        -Vec~BankUsage~ banks
        -Vec~PathBuf~ dependencies
        -Vec~Artifact~ artifacts
        from(result)
    }
    class ReferenceReport {
        -bool success
        -String topic
        -String text
    }
    class CoreApi {
        <<module>>
        assemble(request)
        reference(topic)
    }
    class OutputModule {
        <<module>>
        write_artifacts(result, input, output, base, root, options)
        srec(binary, map)
    }
    class StagedArtifact {
        -ArtifactKind kind
        -PathBuf path
        -PathBuf temp
        -usize size
        -Option~File~ file
        -write(kind, path, data)
        -commit()
        drop()
    }
    class Artifact
    CliModule ..> Arguments : parse・run
    Arguments *-- AssembleRequest : request
    CliModule ..> CoreApi : アセンブル
    CliModule ..> OutputModule : 成功かつcheck無効時
    Server ..|> ServerHandler
    Server ..> AssemblyInput : check・execute
    Server ..> BuildInput : assemble
    Server ..> ReferenceInput : get_reference
    AssemblyInput *-- AssembleOptions : options
    BuildInput *-- AssembleOptions : options
    Server ..> AssembleRequest : executeで生成
    Server ..> CoreApi : アセンブル・参照
    Server ..> OutputModule : 成功かつwrite有効時
    Server ..> Report : 構造化応答
    Server ..> ReferenceReport : 参照応答
    AssembleResult ..> Report : Fromで所有権を移動
    Report "1" *-- "0..*" Artifact : artifacts
    OutputModule ..> StagedArtifact : 一時ファイル作成
    StagedArtifact ..> Artifact : commitで生成
```

`ServerHandler` はrmcpのtraitです。MCPツールはRust上では非公開メソッドですが、
マクロによってMCPの `assemble`・`check`・`get_reference` として公開されます。
`gate` の型は `Arc<tokio::sync::Mutex<()>>` で、checkとassembleの実行を直列化します。
`tool_router` の型は `ToolRouter<Self>` です。アセンブル処理は `spawn_blocking` で実行します。
`Report::from` は診断・シンボル・領域・使用量・依存パスを移動し、ROMバイト列を応答に含めません。
成果物情報は書き込み後に `Report.artifacts` に設定されます。

## 6. アセンブルと成果物生成のシーケンス図

CLIとMCPに共通する流れです。MCP固有のMutex・非同期タスク・応答変換は省略しています。

```mermaid
sequenceDiagram
    actor Caller as CLI / MCP
    participant API as core::assemble
    participant E as Engine
    participant S as source
    participant X as expr / opcode / image
    participant O as output::write_artifacts
    participant T as StagedArtifact
    participant FS as ファイルシステム

    Caller->>API: assemble(&request)
    API->>E: 要求ごとのEngineを生成・オプション検証
    API->>S: find_file(request, input)
    S->>FS: パス正規化・root内か確認
    FS-->>S: 入力パス
    S-->>API: 入力パス / エラー
    API->>E: load(path, trace)
    E->>S: read_lines(request, path, trace)
    S->>FS: ソース読み込み
    S-->>E: Vec of Line / エラー
    Note over API,E: 初期化・入力解決・読込失敗時は診断付きで早期return
    loop Layout → Emit（エラー時は中断）
        API->>E: pass設定・reset()・run(lines)
        loop ソース行・マクロ展開・include
            E->>E: 条件判定・parse・execute
            opt 式・命令・画像を処理
                E->>X: evaluate / opcode / 画像変換
                X-->>E: 評価値 / バイト列 / エラー
            end
            E->>E: 位置・シンボル更新、Emit時にバイト出力
        end
        API->>E: 条件・プロシージャの閉じ忘れと診断を確認
        opt Layout終了・エラーなし
            API->>E: relocate()・予約シンボル更新
        end
    end
    API->>E: 成否判定・成功時に結果整形
    E-->>API: AssembleResultの所有権を移動
    API-->>Caller: AssembleResult
    alt 成功かつ成果物書き込みが有効
        Caller->>O: write_artifacts(result, paths, options)
        O->>O: 全出力先を検証・入力上書きを拒否
        loop 全成果物をステージング
            O->>T: write(kind, path, data)
            T->>FS: 一時ファイル作成・write・sync
        end
        loop 全ステージング成功後、各成果物を確定
            O->>T: commit()
            T->>FS: close・rename
            T-->>O: Artifact / エラー
        end
        O-->>Caller: Vec of Artifact / エラー
        Note over Caller,O: 出力失敗は呼び出し側がE_OUTPUT診断へ変換
    else checkまたはアセンブル失敗
        Note over Caller: 成果物を書き込まず結果を返す・表示する
    end
```

Layoutパスは配置とシンボルを計算し、その後 `relocate` がプロシージャの配置を確定します。
Emitパスはその配置に基づいてROMとmapを生成します。
失敗結果では `binary` と `map` を空にします。
一時ファイルは `Drop` で後片付けします。複数成果物のrenameは順に実行されるため、
途中の失敗で既に確定した成果物を元に戻すトランザクションにはなっていません。
