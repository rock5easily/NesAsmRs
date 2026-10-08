//! The ROM image and byte emission.

use super::Engine;
use crate::error::{AsmError, AsmResult};
use crate::state::{BANK_SIZE, CALL_PAGE, MAX_BANKS, ROM_LIMIT, Section};
use crate::{BankUsage, Segment};

/// ROM bytes, the legacy placement map and which bytes were written. Storage
/// grows with the highest bank written instead of reserving the whole ROM.
#[derive(Default)]
pub(super) struct RomImage {
    binary: Vec<u8>,
    map: Vec<u8>,
    /// One bit per ROM byte.
    occupied: Vec<u64>,
    pub max_bank: usize,
}

impl RomImage {
    fn grow(&mut self, end: usize) {
        if self.binary.len() < end {
            let end = end.div_ceil(BANK_SIZE) * BANK_SIZE;
            self.binary.resize(end, 0);
            // The C# port leaves the map's unassigned bytes zero-initialized;
            // occupancy is tracked separately.
            self.map.resize(end, 0);
            self.occupied.resize(end / 64, 0);
        }
    }

    pub fn write(&mut self, address: usize, bytes: &[u8], map_byte: u8) {
        let end = address + bytes.len();
        self.grow(end);
        self.binary[address..end].copy_from_slice(bytes);
        self.map[address..end].fill(map_byte);
        for a in address..end {
            self.occupied[a / 64] |= 1 << (a % 64);
        }
    }

    fn is_occupied(&self, address: usize) -> bool {
        self.occupied
            .get(address / 64)
            .is_some_and(|word| word & (1 << (address % 64)) != 0)
    }

    /// Used bytes and section runs of one bank, for the segment usage report.
    pub fn bank_usage(&self, bank: usize, name: Option<String>) -> BankUsage {
        let base = bank * BANK_SIZE;
        let map = |offset: usize| self.map.get(base + offset).copied().unwrap_or(0);
        let occupied = |offset: usize| self.is_occupied(base + offset);
        let mut segments = Vec::new();
        let mut offset = 0;
        while offset < BANK_SIZE {
            if !occupied(offset) {
                offset += 1;
                continue;
            }
            let section = map(offset) & 0x0f;
            let start = offset;
            while offset < BANK_SIZE && occupied(offset) && map(offset) & 0x0f == section {
                offset += 1;
            }
            segments.push(Segment {
                section: Section::from_map_byte(section),
                start: (usize::from(map(start) >> 5) * BANK_SIZE) + start,
                size: offset - start,
            });
        }
        BankUsage {
            bank,
            name,
            used: (0..BANK_SIZE).filter(|&o| occupied(o)).count(),
            capacity: BANK_SIZE,
            segments,
        }
    }

    /// The binary and map, `len` bytes long.
    pub fn into_parts(mut self, len: usize) -> (Vec<u8>, Vec<u8>) {
        self.grow(len);
        self.binary.truncate(len);
        self.map.truncate(len);
        (self.binary, self.map)
    }
}

impl Engine<'_> {
    pub(super) fn emit(&mut self, bytes: &[u8]) -> AsmResult<()> {
        self.emit_impl(Some(bytes), bytes.len(), false)
    }

    /// Emits file or tile data, which may continue into the next bank.
    pub(super) fn emit_buffer(&mut self, bytes: &[u8]) -> AsmResult<()> {
        self.emit_impl(Some(bytes), bytes.len(), true)
    }

    /// Advances like [`Self::emit_buffer`] for `len` bytes without data (layout pass).
    pub(super) fn advance_buffer(&mut self, len: usize) -> AsmResult<()> {
        self.emit_impl(None, len, true)
    }

    fn emit_impl(&mut self, data: Option<&[u8]>, len: usize, buffer: bool) -> AsmResult<()> {
        if self.section.is_ram() {
            return Err("Data emission not allowed in RAM section".into());
        }
        let mut pos = self.position;
        let mut done = 0;
        while done < len {
            if pos.offset >= BANK_SIZE {
                if self.procs.inside() {
                    return Err(AsmError::fatal("Procedure exceeds 8 KiB"));
                }
                if !buffer && !self.cat.contains(&pos.bank) {
                    return Err(AsmError::fatal("Bank overflow, offset > $1FFF"));
                }
                pos.bank += 1;
                pos.page = next_page(pos.page, buffer);
                pos.offset = 0;
            }
            let chunk = (len - done).min(BANK_SIZE - pos.offset);
            let address = pos.linear();
            if self.pass.is_emitting() && address >= ROM_LIMIT {
                return Err(AsmError::fatal("ROM limit exceeded"));
            }
            if pos.bank < MAX_BANKS {
                self.rom.max_bank = self.rom.max_bank.max(pos.bank);
            }
            if self.pass.is_emitting()
                && let Some(data) = data
            {
                self.rom.write(
                    address,
                    &data[done..done + chunk],
                    self.section.map_byte(pos.page),
                );
            }
            pos.offset += chunk;
            done += chunk;
        }
        self.position = pos;
        // A procedure keeps offset 8192 at its end so ENDP records the full size.
        if self.position.offset == BANK_SIZE
            && !self.procs.inside()
            && (buffer || self.cat.contains(&self.position.bank))
        {
            self.position.bank += 1;
            self.position.page = next_page(self.position.page, buffer);
            self.position.offset = 0;
        }
        Ok(())
    }
}

/// The CPU page after a bank change. Data that runs past $FFFF continues at
/// $8000 (page 4) instead of wrapping to page 0.
fn next_page(page: usize, buffer: bool) -> usize {
    let page = (page + 1) & 7;
    if buffer && page == 0 { CALL_PAGE } else { page }
}
