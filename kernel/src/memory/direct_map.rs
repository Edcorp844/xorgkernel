use crate::cpu::control;
use crate::memory::paging;

/// Base virtual address of the kernel's physical-memory window.
///
/// Physical address zero is visible at this virtual address.
pub const PHYSICAL_MEMORY_BASE: u32 = 0xc000_0000;

/// Size of the physical-memory window.
///
/// The current machine reports approximately 128 MiB of usable physical
/// memory. Mapping the entire first 128 MiB gives the kernel a permanent
/// virtual address through which it can access every currently relevant
/// physical frame.
pub const PHYSICAL_MEMORY_SIZE: u32 = 128 * 1024 * 1024;

/// x86 4 MiB page size.
const LARGE_PAGE_SIZE: u32 = 4 * 1024 * 1024;

/// Page-directory entry flags.
const PAGE_PRESENT: u32 = 1 << 0;
const PAGE_WRITABLE: u32 = 1 << 1;
const PAGE_SIZE: u32 = 1 << 7;

/// Installs the kernel's physical-memory window.
///
/// The mapping is:
///
/// `0xc0000000 + physical_address -> physical_address`
///
/// using 4 MiB x86 pages.
pub fn init() {
    println!();
    println!("Initializing kernel physical-memory window...");
    println!("  Virtual base: 0x{:08x}", PHYSICAL_MEMORY_BASE);
    println!(
        "  Physical size: {} MiB",
        PHYSICAL_MEMORY_SIZE / (1024 * 1024)
    );

    control::enable_pse();

    let page_directory = paging::page_directory_address();

    let page_directory_ptr = page_directory as *mut u32;

    let first_directory_index = (PHYSICAL_MEMORY_BASE >> 22) as usize;

    let page_count = (PHYSICAL_MEMORY_SIZE / LARGE_PAGE_SIZE) as usize;

    for index in 0..page_count {
        let physical_address = (index as u32) * LARGE_PAGE_SIZE;

        let entry = physical_address | PAGE_PRESENT | PAGE_WRITABLE | PAGE_SIZE;

        unsafe {
            core::ptr::write_volatile(page_directory_ptr.add(first_directory_index + index), entry);
        }
    }

    flush_tlb();

    println!("  Page directory: 0x{:08x}", page_directory);
    println!(
        "  PDE range: {} - {}",
        first_directory_index,
        first_directory_index + page_count - 1
    );
    println!("  Physical-memory window enabled.");
}

/// Converts a physical address into its kernel virtual address.
///
/// The caller must ensure the physical address falls inside the mapped
/// physical-memory window.
pub const fn phys_to_virt(physical_address: u32) -> u32 {
    PHYSICAL_MEMORY_BASE + physical_address
}

/// Converts a kernel physical-memory-window address back to a physical
/// address.
pub const fn virt_to_phys(virtual_address: u32) -> Option<u32> {
    if virtual_address < PHYSICAL_MEMORY_BASE {
        return None;
    }

    let physical_address = virtual_address - PHYSICAL_MEMORY_BASE;

    if physical_address >= PHYSICAL_MEMORY_SIZE {
        return None;
    }

    Some(physical_address)
}

/// Returns a mutable byte pointer for a physical address.
///
/// The returned pointer is valid only while the physical-memory window is
/// active and the physical address lies within its mapped range.
pub fn physical_ptr(physical_address: u32) -> *mut u8 {
    phys_to_virt(physical_address) as *mut u8
}

/// Clears one physical page through the kernel physical-memory window.
pub fn clear_page(physical_address: u32) {
    let address = physical_ptr(physical_address);

    unsafe {
        core::ptr::write_bytes(address, 0, 4096);
    }
}

/// Invalidates the current TLB.
///
/// Reloading CR3 invalidates the non-global mappings on this architecture.
fn flush_tlb() {
    let cr3 = control::read_cr3();

    unsafe {
        control::write_cr3(cr3);
    }
}
