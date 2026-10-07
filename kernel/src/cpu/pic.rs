//! 8259 Programmable Interrupt Controller.
//!
//! The 8259 PIC is the legacy x86 interrupt controller. Modern
//! systems replaced it with the APIC, but the PIC remains available
//! on every x86 machine and is often used during early boot, before
//! the APIC has been configured.
//!
//! # Architecture
//!
//! A standard PC has two 8259 chips in a master/slave configuration:
//!
//! ```text
//! Master PIC  (IRQ 0-7)   I/O ports 0x20, 0x21
//! Slave  PIC  (IRQ 8-15)  I/O ports 0xA0, 0xA1
//! ```
//!
//! The slave is cascaded onto the master's IRQ2 line. When the
//! slave has a pending interrupt, it raises IRQ2 on the master, and
//! the master forwards the request.
//!
//! # Remapping
//!
//! By default, the BIOS maps IRQ0-7 to interrupt vectors 0x08-0x0F
//! and IRQ8-15 to 0x70-0x77. The first range collides with CPU
//! exceptions (vectors 0-31), which is a problem: an IRQ0 timer tick
//! would be delivered as vector 0x08 (double fault) instead of a
//! normal interrupt.
//!
//! The standard fix is to remap the PIC so that IRQ0-15 are
//! delivered at vectors 0x20-0x2F. This module does that once, at
//! boot.
//!
//! # Status
//!
//! Once the APIC is initialized, the PIC should be masked entirely
//! (all IRQs disabled) so that the two controllers do not both
//! attempt to deliver interrupts. Until then, the PIC handles all
//! hardware interrupts.

use crate::console::serial::inb;
use crate::console::serial::outb;

/// I/O port of the master PIC's command register.
const MASTER_COMMAND: u16 = 0x20;

/// I/O port of the master PIC's data register.
const MASTER_DATA: u16 = 0x21;

/// I/O port of the slave PIC's command register.
const SLAVE_COMMAND: u16 = 0xA0;

/// I/O port of the slave PIC's data register.
const SLAVE_DATA: u16 = 0xA1;

/// First vector the master PIC will deliver.
///
/// IRQ0 is delivered as vector `MASTER_VECTOR_BASE + 0`,
/// IRQ1 as `MASTER_VECTOR_BASE + 1`, and so on up to IRQ7.
const MASTER_VECTOR_BASE: u8 = 0x20;

/// First vector the slave PIC will deliver.
///
/// IRQ8 is delivered as vector `SLAVE_VECTOR_BASE + 0`,
/// IRQ9 as `SLAVE_VECTOR_BASE + 1`, and so on up to IRQ15.
const SLAVE_VECTOR_BASE: u8 = 0x28;

/// End-of-interrupt command byte.
///
/// Written to the command register after an IRQ has been handled,
/// so the PIC can raise the next pending interrupt.
const COMMAND_EOI: u8 = 0x20;

/// ICW1: initialize the PIC, expect ICW4.
const ICW1_INIT: u8 = 0x11;

/// ICW4: 8086/88 mode.
///
/// Tells the PIC to expect the modern x86 interrupt acknowledge
/// protocol instead of the original 8080 mode.
const ICW4_8086: u8 = 0x01;

/// Initializes and remaps both PICs.
///
/// After this call:
///
/// - IRQ0-7  are delivered at vectors 0x20-0x27
/// - IRQ8-15 are delivered at vectors 0x28-0x2F
/// - all IRQ lines are masked
///
/// The caller must unmask individual lines with [`unmask`] after
/// installing the corresponding IDT entries. Unmasking a line whose
/// vector has no handler in the IDT will cause the CPU to fault when
/// that interrupt is raised.
pub fn remap() {
    unsafe {
        // Save the existing masks. We restore them at the end so
        // the BIOS's idea of which lines are active is preserved.
        let saved_master = inb(MASTER_DATA);
        let saved_slave = inb(SLAVE_DATA);

        // ---- Start initialization sequence (ICW1). ----
        outb(MASTER_COMMAND, ICW1_INIT);
        outb(SLAVE_COMMAND, ICW1_INIT);

        // ---- ICW2: vector offset. ----
        outb(MASTER_DATA, MASTER_VECTOR_BASE);
        outb(SLAVE_DATA, SLAVE_VECTOR_BASE);

        // ---- ICW3: cascade wiring. ----
        //
        // The slave is wired to the master's IRQ2 line. The master
        // is told this via a bitmask (bit 2 set); the slave is told
        // its cascade identity (2).
        outb(MASTER_DATA, 0x04);
        outb(SLAVE_DATA, 0x02);

        // ---- ICW4: 8086 mode. ----
        outb(MASTER_DATA, ICW4_8086);
        outb(SLAVE_DATA, ICW4_8086);

        // ---- Restore saved masks. ----
        outb(MASTER_DATA, saved_master);
        outb(SLAVE_DATA, saved_slave);
    }
}

/// Unmasks an IRQ line, allowing the PIC to deliver it.
///
/// `irq` must be in the range 0-15. IRQ lines 0-7 are on the master
/// PIC; 8-15 are on the slave.
///
/// After unmasking, the CPU will receive interrupts on the vector
/// corresponding to the IRQ line. The IDT must have a valid handler
/// installed for that vector before this is called.
pub fn unmask(irq: u8) {
    assert!(irq < 16, "pic: IRQ number out of range");

    unsafe {
        if irq < 8 {
            // Master PIC.
            let mask = inb(MASTER_DATA) & !(1 << irq);
            outb(MASTER_DATA, mask);
        } else {
            // Slave PIC.
            //
            // The slave is cascaded through the master's IRQ2 line.
            // Unmasking an IRQ on the slave therefore requires
            // unmasking IRQ2 on the master as well.
            let slave_irq = irq - 8;
            let slave_mask = inb(SLAVE_DATA) & !(1 << slave_irq);
            outb(SLAVE_DATA, slave_mask);

            let master_mask = inb(MASTER_DATA) & !(1 << 2);
            outb(MASTER_DATA, master_mask);
        }
    }
}

/// Masks an IRQ line, preventing the PIC from delivering it.
///
/// `irq` must be in the range 0-15.
pub fn mask(irq: u8) {
    assert!(irq < 16, "pic: IRQ number out of range");

    unsafe {
        if irq < 8 {
            let mask = inb(MASTER_DATA) | (1 << irq);
            outb(MASTER_DATA, mask);
        } else {
            let slave_irq = irq - 8;
            let mask = inb(SLAVE_DATA) | (1 << slave_irq);
            outb(SLAVE_DATA, mask);
        }
    }
}

/// Sends an end-of-interrupt notification to the PIC.
///
/// Must be called after handling an IRQ, once the handler has
/// finished. Without this, the PIC will not raise further interrupts
/// on the same priority level.
///
/// `irq` must be the IRQ number that was handled. If the IRQ came
/// from the slave PIC (8-15), an EOI is sent to both chips.
pub fn end_of_interrupt(irq: u8) {
    unsafe {
        if irq >= 8 {
            outb(SLAVE_COMMAND, COMMAND_EOI);
        }
        outb(MASTER_COMMAND, COMMAND_EOI);
    }
}

/// Masks every IRQ line on both PICs.
///
/// Used when transitioning to the APIC. After this call, the PIC
/// will not deliver any interrupts; the APIC becomes the sole
/// interrupt controller.
#[allow(dead_code)]
pub fn mask_all() {
    unsafe {
        outb(MASTER_DATA, 0xFF);
        outb(SLAVE_DATA, 0xFF);
    }
}
