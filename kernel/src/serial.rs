use core::fmt;

const COM1: u16 = 0x3F8;

pub struct SerialPort;

impl SerialPort {
    pub const fn new() -> Self {
        Self
    }

    /// Initialize COM1 at 38400 baud, 8N1.
    pub fn init(&self) {
        unsafe {
            // Disable interrupts.
            outb(COM1 + 1, 0x00);

            // Enable DLAB.
            outb(COM1 + 3, 0x80);

            // Divisor = 3.
            //
            // 115200 / 3 = 38400 baud.
            outb(COM1, 0x03);
            outb(COM1 + 1, 0x00);

            // 8 data bits, no parity, one stop bit.
            outb(COM1 + 3, 0x03);

            // Enable FIFO.
            // Clear receive/transmit FIFOs.
            // 14-byte threshold.
            outb(COM1 + 2, 0xC7);

            // IRQs enabled.
            // RTS and DTR enabled.
            outb(COM1 + 4, 0x0B);
        }
    }

    pub fn write_byte(&self, byte: u8) {
        unsafe {
            // Wait until the transmitter holding register is empty.
            while (inb(COM1 + 5) & 0x20) == 0 {}

            outb(COM1, byte);
        }
    }
}

impl fmt::Write for SerialPort {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for byte in s.bytes() {
            self.write_byte(byte);
        }

        Ok(())
    }
}

unsafe fn outb(port: u16, value: u8) {
    unsafe {
        core::arch::asm!(
            "out dx, al",
            in("dx") port,
            in("al") value,
            options(nostack, preserves_flags),
        );
    }
}

unsafe fn inb(port: u16) -> u8 {
    let value: u8;

    unsafe {
        core::arch::asm!(
            "in al, dx",
            in("dx") port,
            out("al") value,
            options(nostack, preserves_flags),
        );
    }

    value
}