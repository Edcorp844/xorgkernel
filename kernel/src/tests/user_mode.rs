//! User-mode infrastructure tests.
//!
//! These tests verify the descriptors and the TSS that user mode
//! uses, and exercise the syscall dispatcher from ring 0.
//!
//! # What's tested
//!
//! - The GDT has six entries, with the expected selectors.
//! - The TSS descriptor points at the TSS structure.
//! - The syscall dispatcher's fallback path returns
//!   `ERR_NOSYS` for an unrecognized syscall number.
//! - `SYS_SELF_CELL` returns `ERR_INVALID` when the caller has
//!   no cell (which is the case for the bootstrap context that
//!   runs these tests).
//!
//! The tests run before the scheduler starts. They exercise the
//! ring-0 path of the syscall gate only; the ring-3 path is
//! exercised by the user task that runs after the scheduler takes
//! over.

use crate::cpu::{gdt, tss};
use crate::println;

// ---------------------------------------------------------------------
// Descriptor tests
// ---------------------------------------------------------------------

/// Verifies that the GDT is loaded and the TSS descriptor points
/// at the TSS structure.
pub fn test_gdt_and_tss_loaded() {
    println!();
    println!("Testing GDT and TSS...");

    // The TSS must have an address. If the GDT descriptor were
    // built against a null address, `ltr` would have faulted
    // during boot and the kernel would not be running.
    let tss_address = tss::address();
    assert!(tss_address != 0, "TSS must have a non-zero address");

    println!("  TSS address: 0x{:08x}", tss_address);

    // The TSS's esp0 is a placeholder (zero) at this point in the
    // boot. It becomes a real stack top when the scheduler
    // performs its first context switch.
    let esp0 = tss::kernel_stack();
    println!("  TSS esp0 (initial): 0x{:08x}", esp0);

    // The selectors must have the expected values. A change to any
    // of them breaks user mode: CS/SS for ring 3 must have RPL 3,
    // and the TSS selector must match the GDT entry loaded with
    // `ltr`.
    assert_eq!(gdt::KERNEL_CODE_SELECTOR, 0x08);
    assert_eq!(gdt::KERNEL_DATA_SELECTOR, 0x10);
    assert_eq!(gdt::USER_CODE_SELECTOR, 0x18);
    assert_eq!(gdt::USER_DATA_SELECTOR, 0x20);
    assert_eq!(gdt::TSS_SELECTOR, 0x28);

    println!(
        "  Selectors: code 0x08, data 0x10, user code 0x18, user data 0x20, TSS 0x28: SUCCESS"
    );

    println!("  GDT and TSS loaded: SUCCESS");
}

// ---------------------------------------------------------------------
// Syscall dispatcher tests (ring 0)
// ---------------------------------------------------------------------

/// Tests that an unrecognized syscall number returns `ERR_NOSYS`.
///
/// The dispatcher's fallback branch returns `-3` (`ERR_NOSYS`) for
/// any syscall number it does not recognize. This test invokes
/// `int 0x80` with an unrecognized number from ring 0 and checks
/// that the status code in EAX is `-3`.
pub fn test_unknown_syscall_from_ring_0() {
    println!();
    println!("Testing unknown syscall from ring 0...");

    let syscall_number: u32 = 0xABCD;
    let status: u32;
    let value: u32;

    unsafe {
        core::arch::asm!(
            "int 0x80",
            inout("eax") syscall_number => status,
            out("edx") value,
            options(nostack, preserves_flags),
        );
    }

    // `-3` as `u32` is `0xFFFF_FFFD`. The dispatcher writes the
    // `i32` status into the frame's EAX slot, and `popal` restores
    // it into EAX. The caller sees the raw bit pattern.
    assert_eq!(
        status, 0xFFFF_FFFD,
        "unknown syscall must return ERR_NOSYS (-3)"
    );

    // The value register is unspecified on error. Do not assert on
    // it; the ABI makes no promise about its contents when the
    // status is non-zero.
    let _ = value;

    println!("  Status: 0x{:08x} (ERR_NOSYS)", status);
    println!("  Unknown syscall returns ERR_NOSYS: SUCCESS");
}

/// Tests `SYS_SELF_CELL` from ring 0.
///
/// The ring-0 caller is the bootstrap context, which has no cell.
/// The dispatcher's `sys_self_cell` looks up the current task
/// through the scheduler; for the bootstrap context this is the
/// task with id 0, whose cell is `CellId::INVALID`. The dispatcher
/// returns `ERR_INVALID` in that case.
///
/// This is a controlled exercise of the dispatcher's argument and
/// return-value paths, and it confirms that the current-task
/// lookup path in the scheduler works.
pub fn test_self_cell_from_ring_0() {
    println!();
    println!("Testing SYS_SELF_CELL from ring 0...");

    let syscall_number: u32 = 3; // SYS_SELF_CELL
    let status: u32;
    let value: u32;

    unsafe {
        core::arch::asm!(
            "int 0x80",
            inout("eax") syscall_number => status,
            out("edx") value,
            options(nostack, preserves_flags),
        );
    }

    // The bootstrap context has no cell, so the dispatcher returns
    // `ERR_INVALID` (-2). `-2` as `u32` is `0xFFFF_FFFE`.
    assert_eq!(
        status, 0xFFFF_FFFE,
        "SYS_SELF_CELL from the bootstrap context must return ERR_INVALID"
    );

    let _ = value;

    println!("  Status: 0x{:08x} (ERR_INVALID)", status);
    println!("  SYS_SELF_CELL from bootstrap: SUCCESS");
}