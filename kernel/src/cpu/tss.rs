//! Task State Segment.
//!
//! The TSS is the x86 structure that tells the CPU where to find
//! the kernel stack when a privilege transition occurs. When user
//! code at CPL 3 traps into the kernel (through an interrupt, an
//! exception, or a syscall gate), the CPU switches to CPL 0 and,
//! unless the trap gate specifies a stack, loads `SS:ESP` from
//! `TSS.SS0:TSS.ESP0`.
//!
//! Without a TSS, the first trap from CPL 3 triple-faults: the
//! CPU has nowhere to push the trap frame, and no way to find
//! out where to push it.
//!
//! # What the kernel uses
//!
//! Of the 32-bit TSS's many fields, the kernel uses exactly two:
//!
//! - `ss0` — the ring-0 stack segment selector, always `0x10`
//!   (the kernel data selector).
//! - `esp0` — the ring-0 stack pointer. This is updated on every
//!   context switch to point at the top of the incoming task's
//!   kernel stack.
//!
//! The rest of the fields (the segment registers to load on a
//! privilege transition, the debug trap flag, the I/O permission
//! bitmap offset) are not used by a flat protected-mode kernel.
//! They are zero.
//!
//! # Why `esp0` must be updated per task
//!
//! Every task has its own kernel stack. If two tasks share a TSS
//! with a stale `esp0`, the second task's first trap from CPL 3
//! would push its frame onto the first task's kernel stack, which
//! is either in use (corruption) or freed (triple fault).
//!
//! `esp0` is therefore updated by `schedule_and_switch` before
//! every switch to a task, to point at that task's kernel stack
//! top.
//!
//! # I/O permission bitmap
//!
//! `iomap_base` is set to the size of the TSS (104 bytes), which
//! is past the end of the structure. The CPU treats an
//! out-of-range `iomap_base` as "no I/O bitmap," which means
//! every port-access instruction from CPL 3 raises #GP. This is
//! the desired default: user mode has no port access, and any
//! port I/O a driver performs goes through a syscall that the
//! kernel's port-I/O code executes at CPL 0.

/// Size of the 32-bit TSS, in bytes.
///
/// Fixed by the architecture. The GDT descriptor's limit field is
/// `TSS_SIZE - 1`.
pub const TSS_SIZE: usize = 104;

/// The 32-bit Task State Segment.
///
/// The field order and offsets are fixed by the architecture. The
/// CPU reads them by offset, so this layout must not be changed.
///
/// Fields not used by the kernel are present so that the offsets
/// of the fields that follow are correct. Removing them would
/// shift `esp0` and break the CPU's reads.
#[repr(C, packed)]
pub struct Tss {
    /// Reserved, unused.
    pub prev_tss: u32,

    /// Ring-0 stack pointer.
    ///
    /// Loaded into ESP when the CPU transitions from CPL 3 to
    /// CPL 0 on an interrupt, exception, or trap gate.
    pub esp0: u32,

    /// Ring-0 stack segment selector.
    ///
    /// Loaded into SS when the CPU transitions from CPL 3 to
    /// CPL 0. Always `0x10` (the kernel data selector).
    pub ss0: u32,

    /// Ring-1 stack pointer. Unused.
    pub esp1: u32,

    /// Ring-1 stack segment selector. Unused.
    pub ss1: u32,

    /// Ring-2 stack pointer. Unused.
    pub esp2: u32,

    /// Ring-2 stack segment selector. Unused.
    pub ss2: u32,

    /// CR3 value to load on a hardware task switch.
    ///
    /// The kernel does not use hardware task switching, so this
    /// field is zero.
    pub cr3: u32,

    /// Registers to load on a hardware task switch. Unused.
    pub eip: u32,
    pub eflags: u32,
    pub eax: u32,
    pub ecx: u32,
    pub edx: u32,
    pub ebx: u32,
    pub esp: u32,
    pub ebp: u32,
    pub esi: u32,
    pub edi: u32,
    pub es: u32,
    pub cs: u32,
    pub ss: u32,
    pub ds: u32,
    pub fs: u32,
    pub gs: u32,
    pub ldt_selector: u32,

    /// Reserved, unused.
    pub trap: u16,

