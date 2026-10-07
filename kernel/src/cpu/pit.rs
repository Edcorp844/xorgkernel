//! 8254 Programmable Interval Timer.
//!
//! The PIT is the legacy x86 interval timer. It has three
//! independent channels:
//!
//! ```text
//! Channel 0   system timer, typically wired to IRQ0
//! Channel 1   DRAM refresh (unused on modern systems)
//! Channel 2   PC speaker (unused by the kernel)
//! ```
//!
//! The kernel uses channel 0 as the system tick source. It is
//! programmed once at boot to fire IRQ0 at a fixed frequency. Once
//! the APIC timer is calibrated, the PIT is typically disabled or
//! left running as a fallback.
//!
//! # Frequency
//!
//! The PIT's input clock is 1,193,182 Hz (a third of the original
//! NTSC color burst frequency, a historical accident). The channel
//! divides this by a 16-bit reload value to produce its output:
//!
//! ```text
//! output_frequency = 1193182 / reload_value
//! ```
//!
//! The maximum reload value is 65535, giving a minimum frequency of
//! about 18.2 Hz. The minimum reload value is 1, giving a maximum
//! frequency of about 1.19 MHz.
//!
//! A frequency of 100 Hz is a common choice: frequent enough to keep
//! time reasonably accurately, infrequent enough not to dominate
//! the CPU with interrupt overhead.

use crate::console::serial::outb;

/// I/O port of channel 0's data register.
const CHANNEL_0_DATA: u16 = 0x40;

/// I/O port of the command register.
const COMMAND: u16 = 0x43;

/// PIT input clock frequency, in Hz.
const PIT_FREQUENCY: u32 = 1_193_182;

/// Command byte: channel 0, lobyte/hibyte access, mode 3 (square
/// wave generator), binary counting.
///
/// Mode 3 is the standard periodic mode: the channel toggles between
/// two halves of the period, producing a square wave. It is the
/// usual choice for a timer tick.
const COMMAND_CHANNEL_0_RATE_GENERATOR: u8 = 0x36;

/// Programs the PIT to fire IRQ0 at approximately `frequency_hz`.
///
/// The actual frequency will be the closest achievable value given
/// the PIT's integer divisor. For typical frequencies (50-1000 Hz),
/// the error is much less than 0.1%.
///
/// This function does not unmask IRQ0 on the PIC. The caller must
/// do that separately, after installing an IDT entry for the tick
/// vector.
pub fn init(frequency_hz: u32) {
    assert!(frequency_hz > 0, "pit: frequency must be positive");

    // Compute the divisor. If frequency_hz is very low, the divisor
    // may exceed the 16-bit range; clamp to the maximum and warn.
    let divisor = PIT_FREQUENCY / frequency_hz;

    let divisor = if divisor > 65535 {
        println!(
            "pit: requested frequency {} Hz is below the minimum; \
             using {} Hz",
            1193182 / 65535,
            frequency_hz
        );
        65535
    } else if divisor == 0 {
        println!(
            "pit: requested frequency {} Hz is above the maximum; \
             using {} Hz",
            frequency_hz, PIT_FREQUENCY
        );
        1
    } else {
        divisor
    };

    unsafe {
        // Command byte.
        outb(COMMAND, COMMAND_CHANNEL_0_RATE_GENERATOR);

        // Divisor, low byte first then high byte.
        outb(CHANNEL_0_DATA, (divisor & 0xFF) as u8);
        outb(CHANNEL_0_DATA, ((divisor >> 8) & 0xFF) as u8);
    }

    println!(
        "pit: channel 0 programmed to {} Hz (divisor {})",
        PIT_FREQUENCY / divisor,
        divisor
    );
}
