//! Reading assembled ROMs: iNES header, interrupt vectors, hex dumps and a
//! 6502 disassembler that writes NESASM syntax.

use crate::opcode::{Mnemonic, Mode};
use crate::state::BANK_SIZE;
use crate::{AssembleResult, BankRef, Symbol};
#[cfg(feature = "schema")]
use schemars::JsonSchema;
use serde::Serialize;
use std::collections::BTreeMap;

const MAGIC: &[u8; 4] = b"NES\x1a";
const HEADER_SIZE: usize = 16;
const TRAINER_SIZE: usize = 512;
const PRG_UNIT: usize = 16 * 1024;
const CHR_UNIT: usize = 8 * 1024;
/// CPU address of the NMI, RESET and IRQ vectors.
const VECTORS: u32 = 0xfffa;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Mirroring {
    Horizontal,
    Vertical,
    FourScreen,
}

/// The fields of an iNES header.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub struct InesHeader {
    /// PRG ROM size in 16 KiB units.
    pub prg_16k: u8,
    /// CHR ROM size in 8 KiB units (0: CHR RAM).
    pub chr_8k: u8,
    pub mapper: u8,
    pub mirroring: Mirroring,
    pub battery: bool,
    pub trainer: bool,
    /// The header uses the NES 2.0 format, whose extra fields are not decoded.
    pub nes2: bool,
}

impl InesHeader {
    pub fn parse(header: &[u8]) -> Result<Self, String> {
        if header.len() < HEADER_SIZE || &header[..4] != MAGIC {
            return Err("Not an iNES header".into());
        }
        let (flags6, flags7) = (header[6], header[7]);
        Ok(Self {
            prg_16k: header[4],
            chr_8k: header[5],
            mapper: (flags6 >> 4) | (flags7 & 0xf0),
            mirroring: if flags6 & 0x08 != 0 {
                Mirroring::FourScreen
            } else if flags6 & 0x01 != 0 {
                Mirroring::Vertical
            } else {
                Mirroring::Horizontal
            },
            battery: flags6 & 0x02 != 0,
            trainer: flags6 & 0x04 != 0,
            nes2: flags7 & 0x0c == 0x08,
        })
    }
}

/// The interrupt vectors at $FFFA-$FFFF of one bank.
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub struct Vectors {
    pub bank: usize,
    pub nmi: u16,
    pub reset: u16,
    pub irq: u16,
}

/// Sixteen bytes of a hex dump.
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub struct HexRow {
    /// CPU address of the first byte.
    pub address: u32,
    /// Bytes as hex pairs separated by spaces.
    pub bytes: String,
}

/// One disassembled instruction.
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub struct Disassembled {
    pub address: u32,
    pub bytes: String,
    /// Labels defined at this address.
    pub labels: Vec<String>,
    /// The instruction in NESASM syntax, or `.db $xx` for an unknown opcode.
    pub instruction: String,
    /// Symbols whose value is the operand address.
    pub operand_symbols: Vec<String>,
    /// A 65C02 form the NES CPU does not execute as such.
    pub cmos_only: bool,
}

/// A ROM payload (without header) split into 8 KiB banks.
#[derive(Clone, Debug, Default)]
pub struct Rom {
    pub header: Option<InesHeader>,
    pub payload: Vec<u8>,
    /// Banks known to hold $FFFA-$FFFF (from assembly).
    vector_banks: Vec<usize>,
}

impl Rom {
    /// A `.nes` file (with iNES header) or a raw `.bin` payload.
    pub fn from_file(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() < HEADER_SIZE || &bytes[..4] != MAGIC {
            return Ok(Self {
                payload: bytes.to_vec(),
                ..Self::default()
            });
        }
        let header = InesHeader::parse(&bytes[..HEADER_SIZE])?;
        let start = HEADER_SIZE + if header.trainer { TRAINER_SIZE } else { 0 };
        let payload = bytes.get(start..).ok_or("File ends inside the trainer")?;
        Ok(Self {
            header: Some(header),
            payload: payload.to_vec(),
            vector_banks: Vec::new(),
        })
    }

    /// The ROM of a successful assembly.
    pub fn from_assembly(result: &AssembleResult) -> Self {
        let vector_banks = result
            .banks
            .iter()
            .filter(|bank| {
                bank.segments.iter().any(|s| {
                    let start = s.start as u32;
                    start <= VECTORS && VECTORS + 5 < start + s.size as u32
                })
            })
            .map(|bank| bank.bank)
            .collect();
        Self {
            header: InesHeader::parse(&result.header).ok(),
            payload: result.binary.clone(),
            vector_banks,
        }
    }

    pub fn bank_count(&self) -> usize {
        self.payload.len().div_ceil(BANK_SIZE)
    }

    fn bank(&self, bank: usize) -> Result<&[u8], String> {
        let start = bank * BANK_SIZE;
        if bank >= self.bank_count() {
            return Err(format!(
                "Bank {bank} does not exist; the ROM has {} banks",
                self.bank_count()
            ));
        }
        Ok(&self.payload[start..(start + BANK_SIZE).min(self.payload.len())])
    }

