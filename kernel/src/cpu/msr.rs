//! Model-Specific Register (MSR) access.
//!
//! MSRs are 64-bit registers addressed by a 32-bit MSR number,
//! accessed via the `rdmsr` and `wrmsr` instructions. They control
//! CPU features that are not exposed through the ordinary control
//! registers, including:
//!
//! - the APIC base address and enable bit
//! - the SYSCALL/SYSRET entry points (for later use)
//! - thermal and performance counters
//! - machine-check configuration
//!
//! Both instructions require CPL 0. Reading or writing an MSR that
//! does not exist raises #GP.

use core::arch::asm;

/// Reads a 64-bit MSR.
///
/// Returns the low 32 bits in `eax` and the high 32 bits in `edx`,
/// recombined into a single `u64`.
///
/// # Safety
///
/// The caller must ensure that `msr` is a valid MSR number on the
/// current CPU. Reading an invalid MSR raises a general protection
/// fault, which the current kernel treats as fatal.
pub unsafe fn read(msr: u32) -> u64 {
    let low: u32;
    let high: u32;

    unsafe {
        asm!(
            "rdmsr",
            in("ecx") msr,
            out("eax") low,
            out("edx") high,
            options(nomem, nostack, preserves_flags),
        );
    }

    ((high as u64) << 32) | (low as u64)
}

/// Writes a 64-bit MSR.
///
/// # Safety
///
/// The caller must ensure that:
///
/// - `msr` is a valid MSR number on the current CPU
/// - `value` is a legal value for that MSR
///
/// Writing an invalid MSR or an illegal value raises #GP.
pub unsafe fn write(msr: u32, value: u64) {
    let low = value as u32;
    let high = (value >> 32) as u32;

    unsafe {
        asm!(
            "wrmsr",
            in("ecx") msr,
            in("eax") low,
            in("edx") high,
            options(nomem, nostack, preserves_flags),
        );
    }
}