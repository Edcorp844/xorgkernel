//! Global Descriptor Table.
//!
//! The GDT describes the segment layout for the CPU. In the
//! kernel's flat model, each segment has base 0 and limit 4 GiB;
//! the only thing that distinguishes them is the privilege level
//! and the code/data distinction.
//!
//! # Layout
//!
//! ```text
//!   index  selector  descriptor
//!     0      0x00    null
//!     1      0x08    32-bit ring-0 code
//!     2      0x10    32-bit ring-0 data
//!     3      0x18    32-bit ring-3 code
//!     4      0x20    32-bit ring-3 data
//!     5      0x28    TSS (system descriptor)
//! ```
//!
//! The ring-3 descriptors differ from the ring-0 ones only in the
//! DPL field: 3 for the user segments, 0 for the kernel ones.
//! The ring-3 descriptors are what makes it possible for `iret` to
//! load CS and SS with selectors at CPL 3; without them, the
//! attempt raises #GP.
//!
//! The TSS descriptor is a *system* descriptor, not a code or
//! data segment. Its type field identifies it as a 32-bit
//! available TSS (type 9), and its base and limit point at the
//! TSS structure declared in `cpu/tss.rs`. Loading it into the
//! task register with `ltr` tells the CPU where to find `esp0`
//! when a privilege transition occurs.
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

use crate::cpu::tss;

/// The operand of `lgdt`.
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct GdtPointer {
    limit: u16,
    base: u32,
}

/// Number of entries in the GDT.
///
/// See the module documentation for the layout.
const GDT_ENTRIES: usize = 6;

/// The kernel's GDT.
///
/// The descriptors are:
///
/// - `0x00` — null.
/// - `0x08` — 32-bit ring-0 code. Base 0, limit 4 GiB, DPL 0,
///   executable, readable.
/// - `0x10` — 32-bit ring-0 data. Base 0, limit 4 GiB, DPL 0,
///   writable.
/// - `0x18` — 32-bit ring-3 code. Base 0, limit 4 GiB, DPL 3,
///   executable, readable.
/// - `0x20` — 32-bit ring-3 data. Base 0, limit 4 GiB, DPL 3,
///   writable.
/// - `0x28` — TSS. Base is filled in at runtime by [`init`],
///   because the TSS's address is not known at compile time.
///   The entry stored here is a placeholder and is replaced
///   before `lgdt`.
///
/// The kernel and user code/data descriptors are precomputed as
/// `u64` constants. Precomputing them is more legible than
/// assembling them at runtime, and it lets the descriptor values
/// be examined in the source without decoding a struct.
///
/// The descriptor layout for a code or data segment is:
///
/// ```text
///   bits 63-56  base 31-24
///   bits 55-52  flags: G, D/B, L, AVL
///   bits 51-48  limit 19-16
///   bits 47-40  access: P, DPL, S, type
///   bits 39-32  base 23-16
///   bits 31-16  base 15-0
///   bits 15-0   limit 15-0
/// ```
///
/// For a flat 32-bit segment, base is 0, limit is 0xFFFFF with the
/// granularity flag set (giving 4 GiB), and the flags byte is
/// `0xCF` (G=1, D/B=1, L=0, AVL=0). The access byte distinguishes
/// the segments:
///
/// - `0x9A` — P=1, DPL=0, S=1, type=1010 (code, readable)
/// - `0x92` — P=1, DPL=0, S=1, type=0010 (data, writable)
/// - `0xFA` — P=1, DPL=3, S=1, type=1010 (code, readable)
/// - `0xF2` — P=1, DPL=3, S=1, type=0010 (data, writable)
///
/// The TSS descriptor is different: it is a system descriptor
/// (S=0, in the access byte), its type is 1001 (32-bit available
/// TSS), and its access byte is `0x89` (P=1, DPL=0, S=0, type=9).
/// The base and limit must point at the TSS structure; both are
/// filled in at runtime.
static GDT: [u64; GDT_ENTRIES] = [
    // 0x00 — null descriptor. The CPU requires index 0 to be
    // all zeros.
    0x0000000000000000,
    // 0x08 — ring-0 code.
    0x00CF9A000000FFFF,
    // 0x10 — ring-0 data.
    0x00CF92000000FFFF,
    // 0x18 — ring-3 code.
    0x00CFFA000000FFFF,
    // 0x20 — ring-3 data.
    0x00CFF2000000FFFF,
    // 0x28 — TSS. Placeholder; the real value is written by
    // `init` before `lgdt`.
    0x0000000000000000,
];

/// Selector for the kernel code segment.
pub const KERNEL_CODE_SELECTOR: u16 = 0x08;

/// Selector for the kernel data segment.
pub const KERNEL_DATA_SELECTOR: u16 = 0x10;

/// Selector for the user code segment.
///
/// The low two bits are the Requested Privilege Level (RPL),
/// which for a user code selector is 3. So the full selector is
/// `0x18 | 3 == 0x1B`. The constant here is the *index* selector
/// without RPL; the `iret` path sets RPL explicitly when it
/// builds the frame.
#[allow(dead_code)]
pub const USER_CODE_SELECTOR: u16 = 0x18;

/// Selector for the user data segment.
///
/// Same note as [`USER_CODE_SELECTOR`]: the full selector for SS
/// on `iret` is `0x20 | 3 == 0x23`.
#[allow(dead_code)]
pub const USER_DATA_SELECTOR: u16 = 0x20;

