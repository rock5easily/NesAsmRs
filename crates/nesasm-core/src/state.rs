//! Memory layout of the NES target and small enums shared by the engine.

/// Size of one assembler bank (and of one CPU page).
pub(crate) const BANK_SIZE: usize = 0x2000;
/// Number of 8 KiB ROM banks.
pub(crate) const MAX_BANKS: usize = 128;
/// Total ROM payload size.
pub(crate) const ROM_LIMIT: usize = MAX_BANKS * BANK_SIZE;
/// Zero page is $0000-$00FF.
pub(crate) const ZP_LIMIT: usize = 0x100;
/// BSS starts after the stack page and ends with internal RAM.
pub(crate) const BSS_START: usize = 0x200;
pub(crate) const BSS_LIMIT: usize = 0x800;
/// CPU page that code starts in ($E000), as in the C# version.
pub(crate) const START_PAGE: usize = 7;
/// CPU page procedures are assembled for ($A000).
pub(crate) const PROC_PAGE: usize = 5;
/// CPU page of the CALL trampoline bank ($8000).
pub(crate) const CALL_PAGE: usize = 4;
/// Bank number used for procedures before relocation (legacy value).
pub(crate) const PROCEDURE_BANK: usize = 0xf1;
/// Macro expansion plus include depth.
pub(crate) const MAX_NESTING: usize = 32;
pub(crate) const MAX_CONDITIONAL_NESTING: usize = 64;
/// Source lines processed per pass, after expansion.
pub(crate) const STEP_LIMIT: usize = 1_000_000;
/// Largest PCX accepted: 1024x768 with up to four 8-bit planes, RLE worst case.
pub(crate) const PCX_LIMIT: u64 = 128 + 2 * 4 * 1024 * 768 + 769;

/// The discriminants are part of the legacy placement-map format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub(crate) enum Section {
    ZeroPage = 0,
    Bss = 1,
    Code = 2,
    Data = 3,
}

impl Section {
    pub(crate) const fn index(self) -> usize {
        self as usize
    }

    pub(crate) const fn is_ram(self) -> bool {
        matches!(self, Self::ZeroPage | Self::Bss)
    }

    pub(crate) const fn map_byte(self, page: usize) -> u8 {
        self as u8 + ((page as u8) << 5)
    }

    pub(crate) const fn from_map_byte(byte: u8) -> crate::SectionKind {
        match byte & 0x0f {
            0 => crate::SectionKind::ZeroPage,
            1 => crate::SectionKind::Bss,
            3 => crate::SectionKind::Data,
            _ => crate::SectionKind::Code,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pass {
    Layout,
    Emit,
}

impl Pass {
    pub(crate) const fn is_layout(self) -> bool {
        matches!(self, Self::Layout)
    }

    pub(crate) const fn is_emitting(self) -> bool {
        matches!(self, Self::Emit)
    }
}