    /// The vectors of the banks that hold them: banks the assembly placed at
    /// $FFFA, or else the last PRG bank (the last bank without a header).
    pub fn vectors(&self) -> Vec<Vectors> {
        let banks = if !self.vector_banks.is_empty() {
            self.vector_banks.clone()
        } else if let Some(header) = &self.header {
            let prg_banks = usize::from(header.prg_16k) * PRG_UNIT / BANK_SIZE;
            prg_banks.checked_sub(1).into_iter().collect()
        } else {
            self.bank_count().checked_sub(1).into_iter().collect()
        };
        let word = |data: &[u8], offset: usize| {
            data.get(offset..offset + 2)
                .map(|w| u16::from_le_bytes([w[0], w[1]]))
        };
        banks
            .into_iter()
            .filter_map(|bank| {
                let data = self.bank(bank).ok()?;
                let offset = VECTORS as usize % BANK_SIZE;
                Some(Vectors {
                    bank,
                    nmi: word(data, offset)?,
                    reset: word(data, offset + 2)?,
                    irq: word(data, offset + 4)?,
                })
            })
            .collect()
    }

    /// Problems that would stop the ROM from loading or starting.
    pub fn warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        match &self.header {
            None => warnings.push("No iNES header (raw payload)".into()),
            Some(header) => {
                let expected =
                    usize::from(header.prg_16k) * PRG_UNIT + usize::from(header.chr_8k) * CHR_UNIT;
                if header.prg_16k == 0 {
                    warnings.push("The header declares no PRG ROM (.inesprg)".into());
                }
                if expected != self.payload.len() {
                    warnings.push(format!(
                        "The header declares {} bytes of PRG+CHR ROM, the data has {}",
                        expected,
                        self.payload.len()
                    ));
                }
            }
        }
        let vectors = self.vectors();
        if vectors.is_empty() {
            warnings.push("No bank holds the interrupt vectors at $FFFA".into());
        }
        for v in vectors {
            if v.reset < 0x8000 {
                warnings.push(format!(
                    "The RESET vector in bank {} is ${:04X}, outside PRG ROM",
                    v.bank, v.reset
                ));
            }
        }
        warnings
    }

    /// `length` bytes from `address` (CPU address within `bank`).
    pub fn hexdump(&self, bank: usize, address: u32, length: usize) -> Result<Vec<HexRow>, String> {
        let data = self.bank(bank)?;
        let start = address as usize % BANK_SIZE;
        let end = (start + length).min(data.len());
        Ok(data
            .get(start..end)
            .unwrap_or_default()
            .chunks(16)
            .enumerate()
            .map(|(i, row)| HexRow {
                address: address + (i * 16) as u32,
                bytes: hex(row),
            })
            .collect())
    }

    /// Disassembles `count` instructions from `address` (CPU address within
    /// `bank`), naming labels and operands from `symbols`.
    pub fn disassemble(
        &self,
        bank: usize,
        address: u32,
        count: usize,
        symbols: &BTreeMap<String, Symbol>,
    ) -> Result<Vec<Disassembled>, String> {
        let data = self.bank(bank)?;
        let table = decode_table();
        let symbols = SymbolIndex::new(symbols);
        let mut offset = address as usize % BANK_SIZE;
        let mut pc = address;
        let mut lines = Vec::new();
        while lines.len() < count && offset < data.len() {
            let code = data[offset];
            let decoded = table[usize::from(code)];
            let length = decoded.map_or(1, |(_, mode)| operand_size(mode) + 1);
            let line = match (decoded, data.get(offset..offset + length)) {
                (Some((mnemonic, mode)), Some(bytes)) => {
                    let operand = match bytes.len() {
                        2 => u32::from(bytes[1]),
                        3 => u32::from(u16::from_le_bytes([bytes[1], bytes[2]])),
                        _ => 0,
                    };
                    let value = if mode == Mode::Rel {
                        (pc + 2).wrapping_add(operand as u8 as i8 as u32) & 0xffff
                    } else {
                        operand
                    };
                    Disassembled {
                        address: pc,
                        bytes: hex(bytes),
                        labels: symbols.labels(bank, pc),
                        instruction: format_instruction(mnemonic, mode, value),
                        operand_symbols: symbols.operand(bank, mode, value),
                        cmos_only: is_cmos_only(code),
                    }
                }
                // Unknown opcode, or an instruction cut off by the end of the bank.
                _ => Disassembled {
                    address: pc,
                    bytes: hex(&data[offset..=offset]),
                    labels: symbols.labels(bank, pc),
                    instruction: format!(".db ${code:02X}"),
                    operand_symbols: Vec::new(),
                    cmos_only: false,
                },
            };
            let size = if line.instruction.starts_with(".db") {
                1
            } else {
                length
            };
            offset += size;
            pc += size as u32;
            lines.push(line);
        }
        Ok(lines)
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Opcode to instruction, from the assembler's own table.
fn decode_table() -> [Option<(Mnemonic, Mode)>; 256] {
    const MODES: [Mode; 15] = [
        Mode::Acc,
        Mode::Imm,
        Mode::Zp,
        Mode::Zpx,
        Mode::Zpy,
        Mode::Zi,
        Mode::Zix,
        Mode::Ziy,
        Mode::Abs,
        Mode::Ax,
        Mode::Ay,
        Mode::Ind,
        Mode::Ix,
        Mode::Imp,
        Mode::Rel,
    ];
    let mut table = [None; 256];
    for mnemonic in Mnemonic::ALL {
        for mode in MODES {
            if let Some(code) = mnemonic.opcode(mode) {
                table[usize::from(code)] = Some((mnemonic, mode));
            }
        }
    }
    table
}

const fn operand_size(mode: Mode) -> usize {
    match mode {
        Mode::Imp | Mode::Acc => 0,
        Mode::Abs | Mode::Ax | Mode::Ay | Mode::Ind | Mode::Ix => 2,
        _ => 1,
    }
}

/// 65C02 encodings the legacy assembler accepts but the NES CPU (NMOS 6502)
/// does not execute as such.
fn is_cmos_only(code: u8) -> bool {
    matches!(code, 0x12 | 0x32 | 0x52 | 0x72 | 0x92 | 0xb2 | 0xd2 | 0xf2)
        || matches!(code, 0x89 | 0x34 | 0x3c | 0x1a | 0x3a | 0x7c)
}

/// NESASM syntax: `<` for zero page and `[...]` for indirect addressing.
fn format_instruction(mnemonic: Mnemonic, mode: Mode, value: u32) -> String {
    let name = mnemonic.name();
    match mode {
        Mode::Imp => name.to_owned(),
        Mode::Acc => format!("{name} A"),
        Mode::Imm => format!("{name} #${value:02X}"),
        Mode::Zp => format!("{name} <${value:02X}"),
        Mode::Zpx => format!("{name} <${value:02X},X"),
        Mode::Zpy => format!("{name} <${value:02X},Y"),
        Mode::Zi => format!("{name} [${value:02X}]"),
        Mode::Zix => format!("{name} [${value:02X},X]"),
        Mode::Ziy => format!("{name} [${value:02X}],Y"),
        Mode::Abs | Mode::Rel => format!("{name} ${value:04X}"),
        Mode::Ax => format!("{name} ${value:04X},X"),
        Mode::Ay => format!("{name} ${value:04X},Y"),
        Mode::Ind => format!("{name} [${value:04X}]"),
        Mode::Ix => format!("{name} [${value:04X},X]"),
    }
}

/// Symbols by value, for naming addresses.
struct SymbolIndex<'a> {
    rom: BTreeMap<u32, Vec<(u8, &'a str)>>,
    ram: BTreeMap<u32, Vec<&'a str>>,
}

impl<'a> SymbolIndex<'a> {
    fn new(symbols: &'a BTreeMap<String, Symbol>) -> Self {
        let mut rom: BTreeMap<u32, Vec<(u8, &str)>> = BTreeMap::new();
        let mut ram: BTreeMap<u32, Vec<&str>> = BTreeMap::new();
        // Reserved symbols (line 0) such as MAGICKIT are not addresses.
        for s in symbols.values().filter(|s| s.location.line > 0) {
            match s.bank {
                BankRef::Rom(bank) => rom.entry(s.value).or_default().push((bank, &s.name)),
                BankRef::Constant => ram.entry(s.value).or_default().push(&s.name),
                BankRef::Procedure => {}
            }
        }
        Self { rom, ram }
    }
    fn labels(&self, bank: usize, address: u32) -> Vec<String> {
        self.rom
            .get(&address)
            .into_iter()
            .flatten()
            .filter(|(b, _)| usize::from(*b) == bank)
            .map(|(_, name)| (*name).to_owned())
            .collect()
    }
    fn operand(&self, bank: usize, mode: Mode, value: u32) -> Vec<String> {
        match mode {
            Mode::Imp | Mode::Acc | Mode::Imm => Vec::new(),
            _ if value >= 0x8000 => {
                let names = self.rom.get(&value).map(Vec::as_slice).unwrap_or_default();
                // Prefer labels in the same bank; other banks may share the address.
                let same: Vec<String> = names
                    .iter()
                    .filter(|(b, _)| usize::from(*b) == bank)
                    .map(|(_, n)| (*n).to_owned())
                    .collect();
                if same.is_empty() {
                    names.iter().map(|(_, n)| (*n).to_owned()).collect()
                } else {
                    same
                }
            }
            _ => self
                .ram
                .get(&value)
                .into_iter()
                .flatten()
                .map(|n| (*n).to_owned())
                .collect(),
        }
    }
}