/// Selector for the TSS.
///
/// The TSS selector has no RPL: `ltr` ignores the low bits, and
/// the kernel loads the TSS at ring 0 with RPL 0.
pub const TSS_SELECTOR: u16 = 0x28;

/// Builds the TSS descriptor.
///
/// The descriptor is written into the GDT at index 5 by
/// [`init`]. The base is the physical address of the TSS
/// structure, and the limit is `TSS_SIZE - 1`.
///
/// # Descriptor layout for a 32-bit TSS
///
/// ```text
///   bits 63-56  base 31-24
///   bits 55-52  flags: G=0, D/B=0, L=0, AVL=0
///   bits 51-48  limit 19-16
///   bits 47-40  access: P=1, DPL=0, S=0, type=1001
///   bits 39-32  base 23-16
///   bits 31-16  base 15-0
///   bits 15-0   limit 15-0
/// ```
///
/// The granularity flag is 0, so the limit is in bytes, not 4 KiB
/// units. `TSS_SIZE` is 104 bytes, well within the 20-bit limit
/// field's 1 MiB maximum.
fn build_tss_descriptor() -> u64 {
    let base = tss::address() as u64;
    let limit = (tss::TSS_SIZE - 1) as u64;

    let limit_low = limit & 0xFFFF;
    let limit_high = (limit >> 16) & 0xF;
    let base_low = base & 0xFFFF;
    let base_mid = (base >> 16) & 0xFF;
    let base_high = (base >> 24) & 0xFF;

    // Access byte: P=1, DPL=0, S=0, type=9 (32-bit available TSS).
    let access: u64 = 0x89;

    // Flags byte: G=0, D/B=0, L=0, AVL=0.
    let flags: u64 = 0x0;

    limit_low
        | (base_low << 16)
        | (base_mid << 32)
        | (access << 40)
        | (limit_high << 48)
        | (flags << 52)
        | (base_high << 56)
}

/// Loads the kernel GDT, reloads the segment registers, and loads
/// the TSS.
///
/// The order matters:
///
/// 1. Initialize the TSS's static fields via [`tss::init`]. The
///    TSS's `ss0` and `iomap_base` must be correct before the
///    first trap from CPL 3, which cannot happen before the GDT
///    is loaded and `ltr` has run.
///
/// 2. Build the TSS descriptor and write it into the GDT. The
///    GDT is a `static`, so this requires an `unsafe` write.
///    The write must happen before `lgdt`.
///
/// 3. Load the GDT with `lgdt`.
///
/// 4. Reload CS with a far return, so the cached CS descriptor
///    matches the new GDT.
///
/// 5. Reload the data segment registers.
///
/// 6. Load the TSS with `ltr`. Until this step, the CPU has no
///    valid task register and would fault on a privilege
///    transition. After it, `esp0` and `ss0` are reachable.
pub fn init() {
    // 1. Initialize the TSS's static fields.
    tss::init();

    // 2. Write the TSS descriptor into the GDT.
    //
    // The GDT is a static and is normally immutable; the write
    // is safe because `init` runs exactly once, before any other
    // code can read the GDT, and because a concurrent read of a
    // descriptor mid-write would be a boot-order bug rather than
    // a data race (the kernel is single-threaded during boot).
    //
    // The cast to `*mut u64` is what allows the write. Reading
    // the descriptor back from an immutable reference would see
    // the same value.
    let gdt_ptr = core::ptr::addr_of!(GDT) as *mut u64;
    unsafe {
        core::ptr::write_volatile(gdt_ptr.add(5), build_tss_descriptor());
    }

    // 3. Load the GDT.
    let pointer = GdtPointer {
        limit: (core::mem::size_of::<[u64; GDT_ENTRIES]>() - 1) as u16,
        base: core::ptr::addr_of!(GDT) as u32,
    };

    unsafe {
        asm!(
            "lgdt ({pointer})",
            pointer = in(reg) &pointer,
            options(readonly, nostack, preserves_flags, att_syntax),
        );

        // 4. Reload CS with the kernel code selector.
        asm!(
            "pushl $0x08",
            "pushl $2f",
            "lret",
            "2:",
            options(nostack, att_syntax),
        );

        // 5. Reload the data segment registers.
        asm!(
            "movw $0x10, %ax",
            "movw %ax, %ds",
            "movw %ax, %es",
            "movw %ax, %fs",
            "movw %ax, %gs",
            "movw %ax, %ss",
            options(nostack, preserves_flags, att_syntax),
        );

        // 6. Load the TSS.
        asm!(
            "ltr ax",
            in("ax") TSS_SELECTOR,
            options(nostack, preserves_flags),
        );
    }

    println!(
        "GDT loaded: {} entries, ring-0 code 0x{:02x}, ring-0 data 0x{:02x}, \
         ring-3 code 0x{:02x}, ring-3 data 0x{:02x}, TSS 0x{:02x}",
        GDT_ENTRIES,
        KERNEL_CODE_SELECTOR,
        KERNEL_DATA_SELECTOR,
        USER_CODE_SELECTOR,
        USER_DATA_SELECTOR,
        TSS_SELECTOR,
    );
    println!("TSS loaded: address 0x{:08x}", tss::address());
}
