use crate::arch;
use crate::cpu::control;

const PAGE_SIZE: u32 = 4096;
const ENTRY_COUNT: usize = 1024;

const PRESENT: u32 = 1 << 0;
const WRITABLE: u32 = 1 << 1;
const USER: u32 = 1 << 2;

const ADDRESS_MASK: u32 = 0xFFFF_F000;

#[derive(Clone, Copy)]
pub struct PageFlags(u32);

impl PageFlags {
    pub const PRESENT: Self = Self(PRESENT);
    pub const WRITABLE: Self = Self(WRITABLE);
    pub const USER: Self = Self(USER);
    pub const KERNEL_RW: Self = Self(PRESENT | WRITABLE);
    pub const KERNEL_RO: Self = Self(PRESENT);
    pub const USER_RW: Self = Self(PRESENT | WRITABLE | USER);

    const fn bits(self) -> u32 { self.0 }
}

/// The page directory is a linker-placed 4 KiB region.
fn page_directory() -> *mut u32 {
    arch::__page_directory_start() as *mut u32
}

/// The bootstrap page table is a linker-placed 4 KiB region.
fn first_page_table() -> *mut u32 {
    arch::__page_table_start() as *mut u32
}

pub fn page_directory_address() -> u32 {
    arch::__page_directory_start()
}

pub fn first_page_table_address() -> u32 {
    arch::__page_table_start()
}

unsafe fn pd_get(index: usize) -> u32 {
    unsafe { core::ptr::read_volatile(page_directory().add(index)) }
}

unsafe fn pd_set(index: usize, value: u32) {
    unsafe { core::ptr::write_volatile(page_directory().add(index), value) }
}

unsafe fn pt_get(index: usize) -> u32 {
    unsafe { core::ptr::read_volatile(first_page_table().add(index)) }
}

unsafe fn pt_set(index: usize, value: u32) {
    unsafe { core::ptr::write_volatile(first_page_table().add(index), value) }
}

pub fn init() {
    println!();
    println!("Initializing paging...");

    initialize_identity_mapping();

    println!();
    println!("Paging structures:");
    println!("Page directory: 0x{:08x}", page_directory_address());
    println!("Page table:     0x{:08x}", first_page_table_address());

    println!("Loading CR3...");
    unsafe {
        control::write_cr3(page_directory_address());
    }
    println!("CR3 = 0x{:08x}", control::read_cr3());

    println!("Enabling paging...");
    control::enable_paging();
    println!("CR0 = 0x{:08x}", control::read_cr0());
    println!("Paging enabled.");
}

fn initialize_identity_mapping() {
    println!("  Clearing/building first page table...");

    for index in 0..ENTRY_COUNT {
        let physical_address = (index as u32) * PAGE_SIZE;
        unsafe {
            pt_set(index, physical_address | PageFlags::KERNEL_RW.bits());
        }
    }

    println!("  First page table complete");

    let page_table_address = first_page_table_address();

    println!("  Installing PDE[0] -> 0x{:08x}", page_table_address);

    unsafe {
        pd_set(0, page_table_address | PageFlags::KERNEL_RW.bits());
    }

    println!("  PDE[0] installed");
    println!("  Identity mapping complete");
}

fn directory_index(address: u32) -> usize {
    ((address >> 22) & 0x3ff) as usize
}

fn table_index(address: u32) -> usize {
    ((address >> 12) & 0x3ff) as usize
}

pub fn translate(virtual_address: u32) -> Option<u32> {
    let directory = directory_index(virtual_address);
    let table = table_index(virtual_address);

    let directory_entry = unsafe { pd_get(directory) };

    if directory_entry & PRESENT == 0 {
        return None;
    }

    if directory != 0 {
        return None;
    }

    let table_entry = unsafe { pt_get(table) };

    if table_entry & PRESENT == 0 {
        return None;
    }

    let physical_page = table_entry & ADDRESS_MASK;
    let offset = virtual_address & 0xfff;

    Some(physical_page | offset)
}

pub fn map_page(virtual_address: u32, physical_address: u32, flags: PageFlags) {
    assert_page_aligned(virtual_address);
    assert_page_aligned(physical_address);

    let directory = directory_index(virtual_address);
    let table = table_index(virtual_address);

    if directory != 0 {
        panic!("map_page: page table not allocated");
    }

    let entry = (physical_address & ADDRESS_MASK) | flags.bits();

    unsafe {
        pt_set(table, entry);
    }

    flush_tlb();
}

pub fn unmap_page(virtual_address: u32) {
    assert_page_aligned(virtual_address);

    let directory = directory_index(virtual_address);
    let table = table_index(virtual_address);

    if directory != 0 {
        return;
    }

    unsafe {
        pt_set(table, 0);
    }

    flush_tlb();
}

fn flush_tlb() {
    let cr3 = control::read_cr3();
    unsafe {
        control::write_cr3(cr3);
    }
}

fn assert_page_aligned(address: u32) {
    if address & 0xfff != 0 {
        panic!("paging: address is not page aligned");
    }
}