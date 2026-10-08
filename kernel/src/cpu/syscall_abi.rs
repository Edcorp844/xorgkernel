//! The syscall ABI.
//!
//! The kernel exposes a small set of syscalls to CPL 3 code through
//! the `int 0x80` gate. This module defines the wire format: the
//! syscall numbers, the register roles, and the error codes.
//!
//! # Design
//!
//! The ABI is **strictly register-based**. Arguments are passed in
//! registers, never through a user-memory pointer, for three
//! reasons:
//!
//! 1. **TOCTOU resistance.** The CPU captures the register file
//!    atomically when the trap is taken. The kernel cannot be
//!    tricked into re-reading a value that user code changed after
//!    validation, because there is nothing to re-read — the value
//!    is already in a register that only the kernel can modify.
//!
//! 2. **No speculative loads from user memory.** The kernel never
//!    dereferences a user pointer on the syscall fast path, so it
//!    never exposes a speculative-load gadget to user code.
//!
//! 3. **O(1) argument access.** Reading arguments is a register
//!    read, not a memory load. The syscall dispatch does no cache
//!    misses on the fast path.
//!
//! # Register layout (i686)
//!
//! On the current 32-bit target, the register roles are:
//!
//! ```text
//!   EAX    syscall number
//!   EBX    target capability ticket (or arg 0)
//!   ECX    rights mask / command (or arg 1)
//!   EDX    arg 2 / source slot
//!   ESI    arg 3 / dest slot
//!   EDI    arg 4
//! ```
//!
//! On x86_64 the roles map to RAX, RDI, RSI, RDX, R10, R8
//! respectively, matching the layout in the OCAP architecture
//! documentation. The two ABIs are conceptually identical; only
//! the physical register names differ.
//!
//! # Return values
//!
//! On return, the kernel writes:
//!
//! ```text
//!   EAX    status code (0 on success, non-zero on error)
//!   EDX    return value (scalar or capability ticket)
//! ```
//!
//! Two registers, because the return value can be a capability
//! ticket (a `u32`), and mixing the status code and the return
//! value in one register is exactly what the Linux ABI does wrong.
//! Keeping them separate means a syscall that succeeds can still
//! return a value, and a syscall that fails can still be
//! distinguished from a syscall that succeeds and happens to
//! return a large number.
//!
//! # Pointer arguments
//!
//! Syscalls that need to transfer bulk data (for example,
//! `SYS_DEBUG_WRITE`) still take a pointer argument, but the
//! pointer must be validated against the caller's address space
//! before the kernel dereferences it. The validation is:
//!
//! 1. Every byte in `[ptr, ptr + len)` must be mapped.
//! 2. Every mapping must be user-accessible (the `USER` bit set).
//!
//! Only after both checks does the kernel read the buffer. This
//! is the "exception rule" from the OCAP design: pointer payloads
//! are permitted, but only behind explicit validation.

// ---------------------------------------------------------------------
// Syscall numbers
// ---------------------------------------------------------------------

/// Write bytes from a user buffer to the console.
///
/// # Arguments
///
/// - `arg0` (EBX): virtual address of the buffer.
/// - `arg1` (ECX): length in bytes. Must be at most
///   [`SYS_DEBUG_WRITE_MAX`].
///
/// # Return
///
/// - Status: 0 on success, [`ERR_FAULT`] if the buffer is not
///   fully mapped and user-accessible, [`ERR_INVALID`] if the
///   length is zero or too large.
/// - Value: the number of bytes written on success.
pub const SYS_DEBUG_WRITE: u32 = 1;

/// Voluntarily yield the CPU to another ready task.
///
/// # Arguments
///
/// None.
///
/// # Return
///
/// - Status: 0.
/// - Value: 0.
///
/// The syscall returns when the task is scheduled again.
pub const SYS_YIELD: u32 = 2;

/// Return the caller's cell ID.
///
/// # Arguments
///
/// None.
///
/// # Return
///
/// - Status: 0.
/// - Value: the caller's `CellId` as a `u32`. This is the raw
///   value of the cell identifier, not a capability ticket.
pub const SYS_SELF_CELL: u32 = 3;

/// Exit the calling task.
///
/// # Arguments
///
/// - `arg0` (EBX): exit code.
///
/// # Return
///
/// Never. The task is terminated and never scheduled again.
///
/// **Not implemented in this session.** Returns [`ERR_NOSYS`].
pub const SYS_EXIT: u32 = 4;

// ---------------------------------------------------------------------
// Limits
// ---------------------------------------------------------------------

/// Maximum length for `SYS_DEBUG_WRITE`.
///
/// Chosen so the kernel can copy the buffer into a small
/// stack-allocated scratch area, avoiding any heap allocation on
/// the syscall path.
pub const SYS_DEBUG_WRITE_MAX: u32 = 256;

// ---------------------------------------------------------------------
// Error codes
// ---------------------------------------------------------------------

/// The operation succeeded.
pub const ERR_OK: i32 = 0;

/// The caller lacks the rights required for the operation.
///
/// This is the capability model's central error: the caller
/// presented a capability, but the rights on that capability were
/// not sufficient. Distinct from [`ERR_INVALID`], which means the
/// arguments were malformed.
pub const ERR_PERM: i32 = -1;

/// The arguments to the syscall were malformed.
///
/// Examples: a length of zero, an unrecognized rights bit, or a
/// capability ticket that names no live capability.
pub const ERR_INVALID: i32 = -2;

/// The syscall number is not implemented.
pub const ERR_NOSYS: i32 = -3;

/// A user-supplied pointer could not be dereferenced.
///
/// The buffer is not fully mapped, or is mapped without the
/// `USER` bit, or the address range crosses a boundary the
/// kernel refuses to read.
pub const ERR_FAULT: i32 = -4;

// ---------------------------------------------------------------------
// The return-value pair
// ---------------------------------------------------------------------

/// The result of a syscall.
///
/// The status is `i32` so that negative values are the natural
/// encoding of errors. The value is `u32` because the return value
/// is a scalar or a capability ticket — never negative.
#[derive(Clone, Copy, Debug)]
pub struct SyscallResult {
    /// The status code: 0 for success, negative for errors.
    pub status: i32,

    /// The value returned by the syscall. Meaning depends on the
    /// syscall and the status.
    pub value: u32,
}

impl SyscallResult {
    /// The successful result with no value.
    pub const OK: Self = Self {
        status: ERR_OK,
        value: 0,
    };

    /// A successful result with a value.
    pub const fn ok(value: u32) -> Self {
        Self {
            status: ERR_OK,
            value,
        }
    }

    /// A failed result with the given status code.
    pub const fn err(status: i32) -> Self {
        Self { status, value: 0 }
    }

    /// Returns whether the result is a success.
    pub const fn is_ok(&self) -> bool {
        self.status == ERR_OK
    }
}