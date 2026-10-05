use core::arch::asm;

/// CR0 paging bit.
const CR0_PG: u32 = 1 << 31;

/// CR4 Page Size Extension bit.
///
/// When enabled, a page-directory entry with the PS bit set maps a 4 MiB
/// page instead of pointing to a page table.
const CR4_PSE: u32 = 1 << 4;

/// Reads the processor's CR0 register.
pub fn read_cr0() -> u32 {
    let value: u32;

    unsafe {
        asm!(
            "mov {0:e}, cr0",
            out(reg) value,
            options(nomem, nostack, preserves_flags),
        );
    }

    value
}

/// Writes the processor's CR0 register.
pub unsafe fn write_cr0(value: u32) {
    unsafe {
        asm!(
        "mov cr0, {0:e}",
        in(reg) value,
        options(nomem, nostack, preserves_flags),
        );
    }
}

/// Reads the processor's CR2 register.
///
/// CR2 contains the linear address that caused the most recent page fault.
pub fn read_cr2() -> u32 {
    let value: u32;

    unsafe {
        asm!(
            "mov {0:e}, cr2",
            out(reg) value,
            options(nomem, nostack, preserves_flags),
        );
    }

    value
}

/// Reads the processor's CR3 register.
pub fn read_cr3() -> u32 {
    let value: u32;

    unsafe {
        asm!(
            "mov {0:e}, cr3",
            out(reg) value,
            options(nomem, nostack, preserves_flags),
        );
    }

    value
}

/// Writes the processor's CR3 register.
pub unsafe fn write_cr3(value: u32) {
    unsafe {
        asm!(
        "mov cr3, {0:e}",
        in(reg) value,
        options(nomem, nostack, preserves_flags),
        );
    }
}

/// Reads the processor's CR4 register.
pub fn read_cr4() -> u32 {
    let value: u32;

    unsafe {
        asm!(
            "mov {0:e}, cr4",
            out(reg) value,
            options(nomem, nostack, preserves_flags),
        );
    }

    value
}

/// Writes the processor's CR4 register.
pub unsafe fn write_cr4(value: u32) {
    unsafe {
        asm!(
        "mov cr4, {0:e}",
        in(reg) value,
        options(nomem, nostack, preserves_flags),
        );
    }
}

/// Enables 4 MiB x86 pages.
///
/// This sets CR4.PSE. The page-directory entries themselves must still have
/// their PS bit set before the processor will use 4 MiB mappings.
pub fn enable_pse() {
    let cr4 = read_cr4();

    unsafe {
        write_cr4(cr4 | CR4_PSE);
    }
}

/// Enables paging in CR0.
pub fn enable_paging() {
    let cr0 = read_cr0();

    unsafe {
        write_cr0(cr0 | CR0_PG);
    }
}
