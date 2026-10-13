//! The annotation file: what a person (or an AI reading their notes) knows
//! about a ROM, applied when it is disassembled.

#[cfg(feature = "schema")]
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::path::PathBuf;

/// A CPU address or value: a number, or a string such as `"$C000"`,
/// `"0xC000"` or `"49152"`. Reports show it as `"$C000"`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Address(pub u32);

impl Address {
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        let (digits, radix) = if let Some(hex) = text.strip_prefix('$') {
            (hex, 16)
        } else if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
            (hex, 16)
        } else {
            (text, 10)
        };
        u32::from_str_radix(&digits.replace('_', ""), radix)
            .ok()
            .filter(|_| !digits.is_empty() && !digits.starts_with('+'))
            .map(Self)
            .ok_or_else(|| {
                format!("Invalid address '{text}'; use a number, \"$C000\" or \"0xC000\"")
            })
    }
}

impl std::fmt::Display for Address {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "${:04X}", self.0)
    }
}

impl Serialize for Address {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Address {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Number(u64),
            Text(String),
        }
        match Raw::deserialize(deserializer)? {
            Raw::Number(n) => u32::try_from(n)
                .map(Self)
                .map_err(|_| serde::de::Error::custom("address out of range")),
            Raw::Text(text) => Self::parse(&text).map_err(serde::de::Error::custom),
        }
    }
}

#[cfg(feature = "schema")]
impl JsonSchema for Address {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Address".into()
    }
    fn inline_schema() -> bool {
        true
    }
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "description": "A number, or a string such as \"$C000\", \"0xC000\" or \"49152\"",
            "anyOf": [
                { "type": "integer", "minimum": 0 },
                { "type": "string" }
            ]
        })
    }
}

/// Annotations that turn a ROM into readable, reassemblable source.
#[derive(Clone, Debug, Default, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[serde(default, deny_unknown_fields)]
pub struct DisasmConfig {
    /// ROM file relative to the project root: .nes with an iNES header, or a raw .bin.
    pub rom: PathBuf,
    /// For a raw .bin fragment: the CPU address of its first byte. Required
    /// when the file is not a whole number of 8 KiB banks.
    pub load_address: Option<Address>,
    /// For a ROM hack: the original ROM (same format) relative to the project
    /// root. The source then includes unchanged ranges from it with `.incbin`
    /// and disassembles only the changed ranges.
    pub base: Option<PathBuf>,
    /// With `base`: ranges to disassemble besides the changed bytes, to widen
    /// the source (a whole routine, a table). Changed bytes are always included.
    pub patches: Vec<PatchDef>,
    /// CPU window ($8000, $A000, $C000 or $E000) of PRG banks; others use the
    /// default layout (the last two banks at $C000/$E000, the rest alternating
    /// $8000/$A000; a single 8 KiB bank at $E000).
    pub banks: Vec<BankLayout>,
    /// Code entry points besides the interrupt vectors.
    pub entry_points: Vec<EntryPoint>,
    /// Names of ROM addresses.
    pub labels: Vec<LabelDef>,
    /// Names of RAM, I/O and other addresses outside ROM, emitted as `NAME = $xxxx`.
    pub symbols: Vec<SymbolDef>,
    /// How ranges of the ROM are emitted (code, bytes, words, pointer tables, text).
    pub regions: Vec<RegionDef>,
    /// Comments placed before or on the line of an address.
    pub comments: Vec<CommentDef>,
    /// Operand text replacing what the disassembler would write.
    pub operands: Vec<OperandDef>,
    /// Banks of references to addresses that several banks can hold.
    pub references: Vec<ReferenceDef>,
    pub options: DisasmOptions,
}

#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct BankLayout {
    pub bank: usize,
    /// CPU address of the bank's first byte, a multiple of $2000.
    pub org: Address,
}

