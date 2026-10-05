use core::fmt::{self, Write};

use crate::serial::SerialPort;
use crate::sync::SpinLock;
use crate::vga::VgaWriter;

pub struct Console {
    serial: SerialPort,
    vga: VgaWriter,
}

impl Console {
    pub const fn new() -> Self {
        Self {
            serial: SerialPort::new(),
            vga: VgaWriter::new(),
        }
    }

    pub fn init(&mut self) {
        self.serial.init();
        self.vga.clear();
    }
}

impl fmt::Write for Console {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.serial.write_str(s)?;
        self.vga.write_str(s)?;

        Ok(())
    }
}

static CONSOLE: SpinLock<Console> =
    SpinLock::new(Console::new());

pub fn init() {
    CONSOLE.lock().init();
}

pub fn _print(args: fmt::Arguments<'_>) {
    let mut console = CONSOLE.lock();

    let _ = console.write_fmt(args);
}