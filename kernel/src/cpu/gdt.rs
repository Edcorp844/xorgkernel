//! Global Descriptor Table.
//!
//! The GDT describes the segment layout for the CPU. In the
//! kernel's flat model, two segments are enough:
//!
//! - selector 0x08: 32-bit code, base 0, limit 4 GiB
//! - selector 0x10: 32-bit data, base 0, limit 4 GiB
//!
//! Both descriptors are for ring 0. When user mode is added, the
//! GDT will gain ring-3 code and data descriptors, and a TSS.
//!
//! # Reloading CS
//!
//! Loading a new GDT with `lgdt` does not affect CS. CS keeps the
//! descriptor it was loaded with, and a segment register's
//! descriptor cache is populated only when the register itself is
//! reloaded from the GDT.
//!
//! After `lgdt`, the old GDT may no longer be in memory, and the
//! cached CS descriptor may not match any entry in the new GDT.
//! When the CPU later validates CS (for example, when pushing the
//! CS value onto the stack during interrupt delivery), it looks up
//! the selector in the *current* GDT and may find a data segment
//! where a code segment is required. This raises a general
//! protection fault on the first interrupt or exception.
//!
//! The fix is to reload CS explicitly after `lgdt`, using a far
//! return (`lret`) with the new code selector. This is done in
//! [`init`].
//!
//! # Syntax
//!
//! All inline assembly in this file uses AT&T syntax, matching the
//! `.S` files under `src/cpu/`. Every `asm!` block passes
//! `options(att_syntax)`. Memory operands use AT&T parentheses:
//! `(%reg)` rather than Intel's `[%reg]`.

use core::arch::asm;

/// The operand of `lgdt`.
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct GdtPointer {
    limit: u16,
    base: u32,
}

/// The kernel's GDT.
///
/// Entry 0: null descriptor.
/// Entry 1: 32-bit kernel code, selector 0x08.
/// Entry 2: 32-bit kernel data, selector 0x10.
static GDT: [u64; 3] = [0x0000000000000000, 0x00CF9A000000FFFF, 0x00CF92000000FFFF];

/// Selector for the kernel code segment.
const CODE_SELECTOR: u16 = 0x08;

/// Selector for the kernel data segment.
const DATA_SELECTOR: u16 = 0x10;

/// Loads the kernel GDT and reloads the segment registers.
pub fn init() {
    let pointer = GdtPointer {
        limit: (core::mem::size_of::<[u64; 3]>() - 1) as u16,
        base: GDT.as_ptr() as u32,
    };

    unsafe {
        // Load the new GDT. In AT&T syntax a memory operand is
        // written with parentheses around the register that
        // holds the address.
        asm!(
            "lgdt ({pointer})",
            pointer = in(reg) &pointer,
            options(readonly, nostack, preserves_flags, att_syntax),
        );

        // Reload CS with the code selector using a far return.
        asm!(
            "pushl $0x08",
            "pushl $2f",
            "lret",
            "2:",
            options(nostack, att_syntax),
        );

        // Reload the data segment registers with the data
        // selector.
        asm!(
            "movw $0x10, %ax",
            "movw %ax, %ds",
            "movw %ax, %es",
            "movw %ax, %fs",
            "movw %ax, %gs",
            "movw %ax, %ss",
            options(nostack, preserves_flags, att_syntax),
        );
    }
}