    /// Offset of the I/O permission bitmap.
    ///
    /// Set to [`TSS_SIZE`], which is past the end of the TSS.
    /// The CPU interprets this as "no I/O bitmap," and every
    /// port-access instruction from CPL 3 raises #GP.
    pub iomap_base: u16,
}

/// The kernel's TSS.
///
/// There is exactly one TSS on the current kernel, shared by
/// every task. `esp0` is updated on every context switch to point
/// at the incoming task's kernel stack top.
///
/// The TSS lives in `.bss` as a plain Rust static. The GDT
/// descriptor points at `TSS.as_ptr()`, and `ltr` loads the
/// selector. Once loaded, the CPU reads the TSS by physical
/// address whenever it needs `esp0`; the address must remain
/// stable for the kernel's lifetime, which a static guarantees.
static mut TSS: Tss = Tss {
    prev_tss: 0,
    esp0: 0,
    ss0: 0,
    esp1: 0,
    ss1: 0,
    esp2: 0,
    ss2: 0,
    cr3: 0,
    eip: 0,
    eflags: 0,
    eax: 0,
    ecx: 0,
    edx: 0,
    ebx: 0,
    esp: 0,
    ebp: 0,
    esi: 0,
    edi: 0,
    es: 0,
    cs: 0,
    ss: 0,
    ds: 0,
    fs: 0,
    gs: 0,
    ldt_selector: 0,
    trap: 0,
    iomap_base: TSS_SIZE as u16,
};

/// The kernel data selector.
///
/// Loaded into `SS` on a CPL 3 → CPL 0 transition. Must match the
/// selector the GDT assigns to the kernel data segment.
const KERNEL_DATA_SELECTOR: u16 = 0x10;

/// Initializes the TSS.
///
/// Sets `ss0` to the kernel data selector and `esp0` to a
/// placeholder. `esp0` is overwritten by
/// [`set_kernel_stack`] on every context switch; the placeholder
/// exists so that if a trap arrives before the first switch
/// (which cannot happen in the current boot sequence, but is a
/// safe invariant to maintain), the CPU has *some* stack to use.
///
/// Must be called before `cpu::gdt::init`, because the GDT
/// descriptor for the TSS points at this structure.
pub fn init() {
    unsafe {
        let tss = &mut *core::ptr::addr_of_mut!(TSS);

        tss.ss0 = KERNEL_DATA_SELECTOR as u32;
        tss.esp0 = 0;

        // The I/O bitmap offset is already set to TSS_SIZE by the
        // static initializer. Re-asserting it here documents the
        // intent and guards against a future edit that changes
        // the initializer.
        tss.iomap_base = TSS_SIZE as u16;
    }
}

/// Returns the physical address of the TSS.
///
/// The GDT descriptor for the TSS is built from this address. The
/// address is stable for the kernel's lifetime.
///
/// The function is named `address` rather than `physical_address`
/// because in the current kernel, the kernel image is identity-
/// mapped at boot and the address is the same in both the
/// virtual and physical spaces. When user mode arrives and the
/// kernel is mapped differently, this function will need to
/// return the *physical* address, because the CPU reads the TSS
/// through the physical address, not the virtual one. For now,
/// the two are the same.
pub fn address() -> u32 {
    core::ptr::addr_of!(TSS) as u32
}

/// Updates `esp0` to point at the top of a kernel stack.
///
/// Called by `schedule_and_switch` before switching to a task, so
/// that a trap from CPL 3 will use that task's kernel stack. The
/// argument is the *top* of the kernel stack (the highest valid
/// address, where the CPU expects ESP to point on entry).
///
/// The stack pointer passed in must be 4-byte aligned. The kernel
/// stack is allocated by `Task::create` and is at least 256 bytes,
/// so the top is 4-byte aligned by construction.
pub fn set_kernel_stack(esp0: u32) {
    unsafe {
        let tss = &mut *core::ptr::addr_of_mut!(TSS);
        tss.esp0 = esp0;
    }
}

/// Returns the current value of `esp0`.
///
/// Used by tests and diagnostics.
pub fn kernel_stack() -> u32 {
    unsafe { (*core::ptr::addr_of!(TSS)).esp0 }
}
