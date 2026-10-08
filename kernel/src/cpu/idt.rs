//! Interrupt Descriptor Table.
//!
//! The IDT tells the CPU where to find the handler for each of the
//! 256 interrupt and exception vectors. It is loaded once at boot
//! with `lidt` and remains in place for the life of the kernel.
//!
//! # Layout
//!
//! The table itself is a linker-placed region (see `linker.ld`,
//! symbol `__idt_start`) of 256 entries, each 8 bytes. The region is
//! `NOLOAD`, so it is not part of the disk image; the kernel writes
//! the entries during `init`.
//!
//! # Vector assignments
//!
//! ```text
//!    0 -  31    CPU exceptions
//!   32 -  47    hardware IRQs (after PIC remap)
//!   48 - 127    unused (reserved)
//!  128 (0x80)   syscall gate
//!  129 - 255    unused (reserved)
//! ```
//!
//! CPU exceptions are handled by the stubs in `exceptions.S`.
//! Hardware IRQs are handled by the stubs in `irq.S`. The syscall
//! gate is handled by the stub in `syscall.S`. Each stub is
//! responsible for saving whatever state its handler needs; see
//! the individual `.S` files for the frame layouts they produce.
//!
//! # Gate format
//!
//! Each entry is an 8-byte structure:
//!
//! ```text
//!   +0   offset bits  0-15
//!   +2   code segment selector
//!   +4   reserved (zero)
//!   +5   flags
//!   +6   offset bits 16-31
//! ```
//!
//! The flags byte is interpreted as:
//!
//! ```text
//!   bit 7    P    present
//!   bits 6-5 DPL  descriptor privilege level
//!   bit 4    S    0 for system descriptors
//!   bits 3-0 type 0xE = 32-bit interrupt gate, 0xF = 32-bit trap gate
//! ```
//!
//! The kernel uses two flags values:
//!
//! - `0x8E` — present, DPL 0, 32-bit interrupt gate. Used for
//!   every CPU exception and every hardware IRQ.
//! - `0xEE` — present, DPL 3, 32-bit interrupt gate. Used for the
//!   syscall gate at vector 0x80.
//!
//! Interrupt gates clear IF on entry, so a handler runs with
//! interrupts disabled. This is correct for exceptions and IRQs
//! (the CPU is already in a privileged context and the handler must
//! not be preempted by another interrupt of the same class), and it
//! is correct for the syscall gate (a syscall handler must not be
//! preempted mid-execution by a timer tick that could observe an
//! inconsistent frame).
//!
//! The DPL controls *who may invoke* the gate; the code segment
//! selector controls *what privilege level the handler runs at*.
//! The syscall gate is DPL 3 so that CPL-3 code can invoke it, but
//! its selector is `0x08` (kernel code), so the handler runs at
//! CPL 0. That combination — a DPL-3 gate into a ring-0 handler —
//! is the standard shape for a syscall entry.

use crate::arch;
use core::arch::asm;

/// Number of entries in the IDT.
///
/// The x86 architecture defines exactly 256 vectors, so this is
/// fixed.
const IDT_ENTRIES: usize = 256;

/// First vector used for hardware IRQs after the PIC remap.
const IRQ_VECTOR_BASE: usize = 32;

/// Number of hardware IRQ vectors.
///
/// The 8259 PIC provides 16 IRQ lines (0-15), corresponding to
/// vectors 32-47.
const IRQ_VECTOR_COUNT: usize = 16;

/// The vector used for the syscall gate.
///
/// 0x80 is the traditional x86 Linux syscall vector. It is above
/// the IRQ range (after the PIC remap, IRQs are at 0x20-0x2F) and
/// does not collide with any CPU exception.
pub const SYSCALL_VECTOR: usize = 0x80;

/// A single IDT entry.
///
/// The layout matches the format the CPU expects when it performs an
/// interrupt vector lookup. The structure is `packed` because the
/// fields are not naturally aligned: the offset is split across
/// bytes 0-1 and 6-7 with unrelated fields in between.
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct IdtEntry {
    /// Low 16 bits of the handler's code-segment offset.
    offset_low: u16,

    /// Code segment selector loaded into CS before the handler runs.
    selector: u16,

    /// Reserved. Must be zero.
    zero: u8,

    /// Gate flags. See the module documentation for the bit layout.
    flags: u8,

    /// High 16 bits of the handler's code-segment offset.
    offset_high: u16,
}

