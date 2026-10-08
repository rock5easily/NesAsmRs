//! The `.lst` listing in the C# column layout:
//!
//! ```text
//! LLLLL  BB:AAAA  XX XX XX  source
//! ```
//! line number, bank and address, then bytes or a value, then the source text.

use super::{Directive, Engine, Position};
use crate::source::Line;
use crate::state::{BANK_SIZE, MAX_BANKS, Pass};
use crate::{AssembleRequest, ListLevel};
use std::borrow::Cow;

const PREFIX_WIDTH: usize = 26;
const ADDRESS_COLUMN: usize = 7;
const DATA_COLUMN: usize = 16;

#[derive(Default)]
pub(super) struct Listing {
    pub text: String,
    /// `.LIST` / `.NOLIST` state.
    pub enabled: bool,
    /// Whether `.LIST` appeared, which requests a listing file.
    pub requested: bool,
    /// List macro expansions (`.MLIST`, `-m`).
    pub macros: bool,
    /// Current line of the outermost (unexpanded) source.
    pub line_number: usize,
}

impl Listing {
    pub fn reset(&mut self, request: &AssembleRequest) {
        self.enabled = false;
        self.macros = request.options.macro_listing;
        self.line_number = 0;
        self.text = format!("#[1]   {}\n", request.input.display());
    }
    /// Whether `line` is listed in this pass.
    pub fn shows(&self, pass: Pass, line: &Line) -> bool {
        pass.is_emitting() && self.enabled && (!line.expanded || self.macros)
    }
    /// Whether include file headers (`#[n] file`) are written.
    pub fn writes_files(&self, pass: Pass, request: &AssembleRequest) -> bool {
        pass.is_emitting() && self.requested && request.options.list_level > ListLevel::Off
    }
}

/// The fixed-width columns in front of the source text.
struct Prefix([u8; PREFIX_WIDTH]);

impl Prefix {
    fn put(&mut self, column: usize, text: &str) {
        for (out, b) in self.0[column..].iter_mut().zip(text.bytes()) {
            *out = b;
        }
    }
}

impl Engine<'_> {
    /// Appends `line` to the listing. `start` is the position before the line;
    /// `value` (EQU, IF, RS) replaces the address with a value.
    pub(super) fn list_line(
        &mut self,
        line: &Line,
        start: Position,
        directive: Option<Directive>,
        bytes: &[u8],
        has_label: bool,
        value: Option<u32>,
    ) {
        let list_level = self.request.options.list_level;
        if list_level == ListLevel::Off {
            return;
        }
        let level = match directive {
            Some(Directive::Defchr) => ListLevel::Full,
            Some(Directive::Db | Directive::Dw) => ListLevel::Normal,
            _ => ListLevel::Off,
        };
        let width = if directive == Some(Directive::Dw) {
            2
        } else {
            3
        };
        // Like the C# version, file includes and reserved space list only their address.
        let address_only = matches!(
            directive,
            Some(Directive::Incbin | Directive::Incchr | Directive::Ds)
        );
        let chunks: Vec<&[u8]> =
            if bytes.is_empty() || address_only || (level > list_level && bytes.len() > 3) {
                vec![&[]]
            } else {
                bytes.chunks(width).collect()
            };
        let bank = if start.bank < MAX_BANKS && !self.section.is_ram() {
            format!("{:02X}", start.bank)
        } else {
            "--".into()
        };
        let shows_address = !bytes.is_empty()
            || has_label
            || matches!(directive, Some(Directive::Proc | Directive::Procgroup));
        let directive_value = match directive {
            Some(Directive::Bank) => Some(self.position.bank),
            Some(Directive::Page) => Some(self.position.page * BANK_SIZE),
            Some(Directive::Rsset) => Some(self.rs as usize),
            Some(
                Directive::Org | Directive::Zp | Directive::Bss | Directive::Code | Directive::Data,
            ) => Some(self.pc() as usize),
            _ => None,
        };
        for (i, chunk) in chunks.iter().enumerate() {
            let mut prefix = Prefix([b' '; PREFIX_WIDTH]);
            if i == 0 && !line.expanded {
                prefix.put(0, &format!("{:5}", self.listing.line_number));
            }
            if shows_address {
                let address = format!("{:04X}", start.pc() + i * width);
                prefix.put(ADDRESS_COLUMN, &format!("{bank}:{}", &address[..4]));
            } else if let Some(v) = directive_value {
                prefix.put(DATA_COLUMN, &format!("{v:04X}"));
            }
            for (j, b) in chunk.iter().enumerate() {
                prefix.put(DATA_COLUMN + j * 3, &format!("{b:02X}"));
            }
            if let Some(value) = value {
                prefix.put(ADDRESS_COLUMN, "       ");
                prefix.put(DATA_COLUMN, &format!("{:04X}", value & 0xffff));
            }
            self.listing
                .text
                .push_str(std::str::from_utf8(&prefix.0).expect("ASCII listing columns"));
            if i == 0 {
                self.listing.text.push_str(&expand_tabs(&line.text));
            }
            self.listing.text.push('\n');
        }
    }
}

/// Expands tabs to 8-column stops measured from the start of the source text,
/// as the C# version does when it reads a line.
fn expand_tabs(text: &str) -> Cow<'_, str> {
    if !text.contains('\t') {
        return text.into();
    }
    let mut out = String::with_capacity(text.len() + 8);
    let mut column = 0;
    for c in text.chars() {
        if c == '\t' {
            let spaces = 8 - column % 8;
            out.extend(std::iter::repeat_n(' ', spaces));
            column += spaces;
        } else {
            out.push(c);
            column += 1;
        }
    }
    out.into()
}
