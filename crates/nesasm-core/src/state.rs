// Legacy symbol metadata distinguishes constants/RAM from physical ROM banks.
pub(super) const RESERVED_BANK: usize = 0xf0;
pub(super) const PROCEDURE_BANK: usize = 0xf1;

/// The discriminants are part of the legacy placement-map format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub(super) enum Section {
    ZeroPage = 0,
    Bss = 1,
    Code = 2,
    Data = 3,
}

impl Section {
    pub(super) const fn index(self) -> usize {
        self as usize
    }

    pub(super) const fn is_ram(self) -> bool {
        matches!(self, Self::ZeroPage | Self::Bss)
    }

    pub(super) const fn map_byte(self, page: usize) -> u8 {
        self as u8 + ((page as u8) << 5)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Pass {
    Layout,
    Emit,
}

impl Pass {
    pub(super) const fn is_layout(self) -> bool {
        matches!(self, Self::Layout)
    }

    pub(super) const fn is_emitting(self) -> bool {
        matches!(self, Self::Emit)
    }
}