impl IdtEntry {
    /// An all-zero entry.
    ///
    /// An entry with the present bit clear causes the CPU to raise
    /// #GP if the corresponding vector is ever delivered. This is
    /// the correct initial state for a vector with no handler.
    #[allow(dead_code)]
    const MISSING: Self = Self {
        offset_low: 0,
        selector: 0,
        zero: 0,
        flags: 0,
        offset_high: 0,
    };

    /// Builds an entry for a ring-0, 32-bit interrupt gate.
    ///
    /// The handler address is split across the offset fields, and
    /// the gate is marked present with DPL 0. This is the shape
    /// used for CPU exceptions and hardware IRQs.
    fn new(handler: unsafe extern "C" fn()) -> Self {
        let address = handler as usize as u32;

        Self {
            offset_low: address as u16,
            selector: 0x08,
            zero: 0,
            flags: 0x8E,
            offset_high: (address >> 16) as u16,
        }
    }

    /// Builds an entry for a ring-3, 32-bit interrupt gate.
    ///
    /// The flags byte is `0xEE`: present, DPL 3, 32-bit interrupt
    /// gate. DPL 3 is what allows `int 0x80` from CPL 3 to be
    /// delivered without a #GP; a DPL-0 gate would reject the
    /// instruction.
    ///
    /// The selector is still `0x08` (kernel code): the handler runs
    /// at CPL 0 even though the gate is DPL 3. The DPL controls
    /// *who may call* the gate; the selector controls *what
    /// privilege level the handler runs at*.
    fn new_user_callable(handler: unsafe extern "C" fn()) -> Self {
        let address = handler as usize as u32;

        Self {
            offset_low: address as u16,
            selector: 0x08,
            zero: 0,
            flags: 0xEE,
            offset_high: (address >> 16) as u16,
        }
    }
}

/// The operand of the `lidt` instruction.
///
/// `lidt` reads 6 bytes from memory: a 2-byte limit followed by a
/// 4-byte base. The structure is `packed` because the fields are
/// not naturally aligned relative to each other.
#[repr(C, packed)]
struct IdtPointer {
    /// Size of the IDT in bytes, minus one.
    limit: u16,

    /// Linear address of the IDT.
    base: u32,
}

/// Returns a mutable pointer to the start of the IDT.
///
/// The IDT is a linker-placed region, not a Rust static, so it is
/// accessed through `arch::__idt_start`.
fn idt_base() -> *mut IdtEntry {
    arch::__idt_start() as *mut IdtEntry
}

/// Returns the linear address of the IDT.
///
/// This is the value loaded into the IDTR's base field by `lidt`.
pub fn address() -> u32 {
    arch::__idt_start()
}

/// Writes one entry into the IDT.
///
/// The write is volatile: the IDT is read directly by the CPU on
/// every interrupt, and the compiler must not elide or reorder the
/// store.
///
/// # Safety
///
/// `vector` must be less than `IDT_ENTRIES`.
unsafe fn set_entry(vector: usize, entry: IdtEntry) {
    unsafe {
        core::ptr::write_volatile(idt_base().add(vector), entry);
    }
}

