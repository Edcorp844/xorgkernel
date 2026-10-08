//! Syscall dispatch.
//!
//! The `int 0x80` gate is the kernel's entry point from CPL 3. When
//! user code executes `int 0x80`, the CPU:
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
//! The assembly stub saves the general-purpose registers with
//! `pushal`, passes a pointer to the resulting [`SyscallFrame`] to
//! [`syscall_dispatch`], and restores the registers with `popal`
//! before returning via `iret`.
//!
//! # Frame shape
//!
//! The CPU pushes a different frame depending on the privilege
//! level the trap came from:
//!
//! ```text
//!   From CPL 0:  [EFLAGS][CS][EIP]
//!   From CPL 3:  [SS][ESP][EFLAGS][CS][EIP]
//! ```
//!
//! The stub does not normalize; the dispatcher reads the saved CS
//! to determine which shape it is looking at.
//!
//! # Return value
//!
//! The dispatcher writes the syscall's status code into the saved
//! EAX slot and its return value into the saved EDX slot. The
//! stub's `popal` restores those registers, so the user task sees
//! the status in EAX and the value in EDX after the `iret`.

use core::arch::asm;

use crate::capability::core::CapabilityCore;
use crate::cpu::syscall_abi::{
    ERR_FAULT, ERR_INVALID, ERR_NOSYS, SYS_DEBUG_WRITE, SYS_DEBUG_WRITE_MAX, SYS_SELF_CELL,
    SYS_YIELD, SyscallResult,
};

/// Layout of the register block and CPU-pushed frame after
/// `pushal` in `syscall_entry`.
///
/// The struct is `#[repr(C)]` because the assembly code writes into
/// it by fixed offsets. The `user_esp` and `user_ss` fields are
/// only valid when the trap came from CPL 3; from CPL 0, they hold
/// whatever the previous stack contents happened to be.
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
    /// On entry, this holds the syscall number. On return, the
    /// dispatcher writes the status code here.
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
    pub fn from_user_mode(&self) -> bool {
        (self.cs & 0x03) == 3
    }

    /// Returns the syscall number.
    ///
    /// EAX holds the syscall number on entry.
    pub fn syscall_number(&self) -> u32 {
        self.eax
    }

    /// Writes the result of a syscall back into the frame.
    ///
    /// The status code goes into the saved EAX slot, and the value
    /// goes into the saved EDX slot. The stub's `popal` restores
    /// both.
    pub fn set_result(&mut self, result: SyscallResult) {
        self.eax = result.status as u32;
        self.edx = result.value;
    }
}

/// The syscall dispatcher.
///
/// Called from `syscall_entry` in `syscall.S` with a pointer to the
/// frame. The pointer is `*mut` because the dispatcher writes the
/// return value into the frame.
///
/// # Safety
///
/// Called from assembly. `frame` must point at a `SyscallFrame`
/// that the stub has laid out on the current kernel stack. The
/// stub guarantees this; the `SyscallFrame` layout matches the
/// stub's `pushal` block exactly.
#[unsafe(no_mangle)]
pub extern "C" fn syscall_dispatch(frame: *mut SyscallFrame) {
    // SAFETY: the stub guarantees `frame` points at a valid
    // `SyscallFrame` on the current kernel stack.
    let frame = unsafe { &mut *frame };

    let number = frame.syscall_number();
    let arg0 = frame.ebx;
    let arg1 = frame.ecx;

    let result = match number {
        SYS_DEBUG_WRITE => sys_debug_write(frame, arg0, arg1),
        SYS_YIELD => sys_yield(),
        SYS_SELF_CELL => sys_self_cell(frame),
        _ => SyscallResult::err(ERR_NOSYS),
    };

    frame.set_result(result);
}

// ---------------------------------------------------------------------
// The syscalls
// ---------------------------------------------------------------------

