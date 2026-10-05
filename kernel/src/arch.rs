//! Access to linker-provided memory region symbols.
//!
//! Every statically-placed region in the kernel is defined in
//! `linker.ld`. This module is the single place that reads those
//! symbols, so no other module needs to know magic addresses.

/// Returns the start address of a linker-defined symbol.
///
/// # Safety
///
/// The symbol must be declared `extern "C"` and defined by the
/// linker script. All symbols used here are.

macro_rules! linker_symbol {
    ($name:ident) => {
        mod $name {
            unsafe extern "C" {
                #[link_name = stringify!($name)]
                pub static SYMBOL: u8;
            }
        }

        #[allow(dead_code)]
        #[inline(always)]
        pub fn $name() -> u32 {
            core::ptr::addr_of!($name::SYMBOL) as u32
        }
    };
}

linker_symbol!(__kernel_start);
linker_symbol!(__kernel_end);
linker_symbol!(__bootstrap_start);
linker_symbol!(__page_directory_start);
linker_symbol!(__page_directory_end);
linker_symbol!(__page_table_start);
linker_symbol!(__page_table_end);
linker_symbol!(__idt_start);
linker_symbol!(__idt_end);
linker_symbol!(__gdt_start);
linker_symbol!(__gdt_end);
linker_symbol!(__frame_bitmap_start);
linker_symbol!(__frame_bitmap_end);
linker_symbol!(__bootstrap_stack_bottom);
linker_symbol!(__bootstrap_stack_top);
linker_symbol!(__bss_start);
linker_symbol!(__bss_end);
