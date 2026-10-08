//! Syscall dispatch.
//!
//! The `int 0x80` gate is the kernel's entry point from CPL 3.
//! When user code executes `int 0x80`, the CPU:
//!
//! 1. Reads the IDT entry at vector 0x80. It is a DPL-3 interrupt
//!    gate, so the trap is allowed from user mode.
//! 2. Clears IF (interrupt gates do this on entry).
//! 3. Loads CS with the kernel code selector (`0x08`).
//! 4. Loads `SS:ESP` from the TSS's `ss0`/`esp0`, which points at
//!    the running task's kernel stack top.
//! 5. Pushes `SS, ESP, EFLAGS, CS, EIP` onto the new kernel stack.
//! 6. Jumps to `syscall_entry` in `syscall.S`.
//!
//! # Frame shape
//!
//! The frame the CPU pushes depends on the privilege level the trap
//! came from:
//!
//! ```text
//!   From CPL 0:  [EFLAGS][CS][EIP]
//!   From CPL 3:  [SS][ESP][EFLAGS][CS][EIP]
//! ```
//!
//! The assembly stub does not normalize; it passes the raw stack
//! pointer to [`syscall_dispatch`], which reads the saved CS to
//! determine which shape it is looking at.
//!
//! # Session 2 scope
//!
//! The current handler does not implement any syscalls. It prints a
//! diagnostic and returns zero. The purpose of this session is to
//! get the gate, the stub, and the frame layout correct; syscalls
//! will be added in a later session.

use core::arch::asm;

/// Layout of the register block and CPU-pushed frame after
/// `pushal` in `syscall_entry`.
///
/// The struct is `#[repr(C)]` because the assembly code writes into
/// it by fixed offsets. The `esp` and `ss` fields are only valid
/// when the trap came from CPL 3; from CPL 0, they hold whatever
/// the previous stack contents happened to be.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SyscallFrame {
    // ---- Registers saved by `pushal`. ----
    pub edi: u32,
    pub esi: u32,
    pub ebp: u32,
    pub original_esp: u32,
    pub ebx: u32,
    pub edx: u32,
    pub ecx: u32,

    /// The saved EAX.
    ///
    /// On entry, this is the caller's EAX. On return, the handler
    /// writes its return value here, so `popal` in the stub
    /// restores it into EAX.
    pub eax: u32,

    // ---- CPU-pushed frame. ----
    pub eip: u32,
    pub cs: u32,
    pub eflags: u32,

    /// The user stack pointer at the time of the trap.
    ///
    /// Valid only when [`Self::from_user_mode`] is true.
    pub user_esp: u32,

    /// The user stack segment selector at the time of the trap.
    ///
    /// Valid only when [`Self::from_user_mode`] is true.
    pub user_ss: u32,
}

impl SyscallFrame {
    /// Returns whether the trap came from CPL 3.
    ///
    /// The saved CS's low two bits are the RPL. If they are 3, the
    /// trap came from user mode; if 0, from kernel mode.
    pub fn from_user_mode(&self) -> bool {
        (self.cs & 0x03) == 3
    }

    /// Returns the syscall number.
    ///
    /// The convention: EAX holds the syscall number on entry.
    /// The kernel uses this in later sessions; for Session 2, it
    /// is not interpreted.
    pub fn syscall_number(&self) -> u32 {
        self.eax
    }

    /// Sets the return value.
    ///
    /// The stub's `popal` restores EAX from this field, so writing
    /// here is how the handler returns a value to the caller.
    pub fn set_return_value(&mut self, value: u32) {
        self.eax = value;
    }
}

/// The syscall dispatcher.
///
/// Called from `syscall_entry` in `syscall.S` with a pointer to
/// the frame. The pointer is `*mut` because the handler writes the
/// return value into the frame's `eax` field.
///
/// # Safety
///
/// Called from assembly. `frame` must point at a `SyscallFrame`
/// that the stub has laid out on the kernel stack. The stub
/// guarantees this; the `SyscallFrame` layout matches the stub's
/// `pushal` block exactly.
///
/// # Session 2 behavior
///
/// Prints a diagnostic and returns zero. No syscalls are
/// implemented yet.
#[unsafe(no_mangle)]
pub extern "C" fn syscall_dispatch(frame: *mut SyscallFrame) {
    unsafe {
        core::arch::asm!(
            "push eax",
            "mov al, 'E'",
            "out 0xE9, al",
            "pop eax",
            options(nostack, preserves_flags),
        );
    }

    // SAFETY: the stub guarantees `frame` points at a valid
    // `SyscallFrame` on the current kernel stack.
    let frame = unsafe { &mut *frame };

    let from_user = frame.from_user_mode();

    if from_user {
        // The first CPL-3 trap is the observable signal that user
        // mode is live. Print it distinctly so it stands out from
        // the per-syscall diagnostic below.
        use core::sync::atomic::{AtomicBool, Ordering};
        static FIRST_USER_TRAP: AtomicBool = AtomicBool::new(true);

        if FIRST_USER_TRAP.swap(false, Ordering::Relaxed) {
            println!();
            println!("========================================");
            println!("       FIRST CPL-3 SYSCALL TRAP");
            println!("========================================");
            println!("User mode is live.");
            println!();
        }
    }

    println!();
    println!("Syscall trap:");
    println!(
        "  Origin:     {}",
        if from_user { "CPL 3" } else { "CPL 0" }
    );
    println!("  Syscall #:  {}", frame.syscall_number());
    println!("  EIP:        0x{:08x}", frame.eip);
    println!("  CS:         0x{:08x}", frame.cs);
    println!("  EFLAGS:     0x{:08x}", frame.eflags);

    if from_user {
        println!("  User ESP:   0x{:08x}", frame.user_esp);
        println!("  User SS:    0x{:08x}", frame.user_ss);
    } else {
        println!("  User ESP/SS: (not applicable; trap from ring 0)");
    }
    println!("  EAX:        0x{:08x}", frame.eax);
    println!("  EBX:        0x{:08x}", frame.ebx);
    println!("  ECX:        0x{:08x}", frame.ecx);
    println!("  EDX:        0x{:08x}", frame.edx);
    println!("  ESI:        0x{:08x}", frame.esi);
    println!("  EDI:        0x{:08x}", frame.edi);
    println!("  EBP:        0x{:08x}", frame.ebp);

    println!(
        "  TSS.esp0      = 0x{:08x}",
        crate::cpu::tss::kernel_stack()
    );
    println!("  frame addr    = 0x{:08x}", frame as *const _ as u32);
    println!("  frame.eflags  = 0x{:08x}", frame.eflags);

    // Return zero for now. Later sessions will dispatch on the
    // syscall number and return the result of the operation.
    frame.set_return_value(0);

    unsafe {
        core::arch::asm!(
            "mov al, 'D'",
            "out 0xE9, al",
            options(nostack, preserves_flags),
        );
    }
}
