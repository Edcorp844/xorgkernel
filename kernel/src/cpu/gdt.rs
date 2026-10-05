use crate::arch;
use core::arch::asm;

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct GdtPointer {
    limit: u16,
    base: u32,
}

const GDT_ENTRIES: usize = 3;

fn gdt_base() -> *mut u64 {
    arch::__gdt_start() as *mut u64
}

pub fn init() {
    unsafe {
        // Write the three descriptors.
        core::ptr::write_volatile(gdt_base().add(0), 0x0000000000000000);
        core::ptr::write_volatile(gdt_base().add(1), 0x00CF9A000000FFFF);
        core::ptr::write_volatile(gdt_base().add(2), 0x00CF92000000FFFF);

        let pointer = GdtPointer {
            limit: (GDT_ENTRIES * 8 - 1) as u16,
            base: arch::__gdt_start(),
        };

        asm!(
            "lgdt [{0}]",
            in(reg) &pointer,
            options(readonly, nostack, preserves_flags),
        );

        mov_data_segments();
    }
}

unsafe fn mov_data_segments() {
    unsafe {
        asm!(
            "mov ax, 0x10",
            "mov ds, ax",
            "mov es, ax",
            "mov ss, ax",
            "mov fs, ax",
            "mov gs, ax",
            options(nostack, preserves_flags),
        );
    }
}