/// Initializes the IDT and loads it with `lidt`.
///
/// Installs handlers for:
///
/// - CPU exceptions 0-31
/// - hardware IRQs 32-47
/// - the syscall gate at vector 0x80
///
/// All other vectors are left as `MISSING`. Delivering one of them
/// raises #GP, which the kernel treats as a fatal exception.
///
/// Must be called exactly once, after the GDT is loaded and before
/// any interrupt source is enabled.
pub fn init() {
    unsafe {
        // ---- CPU exceptions 0-31. ----
        //
        // The stubs in exceptions.S normalize the interrupt frame
        // and call into Rust for dispatch.
        set_entry(0, IdtEntry::new(exception_entry_0));
        set_entry(1, IdtEntry::new(exception_entry_1));
        set_entry(2, IdtEntry::new(exception_entry_2));
        set_entry(3, IdtEntry::new(exception_entry_3));
        set_entry(4, IdtEntry::new(exception_entry_4));
        set_entry(5, IdtEntry::new(exception_entry_5));
        set_entry(6, IdtEntry::new(exception_entry_6));
        set_entry(7, IdtEntry::new(exception_entry_7));
        set_entry(8, IdtEntry::new(exception_entry_8));
        set_entry(9, IdtEntry::new(exception_entry_9));
        set_entry(10, IdtEntry::new(exception_entry_10));
        set_entry(11, IdtEntry::new(exception_entry_11));
        set_entry(12, IdtEntry::new(exception_entry_12));
        set_entry(13, IdtEntry::new(exception_entry_13));
        set_entry(14, IdtEntry::new(exception_entry_14));
        set_entry(15, IdtEntry::new(exception_entry_15));
        set_entry(16, IdtEntry::new(exception_entry_16));
        set_entry(17, IdtEntry::new(exception_entry_17));
        set_entry(18, IdtEntry::new(exception_entry_18));
        set_entry(19, IdtEntry::new(exception_entry_19));
        set_entry(20, IdtEntry::new(exception_entry_20));
        set_entry(21, IdtEntry::new(exception_entry_21));
        set_entry(22, IdtEntry::new(exception_entry_22));
        set_entry(23, IdtEntry::new(exception_entry_23));
        set_entry(24, IdtEntry::new(exception_entry_24));
        set_entry(25, IdtEntry::new(exception_entry_25));
        set_entry(26, IdtEntry::new(exception_entry_26));
        set_entry(27, IdtEntry::new(exception_entry_27));
        set_entry(28, IdtEntry::new(exception_entry_28));
        set_entry(29, IdtEntry::new(exception_entry_29));
        set_entry(30, IdtEntry::new(exception_entry_30));
        set_entry(31, IdtEntry::new(exception_entry_31));

        // ---- Hardware IRQs 32-47. ----
        //
        // These vectors are only meaningful once the PIC has been
        // remapped (see `cpu::pic::remap`). Before remapping, the
        // PIC delivers IRQs at vectors 0x08-0x0F and 0x70-0x77,
        // which would collide with CPU exceptions. The remap must
        // therefore happen before any IRQ line is unmasked.
        //
        // The stubs in irq.S handle the end-of-interrupt protocol
        // and return via `iret`.
        set_entry(IRQ_VECTOR_BASE + 0, IdtEntry::new(irq_entry_32));
        set_entry(IRQ_VECTOR_BASE + 1, IdtEntry::new(irq_entry_33));
        set_entry(IRQ_VECTOR_BASE + 2, IdtEntry::new(irq_entry_34));
        set_entry(IRQ_VECTOR_BASE + 3, IdtEntry::new(irq_entry_35));
        set_entry(IRQ_VECTOR_BASE + 4, IdtEntry::new(irq_entry_36));
        set_entry(IRQ_VECTOR_BASE + 5, IdtEntry::new(irq_entry_37));
        set_entry(IRQ_VECTOR_BASE + 6, IdtEntry::new(irq_entry_38));
        set_entry(IRQ_VECTOR_BASE + 7, IdtEntry::new(irq_entry_39));
        set_entry(IRQ_VECTOR_BASE + 8, IdtEntry::new(irq_entry_40));
        set_entry(IRQ_VECTOR_BASE + 9, IdtEntry::new(irq_entry_41));
        set_entry(IRQ_VECTOR_BASE + 10, IdtEntry::new(irq_entry_42));
        set_entry(IRQ_VECTOR_BASE + 11, IdtEntry::new(irq_entry_43));
        set_entry(IRQ_VECTOR_BASE + 12, IdtEntry::new(irq_entry_44));
        set_entry(IRQ_VECTOR_BASE + 13, IdtEntry::new(irq_entry_45));
        set_entry(IRQ_VECTOR_BASE + 14, IdtEntry::new(irq_entry_46));
        set_entry(IRQ_VECTOR_BASE + 15, IdtEntry::new(irq_entry_47));

        // ---- Syscall gate. ----
        //
        // Vector 0x80, DPL 3, ring-0 code. User code can invoke
        // this gate with `int 0x80`; the handler runs at CPL 0.
        //
        // The gate is installed here but the syscall handler is
        // not yet implemented. Session 2 tests the ring-0 path;
        // Session 3 will add the ring-3 `iret` path and the first
        // real syscall.
        set_entry(SYSCALL_VECTOR, IdtEntry::new_user_callable(syscall_entry));

        // ---- Load the IDT. ----

        let pointer = IdtPointer {
            limit: (IDT_ENTRIES * 8 - 1) as u16,
            base: address(),
        };

        asm!(
            "lidt [{0}]",
            in(reg) &pointer,
            options(readonly, nostack, preserves_flags),
        );
    }

    println!(
        "IDT loaded: {} entries, {} exceptions, {} IRQs, syscall gate at 0x{:02x}",
        IDT_ENTRIES, 32, IRQ_VECTOR_COUNT, SYSCALL_VECTOR,
    );
}

