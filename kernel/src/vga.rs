use core::fmt;

const VGA_ADDRESS: usize = 0xb8000;

const VGA_WIDTH: usize = 80;
const VGA_HEIGHT: usize = 25;

const DEFAULT_COLOR: u8 = 0x0f;

pub struct VgaWriter {
    column: usize,
    row: usize,
    color: u8,
}

impl VgaWriter {
    pub const fn new() -> Self {
        Self {
            column: 0,
            row: 0,
            color: DEFAULT_COLOR,
        }
    }

    pub fn clear(&mut self) {
        for row in 0..VGA_HEIGHT {
            for column in 0..VGA_WIDTH {
                self.write_cell(row, column, b' ', self.color);
            }
        }

        self.column = 0;
        self.row = 0;
    }

    fn write_cell(
        &self,
        row: usize,
        column: usize,
        character: u8,
        color: u8,
    ) {
        let offset = (row * VGA_WIDTH + column) * 2;

        unsafe {
            core::ptr::write_volatile(
                (VGA_ADDRESS + offset) as *mut u8,
                character,
            );

            core::ptr::write_volatile(
                (VGA_ADDRESS + offset + 1) as *mut u8,
                color,
            );
        }
    }

    fn new_line(&mut self) {
        self.column = 0;

        if self.row + 1 < VGA_HEIGHT {
            self.row += 1;
        } else {
            self.scroll();
        }
    }

    fn scroll(&mut self) {
        for row in 1..VGA_HEIGHT {
            for column in 0..VGA_WIDTH {
                let src_offset =
                    (row * VGA_WIDTH + column) * 2;

                let dst_offset =
                    ((row - 1) * VGA_WIDTH + column) * 2;

                unsafe {
                    let character = core::ptr::read_volatile(
                        (VGA_ADDRESS + src_offset) as *const u8
                    );

                    let color = core::ptr::read_volatile(
                        (VGA_ADDRESS + src_offset + 1) as *const u8
                    );

                    core::ptr::write_volatile(
                        (VGA_ADDRESS + dst_offset) as *mut u8,
                        character,
                    );

                    core::ptr::write_volatile(
                        (VGA_ADDRESS + dst_offset + 1) as *mut u8,
                        color,
                    );
                }
            }
        }

        for column in 0..VGA_WIDTH {
            self.write_cell(
                VGA_HEIGHT - 1,
                column,
                b' ',
                self.color,
            );
        }

        self.row = VGA_HEIGHT - 1;
    }

    fn write_byte(&mut self, byte: u8) {
        match byte {
            b'\n' => self.new_line(),

            b'\r' => {
                self.column = 0;
            }

            byte => {
                self.write_cell(
                    self.row,
                    self.column,
                    byte,
                    self.color,
                );

                self.column += 1;

                if self.column >= VGA_WIDTH {
                    self.new_line();
                }
            }
        }
    }
}

impl fmt::Write for VgaWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for byte in s.bytes() {
            self.write_byte(byte);
        }

        Ok(())
    }
}