//! code written apart from the title, compiled for its ARM11 and linked at a
//! fixed address of its own, which replaces some of the title's functions.
//! it is recompiled like a module the title loaded at that address, and
//! each function it replaces gets a jump to the new code in place of its
//! first instructions.

use std::collections::BTreeSet;
use std::path::Path;

use crate::discover::{Program, Source};
use crate::image::Segment;

/// the name its recompiled code is known by, as a module's is.
pub const NAME: &str = "decomp";

pub struct Decomp {
    /// where the code is loaded and runs.
    pub base: u32,
    pub code: Vec<u8>,
    /// the functions in it, odd for Thumb.
    pub entries: Vec<u32>,
    /// the title's functions it replaces and the code that takes over, the
    /// first odd when it is Thumb.
    pub hooks: Vec<(u32, u32)>,
}

impl Decomp {
    /// reads dir/decomp.bin and dir/decomp.txt, which say `base 0x...`, and
    /// `entry 0x...` and `hook 0x<old> 0x<new>` on lines of their own.
    pub fn read(dir: &Path) -> Result<Decomp, String> {
        let read = |name: &str| std::fs::read(dir.join(name)).map_err(|error| format!("could not read {}, {error}", dir.join(name).display()));
        let code = read("decomp.bin")?;
        let text = String::from_utf8_lossy(&read("decomp.txt")?).into_owned();
        let mut decomp = Decomp { base: 0, code, entries: Vec::new(), hooks: Vec::new() };
        for line in text.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')) {
            let words: Vec<&str> = line.split_whitespace().collect();
            let number = |i: usize| {
                let word = words.get(i).ok_or_else(|| format!("decomp.txt: '{line}' is short"))?;
                u32::from_str_radix(word.trim_start_matches("0x"), 16).map_err(|_| format!("decomp.txt: '{word}' is not an address"))
            };
            match words[0] {
                "base" => decomp.base = number(1)?,
                "entry" => decomp.entries.push(number(1)?),
                "hook" => decomp.hooks.push((number(1)?, number(2)?)),
                // a module's function, hooked by the emulator as the module loads
                "module_hook" | "module_import" => {}
                other => return Err(format!("decomp.txt: '{other}' is not a line it knows")),
            }
        }
        if decomp.base == 0 {
            return Err("decomp.txt has no base".to_owned());
        }
        Ok(decomp)
    }

    /// the jump that replaces a function's first instructions: to the new
    /// code in ARM state, from either state. an ARM function's is
    /// `ldr pc, [pc, #-4]` and the address, a Thumb function's switches
    /// first with `bx pc`, which lands on the next whole word.
    pub fn stub(old: u32, new: u32) -> Vec<u8> {
        const LDR_PC: u32 = 0xE51F_F004;
        const BX_PC: u16 = 0x4778;
        const NOP: u16 = 0x46C0;
        let mut bytes = Vec::new();
        if old & 1 == 0 {
            bytes.extend(LDR_PC.to_le_bytes());
        } else {
            bytes.extend(BX_PC.to_le_bytes());
            // the ARM code starts at the next whole word: after a nop from
            // a word's start, straight after the bx from half a word in
            if old & 2 == 0 {
                bytes.extend(NOP.to_le_bytes());
            }
            bytes.extend(LDR_PC.to_le_bytes());
        }
        bytes.extend((new & !1).to_le_bytes());
        bytes
    }

    /// writes the jumps over the functions in text, the executable's code.
    pub fn patch(&self, text: &mut Segment) {
        for &(old, new) in &self.hooks {
            let at = old & !1;
            let stub = Decomp::stub(old, new);
            let offset = (at - text.base) as usize;
            if let Some(slot) = text.bytes.get_mut(offset..offset + stub.len()) {
                slot.copy_from_slice(&stub);
            }
        }
    }

    /// the code as a module's program: offsets from where it is loaded.
    pub fn program(&self) -> Program {
        let seeds = self.entries.iter().map(|&address| (address - self.base, Source::Export)).collect();
        Program { text: Segment { base: 0, bytes: self.code.clone() }, seeds, slots: Some(BTreeSet::new()) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arm_function_jumps_with_ldr() {
        let stub = Decomp::stub(0x0040_2000, 0x0780_0004);
        assert_eq!(stub, [0x04, 0xF0, 0x1F, 0xE5, 0x04, 0x00, 0x80, 0x07]);
    }

    #[test]
    fn thumb_function_switches_state_first() {
        // word aligned: bx pc, a nop, then the ARM jump
        let stub = Decomp::stub(0x0040_2001, 0x0780_0004);
        assert_eq!(&stub[..4], [0x78, 0x47, 0xC0, 0x46]);
        assert_eq!(stub.len(), 12);
        // half a word in: bx pc lands on the next word, where the jump is
        let stub = Decomp::stub(0x0040_2003, 0x0780_0004);
        assert_eq!(&stub[..2], [0x78, 0x47]);
        assert_eq!(&stub[2..6], [0x04, 0xF0, 0x1F, 0xE5]);
        assert_eq!(stub.len(), 10);
    }
}
