//! Console dispatcher.
//!
//! A `Console` owns a set of output sinks. Every write is fanned
//! out to each registered sink. Sinks are added incrementally:
//! the serial and VGA text sinks are registered at boot, before
//! any other kernel code runs. The framebuffer sink is registered
//! later, once the Multiboot2 information structure has been
//! parsed and the framebuffer parameters are known.
//!
//! Each sink is optional. If a sink is not present, its writes are
//! skipped without error. This allows the same kernel to boot in
//! environments that provide different console hardware.

use core::fmt::{self, Write};

use crate::serial::SerialPort;
use crate::sync::SpinLock;
use crate::vga::VgaWriter;

pub mod font;
pub mod framebuffer;

use framebuffer::Framebuffer;

/// A console fans text output out to multiple sinks.
pub struct Console {
    serial: SerialPort,
    vga: Option<VgaWriter>,
    framebuffer: Option<Framebuffer>,
}

impl Console {
    pub const fn new() -> Self {
        Self {
            serial: SerialPort::new(),
            vga: None,
            framebuffer: None,
        }
    }

    /// Initializes the always-available sinks.
    ///
    /// Serial is initialized unconditionally; VGA text is
    /// registered as well. Both are cheap and never fail.
    ///
    /// Must be called before any other kernel code writes to the
    /// console.
    pub fn init_early(&mut self) {
        self.serial.init();
        self.vga = Some(VgaWriter::new());
    }

    /// Registers a framebuffer sink.
    ///
    /// Called once, after the Multiboot2 information has been
    /// parsed and a framebuffer tag has been found.
    pub fn init_framebuffer(&mut self, fb: Framebuffer) {
        self.framebuffer = Some(fb);
    }
}

impl fmt::Write for Console {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.serial.write_str(s)?;
        if let Some(vga) = self.vga.as_mut() {
            vga.write_str(s)?;
        }
        if let Some(fb) = self.framebuffer.as_mut() {
            fb.write_str(s)?;
        }
        Ok(())
    }
}

/// The kernel's single console, behind a spinlock.
static CONSOLE: SpinLock<Console> = SpinLock::new(Console::new());

/// Initializes the console's early sinks (serial and VGA text).
pub fn init() {
    CONSOLE.lock().init_early();
}

/// Registers a framebuffer sink.
///
/// Called after Multiboot2 parsing, if a framebuffer was provided.
pub fn init_framebuffer(fb: Framebuffer) {
    CONSOLE.lock().init_framebuffer(fb);
}

/// Writes a formatted string to the console.
pub fn _print(args: fmt::Arguments<'_>) {
    let mut console = CONSOLE.lock();
    let _ = console.write_fmt(args);
}