#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct EntryPoint {
    /// Needed only when several banks hold the address.
    pub bank: Option<usize>,
    pub address: Address,
    /// Label name (default: an automatic name).
    pub name: Option<String>,
    pub comment: Option<String>,
    /// The routine never returns to its caller, so code after a JSR to it is not followed.
    #[serde(default)]
    pub no_return: bool,
    /// Bytes of inline arguments after each JSR to the routine; they are emitted as data.
    #[serde(default)]
    pub inline_bytes: usize,
    /// The routine switches in the PRG bank held in the first RAM address
    /// at $8000 and the one in the second at $A000 (`null`: not switched),
    /// and changes no other RAM the caller knows.
    pub switch_banks: Option<[Option<Address>; 2]>,
    /// The routine returns with the PRG banks it was called with (such as
    /// a thread yield that other threads' bank switches run during).
    #[serde(default)]
    pub keeps_banks: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct LabelDef {
    pub bank: Option<usize>,
    pub address: Address,
    pub name: String,
    /// Comment placed above the label.
    pub comment: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SymbolDef {
    pub name: String,
    pub value: Address,
    /// Bytes covered (default 1); operands inside are written `name+offset`.
    pub size: Option<u32>,
    pub comment: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum RegionType {
    /// Decode every instruction in the range, even if no flow reaches it.
    Code,
    /// `.db` values.
    #[default]
    Bytes,
    /// `.dw` values.
    Words,
    /// `.dw` addresses, written as labels.
    Pointers,
    /// Low bytes of addresses whose high bytes are the `pair` table; `.db LOW(label)`.
    LowBytes,
    /// High bytes of addresses whose low bytes are the `pair` table; `.db HIGH(label)`.
    HighBytes,
    /// ASCII text as `.db "..."`; other bytes as values.
    Text,
}

#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct RegionDef {
    pub bank: Option<usize>,
    pub start: Address,
    /// Last address (inclusive); give this or `length`.
    pub end: Option<Address>,
    pub length: Option<u32>,
    #[serde(rename = "type", default)]
    pub kind: RegionType,
    /// For low_bytes/high_bytes: start of the other table (same length and bank).
    pub pair: Option<Address>,
    /// For pointer tables: the targets are code entry points (jump tables).
    #[serde(default)]
    pub code_targets: bool,
    /// For pointer tables: stored value minus target, as in `.dw label-1`
    /// for RTS jump tables (-1).
    #[serde(default)]
    pub adjust: i32,
    /// Values per line (default `options.data_per_line`).
    pub columns: Option<usize>,
    /// Comment placed above the region.
    pub comment: Option<String>,
    /// For pointer and low_bytes tables: start of a table of the same
    /// number of entries (and bank) holding the PRG bank (8 KiB) switched in
    /// for each entry. A target that bank holds is resolved to it; others
    /// (in the fixed banks, or the table's own bank) as usual.
    pub bank_table: Option<Address>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum CommentPosition {
    /// At the end of the line.
    #[default]
    Line,
    /// On lines of their own before the address (and its labels).
    Above,
}

#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct CommentDef {
    pub bank: Option<usize>,
    pub address: Address,
    pub text: String,
    #[serde(default)]
    pub position: CommentPosition,
}

#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct OperandDef {
    pub bank: Option<usize>,
    /// Address of the instruction or data value.
    pub address: Address,
    /// The operand as written after the mnemonic (`#LOW(Table)`, `<ptr+1`,
    /// `[ptr],y`) or the data value (`Table-1`). The round-trip check verifies it.
    pub expr: String,
}