/// Returns the raw fields of one IDT entry, for diagnostics.
///
/// Returns `(offset_low, selector, zero, flags, offset_high)`.
/// The handler address can be reconstructed as
/// `(offset_high as u32) << 16 | offset_low as u32`.
#[allow(dead_code)]
pub fn debug_entry(vector: usize) -> (u16, u16, u8, u8, u16) {
    assert!(vector < IDT_ENTRIES, "idt: vector out of range");

    unsafe {
        let entry = core::ptr::read_volatile(idt_base().add(vector));
        (
            entry.offset_low,
            entry.selector,
            entry.zero,
            entry.flags,
            entry.offset_high,
        )
    }
}

// ---------------------------------------------------------------------
// External handlers
// ---------------------------------------------------------------------

/// CPU exception stubs, defined in `exceptions.S`.
///
/// One symbol per vector, `exception_entry_N` for vector N.
unsafe extern "C" {
    fn exception_entry_0();
    fn exception_entry_1();
    fn exception_entry_2();
    fn exception_entry_3();
    fn exception_entry_4();
    fn exception_entry_5();
    fn exception_entry_6();
    fn exception_entry_7();
    fn exception_entry_8();
    fn exception_entry_9();
    fn exception_entry_10();
    fn exception_entry_11();
    fn exception_entry_12();
    fn exception_entry_13();
    fn exception_entry_14();
    fn exception_entry_15();
    fn exception_entry_16();
    fn exception_entry_17();
    fn exception_entry_18();
    fn exception_entry_19();
    fn exception_entry_20();
    fn exception_entry_21();
    fn exception_entry_22();
    fn exception_entry_23();
    fn exception_entry_24();
    fn exception_entry_25();
    fn exception_entry_26();
    fn exception_entry_27();
    fn exception_entry_28();
    fn exception_entry_29();
    fn exception_entry_30();
    fn exception_entry_31();
}

/// Hardware IRQ stubs, defined in `irq.S`.
///
/// One symbol per IRQ, `irq_entry_N` for vector N. The number in
/// the symbol name is the *vector* (32-47), not the IRQ line
/// (0-15). The mapping from IRQ line to vector is
/// `vector = 32 + irq`.
unsafe extern "C" {
    fn irq_entry_32();
    fn irq_entry_33();
    fn irq_entry_34();
    fn irq_entry_35();
    fn irq_entry_36();
    fn irq_entry_37();
    fn irq_entry_38();
    fn irq_entry_39();
    fn irq_entry_40();
    fn irq_entry_41();
    fn irq_entry_42();
    fn irq_entry_43();
    fn irq_entry_44();
    fn irq_entry_45();
    fn irq_entry_46();
    fn irq_entry_47();
}

/// Syscall stub, defined in `syscall.S`.
///
/// The stub is the low-level entry point for `int 0x80`. It saves
/// the general-purpose registers, passes the current stack pointer
/// to `cpu::syscall::syscall_dispatch`, and returns via `iret`. See
/// `syscall.S` for the frame layout.
unsafe extern "C" {
    fn syscall_entry();
}
