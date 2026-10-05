use crate::arch;
use core::arch::asm;

const IDT_ENTRIES: usize = 256;

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct IdtEntry {
    offset_low: u16,
    selector: u16,
    zero: u8,
    flags: u8,
    offset_high: u16,
}

impl IdtEntry {
    const MISSING: Self = Self {
        offset_low: 0,
        selector: 0,
        zero: 0,
        flags: 0,
        offset_high: 0,
    };

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
}

#[repr(C, packed)]
struct IdtPointer {
    limit: u16,
    base: u32,
}

fn idt_base() -> *mut IdtEntry {
    arch::__idt_start() as *mut IdtEntry
}

pub fn address() -> u32 {
    arch::__idt_start()
}

unsafe fn set_entry(vector: usize, entry: IdtEntry) {
    unsafe {
        core::ptr::write_volatile(idt_base().add(vector), entry);
    }
}

pub fn init() {
    unsafe {
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

    println!("Rust IDT loaded.");
}

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

pub fn debug_entry(vector: usize) -> (u16, u16, u8, u8, u16) {
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