/// Writes bytes from a user buffer to the console.
///
/// The buffer is validated against the caller's address space
/// before the kernel dereferences it. Only after every byte in the
/// range has been confirmed mapped and user-accessible does the
/// kernel copy the data into a kernel stack buffer and hand it to
/// the console.
///
/// # Arguments
///
/// - `buffer` (EBX): virtual address of the buffer.
/// - `length` (ECX): length in bytes, at most
///   [`SYS_DEBUG_WRITE_MAX`].
///
/// # Returns
///
/// `Ok(bytes_written)` on success. `Err(ERR_INVALID)` if the length
/// is zero or exceeds the maximum. `Err(ERR_FAULT)` if the buffer
/// is not fully mapped and user-accessible.
fn sys_debug_write(frame: &SyscallFrame, buffer: u32, length: u32) -> SyscallResult {
    if length == 0 || length > SYS_DEBUG_WRITE_MAX {
        return SyscallResult::err(ERR_INVALID);
    }

    // Validate the buffer against the caller's address space.
    //
    // The range [buffer, buffer + length) may span multiple pages.
    // Every byte in the range must be mapped, and every mapping
    // must be user-accessible. We check page by page rather than
    // byte by byte because the page granularity is what the MMU
    // enforces, and a byte that lies in a mapped page is
    // guaranteed to be readable.
    if !validate_user_range(frame, buffer, length) {
        return SyscallResult::err(ERR_FAULT);
    }

    // Copy the bytes into a kernel stack buffer. The user buffer
    // is not touched again after this point, so a concurrent
    // modification by user code cannot affect what is printed.
    let mut scratch = [0u8; SYS_DEBUG_WRITE_MAX as usize];
    let src = buffer as *const u8;

    for i in 0..(length as usize) {
        // SAFETY: `validate_user_range` has confirmed that every
        // byte in [buffer, buffer + length) is mapped and
        // user-accessible. The CPU is at CPL 0, so the copy
        // cannot fault, and the user bit on the pages means the
        // access is architecturally permitted.
        scratch[i] = unsafe { core::ptr::read_volatile(src.add(i)) };
    }

    // Write the bytes to the console. The console's `_print`
    // takes a `fmt::Arguments`, so we build a `&str` from the
    // scratch buffer. The bytes are treated as UTF-8; invalid
    // sequences are replaced by the formatter.
    let slice = &scratch[..(length as usize)];
    match core::str::from_utf8(slice) {
        Ok(s) => {
            print!("{}", s);
            SyscallResult::ok(length)
        }
        Err(_) => {
            // The user sent bytes that are not valid UTF-8. Print
            // a replacement so the syscall still does something
            // observable, and report the write as successful for
            // the number of bytes we consumed.
            print!("<{} bytes of non-UTF-8 data>", length);
            SyscallResult::ok(length)
        }
    }
}

/// Yields the CPU to another ready task.
///
/// The task is re-enqueued by `schedule_and_switch` and resumes
/// when the scheduler selects it again.
///
/// # Returns
///
/// Always `Ok(0)`. The call may return immediately if no other task
/// is ready at the same or higher priority.
fn sys_yield() -> SyscallResult {
    crate::sched::yield_task();
    SyscallResult::OK
}

/// Returns the caller's cell ID.
///
/// The cell ID is looked up from the current task through the
/// scheduler. It is a raw `CellId`, not a capability ticket; the
/// caller has no authority over the cell merely by knowing its
/// identifier.
///
/// # Returns
///
/// `Ok(cell_id)` on success. `Err(ERR_INVALID)` if the current task
/// has no cell (which should not happen for a user task).
fn sys_self_cell(frame: &SyscallFrame) -> SyscallResult {
    let _ = frame;
    let current_id = crate::sched::scheduler_mut().current();
    let Some(task) = crate::sched::scheduler_mut().task(current_id) else {
        return SyscallResult::err(ERR_INVALID);
    };
    let cell = task.cell();

    if !cell.is_valid() {
        return SyscallResult::err(ERR_INVALID);
    }

    SyscallResult::ok(cell.raw())
}

// ---------------------------------------------------------------------
// User-buffer validation
// ---------------------------------------------------------------------

/// Returns whether `[buffer, buffer + length)` is fully mapped and
/// user-accessible in the calling task's address space.
///
/// The check is performed page by page. Every page in the range
/// must satisfy:
///
/// 1. Its page-directory entry is present.
/// 2. Its page-directory entry has the user bit set.
/// 3. If the PDE points at a page table, the corresponding PTE is
///    present and has the user bit set.
///
/// If the calling task has no address space (which would be a bug
/// in the task-creation path, not a runtime condition), the
/// function returns `false`.
fn validate_user_range(frame: &SyscallFrame, buffer: u32, length: u32) -> bool {
    if length == 0 {
        return true;
    }

    // Look up the calling task's address space capability.
    let current_id = crate::sched::scheduler_mut().current();
    let Some(task) = crate::sched::scheduler_mut().task(current_id) else {
        return false;
    };
    let as_cap = task.address_space();

    let core: &mut CapabilityCore = crate::capability::core_mut();
    let Some(aspace) = core.address_space(as_cap) else {
        return false;
    };

    // The end address is inclusive of the last byte, exclusive of
    // the next page boundary. Use checked arithmetic so a hostile
    // length cannot wrap the range check.
    let end = match buffer.checked_add(length - 1) {
        Some(e) => e,
        None => return false,
    };

    // Walk page by page from `buffer` to `end`.
    let mut page = buffer & !0xfff;
    loop {
        if aspace.translate_user(page).is_none() {
            return false;
        }

        if page == (end & !0xfff) {
            break;
        }

        page = match page.checked_add(0x1000) {
            Some(p) => p,
            None => return false,
        };
    }

    let _ = frame;
    true
}
