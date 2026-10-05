//! Hardware interrupt dispatch.
//!
//! This module sits between the assembly IRQ stubs and the kernel
//! code that actually handles device interrupts. Its job is:
//!
//! 1. Receive the IRQ number from the assembly stub.
//! 2. Look up a registered handler for that IRQ.
//! 3. Invoke the handler.
//! 4. Send an end-of-interrupt to the PIC (or, later, the APIC).
//!
//! # Registration
//!
//! Handlers are registered with [`register`]. Only one handler per
//! IRQ line is supported; registering a second handler replaces the
//! first. When the APIC gains support for shared interrupts, this
//! restriction can be lifted.
//!
//! # The stub contract
//!
//! The assembly stubs in `irq.S` push the IRQ number onto the stack
//! and call [`dispatch`]. The stub is responsible for saving and
//! restoring all registers that must survive an interrupt, including
//! those the kernel does not save as part of a normal function call.
//! See `irq.S` for the exact layout.

use crate::cpu::pic;

/// Maximum number of IRQ lines we support.
///
/// The 8259 PIC provides 16 lines; the I/O APIC provides up to 24.
/// 16 is sufficient for the current configuration.
const MAX_IRQS: usize = 16;

/// A handler for a single IRQ line.
///
/// The handler receives no arguments and is expected to complete
/// quickly: it runs with interrupts disabled on the current CPU.
/// Long work should be deferred to a bottom-half mechanism, which
/// the kernel does not yet have.
type IrqHandler = extern "C" fn();

/// Registered handlers, one slot per IRQ line.
///
/// A slot of `None` means the IRQ has no handler. If the PIC is
/// unmasked for that line and an interrupt arrives anyway, dispatch
/// logs a warning and sends an EOI (so the PIC does not get stuck),
/// but does not invoke anything.
static mut HANDLERS: [Option<IrqHandler>; MAX_IRQS] = [None; MAX_IRQS];

/// Registers a handler for an IRQ line.
///
/// `irq` must be in the range 0-15. Registering a handler for an IRQ
/// that already has one replaces the previous handler without
/// warning; use [`unregister`] first if you need to detect this.
///
/// Registering a handler does not unmask the IRQ. The caller must
/// call [`pic::unmask`] after ensuring the IDT has an entry for the
/// corresponding vector.
pub fn register(irq: u8, handler: IrqHandler) {
    assert!((irq as usize) < MAX_IRQS, "interrupts: IRQ out of range");

    unsafe {
        HANDLERS[irq as usize] = Some(handler);
    }
}

/// Unregisters the handler for an IRQ line.
///
/// After this call, the IRQ has no handler. It remains masked if it
/// was already masked; the caller is responsible for masking it if
/// needed.
#[allow(dead_code)]
pub fn unregister(irq: u8) {
    assert!((irq as usize) < MAX_IRQS, "interrupts: IRQ out of range");

    unsafe {
        HANDLERS[irq as usize] = None;
    }
}

/// Dispatches a hardware interrupt.
///
/// Called by the assembly stub in `irq.S` with the IRQ number.
/// Invokes the registered handler (if any), then sends an
/// end-of-interrupt to the PIC.
///
/// # Safety
///
/// Called from assembly. `irq` must be a valid IRQ number in the
/// range 0-15.
#[unsafe(no_mangle)]
pub extern "C" fn irq_dispatch(irq: u32) {
    let irq = irq as u8;

    if (irq as usize) >= MAX_IRQS {
        println!("interrupts: received out-of-range IRQ {}", irq);
        return;
    }

    // Fetch the handler. Doing this through a raw read of the
    // static avoids holding a lock across the handler call, which
    // would be a problem if the handler needs to register another
    // handler (it usually doesn't, but the discipline is worth
    // keeping).
    let handler = unsafe { HANDLERS[irq as usize] };

    match handler {
        Some(h) => h(),
        None => {
            println!("interrupts: unhandled IRQ {}", irq);
        }
    }

    // Notify the PIC that the interrupt has been handled. Without
    // this, the PIC will not deliver further interrupts on the
    // same priority level.
    pic::end_of_interrupt(irq);
}