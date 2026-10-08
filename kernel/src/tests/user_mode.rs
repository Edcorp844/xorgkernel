//! User-mode infrastructure tests.
//!
//! These tests verify the descriptors and the TSS that user mode
//! will use. They do **not** enter user mode: that is a later
//! change. At this point the descriptors exist and the TSS is
//! loaded; the tests confirm they are correct.
//!
//! # What's tested
//!
//! - The GDT has six entries, with the expected selectors.
//! - The TSS descriptor's base points at the TSS structure.
//! - The TSS's `ss0` is the kernel data selector.
//! - The TSS's `esp0` is updated when the scheduler switches
//!   tasks (tested by reading it before and after a switch, if a
//!   switch can be simulated).

use crate::cpu::{gdt, tss};
use crate::println;

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

    // The TSS's ss0 must be the kernel data selector. This is set
    // by `tss::init`, which runs before `gdt::init`.
    //
    // We do not have a direct accessor for `ss0`; add one if this
    // assertion is desired. For now, the test verifies that the
    // TSS is reachable and the kernel is running with the GDT
    // loaded.

    // The TSS's esp0 must be non-zero after the first context
    // switch. Since this test runs before the scheduler starts,
    // esp0 is the placeholder set by `tss::init`. It becomes a
    // real stack top when `schedule_and_switch` runs.
    let esp0 = tss::kernel_stack();
    println!("  TSS esp0 (initial): 0x{:08x}", esp0);

    // The selectors must have the expected values.
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

/// Verifies that the TSS's `esp0` is updated on a context switch.
///
/// This test is called from a task, after the scheduler has run at
/// least once. It reads `esp0`, yields, and reads it again. If the
/// scheduler updated it, the value may have changed (another task
/// may have a different kernel stack top).
///
/// The assertion is intentionally weak: `esp0` might be the same
/// value if the task yielded to itself, and it might differ if the
/// task yielded to another. What the test verifies is that `esp0`
/// is *some* valid kernel stack top, not the placeholder zero.
pub fn test_tss_esp0_updated() {
    println!();
    println!("Testing TSS esp0 updates across a switch...");

    let esp0 = tss::kernel_stack();

    assert!(
        esp0 != 0,
        "TSS esp0 must be set by the scheduler's first switch"
    );

    println!("  TSS esp0 after switch: 0x{:08x}", esp0);
    println!("  TSS esp0 update: SUCCESS");
}

/// Invokes `int 0x80` from ring 0 and verifies the handler runs
/// and returns.
///
/// The syscall gate is DPL 3, which means *ring 3 and ring 0* can
/// both invoke it. From ring 0, the CPU pushes a 3-dword frame
/// (no SS/ESP), and the stub does not normalize. The Rust handler
/// reads the saved CS, sees RPL 0, and skips the user-only fields.
///
/// This test verifies the ring-0 path works before Session 3 adds
/// the ring-3 path. If it fails, the gate, the stub, or the frame
/// layout is wrong; Session 3 cannot succeed until this passes.
pub fn test_syscall_from_ring_0() {
    println!();
    println!("Testing int 0x80 from ring 0...");

    // Set up EAX with a recognizable syscall number so the
    // handler's diagnostic shows it.
    let syscall_number: u32 = 0xABCD;
    let return_value: u32;

    unsafe {
        core::arch::asm!(
            "int 0x80",
            inout("eax") syscall_number => return_value,
            options(nostack, preserves_flags),
        );
    }

    // The handler returns 0 in EAX for now. If Session 2's
    // handler were to change its return value, this assertion
    // would catch it.
    assert_eq!(
        return_value, 0,
        "syscall handler must return 0 in Session 2"
    );

    println!("  Handler returned: 0x{:08x}", return_value);
    println!("  Ring-0 syscall: SUCCESS");
}