#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct PatchDef {
    pub bank: Option<usize>,
    pub start: Address,
    /// Last address (inclusive); give this or `length`.
    pub end: Option<Address>,
    pub length: Option<u32>,
    /// Comment placed above the range.
    pub comment: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ReferenceDef {
    pub bank: Option<usize>,
    /// Address of the referring instruction or pointer.
    pub address: Address,
    /// Bank holding the referenced address.
    pub target_bank: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ChrMode {
    /// `.incbin "<output stem>.chr"`, written next to the source.
    #[default]
    Incbin,
    /// `.db` lines in the source.
    Db,
}

#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[serde(default, deny_unknown_fields)]
pub struct DisasmOptions {
    /// Upper-case mnemonics (default lower case).
    pub uppercase: bool,
    /// Values per `.db`/`.dw` line (default 8).
    pub data_per_line: usize,
    /// Append the CPU address of each line as a comment.
    pub address_comments: bool,
    /// Name RAM addresses automatically (`zp_10`, `ram_0300`).
    pub auto_ram_labels: bool,
    /// Name the PPU, APU and I/O registers (PPUCTRL ... JOY2; default true).
    pub nes_registers: bool,
    pub chr: ChrMode,
    /// Write each bank to `<stem>_bankNN.asm`, included from the main file.
    /// By default, only when the source would exceed 512 KiB.
    pub split_banks: Option<bool>,
    /// Mapper 4 (MMC3) only: the second-to-last PRG bank is at $C000 and the
    /// last at $E000, and $C000-$FFFF is analysed as one range, so code runs
    /// across $DFFF/$E000. With `split_banks`, both are written to
    /// `<stem>_bankNN_MM.asm`.
    pub mapper4_fixed_prg_16k: bool,
}

impl Default for DisasmOptions {
    fn default() -> Self {
        Self {
            uppercase: false,
            data_per_line: 8,
            address_comments: false,
            auto_ram_labels: false,
            nes_registers: true,
            chr: ChrMode::Incbin,
            split_banks: None,
            mapper4_fixed_prg_16k: false,
        }
    }
}

/// Standard names of the NES registers.
pub(crate) const NES_REGISTERS: [(&str, u32); 25] = [
    ("PPUCTRL", 0x2000),
    ("PPUMASK", 0x2001),
    ("PPUSTATUS", 0x2002),
    ("OAMADDR", 0x2003),
    ("OAMDATA", 0x2004),
    ("PPUSCROLL", 0x2005),
    ("PPUADDR", 0x2006),
    ("PPUDATA", 0x2007),
    ("SQ1_VOL", 0x4000),
    ("SQ1_SWEEP", 0x4001),
    ("SQ1_LO", 0x4002),
    ("SQ1_HI", 0x4003),
    ("SQ2_VOL", 0x4004),
    ("SQ2_SWEEP", 0x4005),
    ("SQ2_LO", 0x4006),
    ("SQ2_HI", 0x4007),
    ("TRI_LINEAR", 0x4008),
    ("TRI_LO", 0x400a),
    ("TRI_HI", 0x400b),
    ("NOISE_VOL", 0x400c),
    ("NOISE_LO", 0x400e),
    ("NOISE_HI", 0x400f),
    ("OAMDMA", 0x4014),
    ("SND_CHN", 0x4015),
    ("JOY1", 0x4016),
];

/// $4017 (JOY2 when read, the APU frame counter when written).
pub(crate) const JOY2: (&str, u32) = ("JOY2", 0x4017);

/// Names that cannot be labels or symbols in the generated source.
const RESERVED: [&str; 21] = [
    "A",
    "X",
    "Y",
    "LOW",
    "HIGH",
    "BANK",
    "PAGE",
    "DEFINED",
    "SIZEOF",
    "VRAM",
    "PAL",
    "REGIONSIZE",
    "LOW_BYTE",
    "HIGH_BYTE",
    "MAGICKIT",
    "DEVELO",
    "CDROM",
    "_BSS_END",
    "_BANK_BASE",
    "_NB_BANK",
    "_CALL_BANK",
];

/// Checks a label or symbol name for the generated source.
pub(crate) fn check_name(name: &str) -> Result<(), String> {
    let valid = !name.is_empty()
        && name.len() <= 64
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !name.starts_with(|c: char| c.is_ascii_digit());
    if !valid {
        return Err(format!(
            "Invalid name '{name}': use letters, digits and _, not starting with a digit, up to 64 characters"
        ));
    }
    if RESERVED.iter().any(|r| r.eq_ignore_ascii_case(name)) {
        return Err(format!("Name '{name}' is reserved by the assembler"));
    }
    Ok(())
}
