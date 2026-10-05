#![no_std]
#![no_main]

#[macro_use]
mod macros;

mod arch;
mod capability;
mod console;
mod memory;
mod serial;
mod sync;
mod vga;

mod cpu;

use core::panic::PanicInfo;

use crate::capability::capability::CapabilityRights;

/// Zero every linker-defined region between `__bootstrap_start` and
/// `__kernel_end`.
///
/// These regions are declared `NOLOAD` in the linker script: they
/// occupy address space but are not part of the disk image loaded by
/// the bootloader. They must therefore be zeroed explicitly before
/// first use.
///
/// This includes:
///
/// - the page directory
/// - the bootstrap page table
/// - the IDT
/// - the GDT
/// - the frame bitmap
/// - the bootstrap stack
/// - `.bss`
///
/// The stack is *not* zeroed here in the current layout; if you
/// later move the bootstrap stack above `__kernel_end`, this
/// function remains correct.
unsafe fn clear_bootstrap_regions() {
    let start = arch::__bootstrap_start() as *mut u8;
    let end = arch::__kernel_end() as *mut u8;

    let size = (end as usize) - (start as usize);

    unsafe {
        core::ptr::write_bytes(start, 0, size);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn kernel_main() -> ! {
    unsafe {
        clear_bootstrap_regions();
    }

    console::init();
    cpu::gdt::init();
    cpu::idt::init();
    memory::paging::init();
    memory::direct_map::init();
    memory::frame::init();
    capability::init();
    test_capability_transfer();
    test_cells();
    test_address_space();

    test_address_space_activation();
    test_kernel_mapping_sharing();
    test_frame_allocator();
    test_address_space_isolation();
    test_page_fault();

    loop {
        core::hint::spin_loop();
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

fn test_address_space() {
    use crate::memory::address_space::AddressSpace;

    println!();
    println!("Testing address spaces...");

    let mut address_space = AddressSpace::new().expect("address-space creation failed");

    println!("  Page directory: 0x{:08x}", address_space.page_directory());

    let virtual_address = 0x0080_0000;
    let physical_address = 0x0010_0000;

    println!("  Checking initial mapping...");
    assert!(!address_space.is_mapped(virtual_address));
    println!("  Initial mapping check: SUCCESS");

    println!("  Creating mapping...");
    assert!(address_space.map(virtual_address, physical_address, true, false,));
    println!("  Mapping creation: SUCCESS");

    println!(
        "  Mapped VA 0x{:08x} -> PA 0x{:08x}",
        virtual_address, physical_address
    );

    println!("  Checking mapping...");
    let mapped = address_space.is_mapped(virtual_address);
    println!("  Mapping check returned: {}", mapped);

    assert!(mapped);
    println!("  Virtual mapping: SUCCESS");

    println!("  Translating...");
    let translated = address_space.translate(virtual_address);

    println!("  Translation result: 0x{:08x}", translated.unwrap_or(0));

    assert_eq!(translated, Some(physical_address));

    println!("  Virtual-to-physical translation: SUCCESS");

    println!("  Unmapping...");
    let unmapped = address_space.unmap(virtual_address);

    println!("  Unmap result: 0x{:08x}", unmapped.unwrap_or(0));

    assert_eq!(unmapped, Some(physical_address));

    println!("  Page unmapping: SUCCESS");

    println!("  Checking isolation...");
    assert!(!address_space.is_mapped(virtual_address));

    println!("  Address-space isolation: SUCCESS");
}

fn test_capability_transfer() {
    println!();
    println!("Testing capability transfer...");

    let core = capability::core_mut();

    let object = core.create_object().expect("object creation failed");

    println!("  Object created: {}", object.raw());

    let parent_rights = CapabilityRights::READ | CapabilityRights::WRITE | CapabilityRights::GRANT;

    let parent = core
        .allocate(object, parent_rights)
        .expect("parent capability allocation failed");

    println!("  Parent capability: 0x{:08x}", parent.raw());

    /*
     * Delegate READ only.
     */
    let child = core
        .transfer(parent, CapabilityRights::READ)
        .expect("READ transfer failed");

    println!("  Child capability:  0x{:08x}", child.raw());

    assert!(core.has_rights(child, CapabilityRights::READ));

    assert!(!core.has_rights(child, CapabilityRights::WRITE));

    assert!(!core.has_rights(child, CapabilityRights::EXECUTE));

    println!("  Rights attenuation: SUCCESS");

    /*
     * Both capabilities refer to the same object.
     */
    assert_eq!(core.object(parent), Some(object));

    assert_eq!(core.object(child), Some(object));

    println!("  Object sharing: SUCCESS");

    /*
     * Parent has WRITE, so it can delegate READ | WRITE.
     */
    let child_rw = core
        .transfer(parent, CapabilityRights::READ | CapabilityRights::WRITE)
        .expect("READ|WRITE transfer failed");

    assert!(core.has_rights(child_rw, CapabilityRights::READ));

    assert!(core.has_rights(child_rw, CapabilityRights::WRITE));

    println!("  Multi-right transfer: SUCCESS");

    /*
     * Parent does not have EXECUTE.
     */
    let denied = core.transfer(parent, CapabilityRights::EXECUTE);

    assert!(denied.is_none());

    println!("  Excess-rights rejection: SUCCESS");

    /*
     * A capability without GRANT cannot delegate.
     */
    let restricted = core
        .allocate(object, CapabilityRights::READ)
        .expect("restricted capability allocation failed");

    let denied = core.transfer(restricted, CapabilityRights::READ);

    assert!(denied.is_none());

    println!("  GRANT enforcement: SUCCESS");

    /*
     * Destroying the object must invalidate every capability
     * referring to it.
     */
    assert!(core.destroy_object(object));

    assert!(core.lookup(parent).is_none());
    assert!(core.lookup(child).is_none());
    assert!(core.lookup(child_rw).is_none());
    assert!(core.lookup(restricted).is_none());

    println!("  Object-wide revocation: SUCCESS");
}
fn test_cells() {
    println!();
    println!("Testing execution cells...");

    println!(
        "  CapabilityCore size: {} bytes",
        core::mem::size_of::<capability::core::CapabilityCore>()
    );
    let core = capability::core_mut();

    println!("  CapabilityCore address: 0x{:08x}", core as *mut _ as u32);

    let object = core.create_object().expect("object creation failed");

    println!("  Object created: {}", object.raw());

    println!("  Creating Cell A...");
    let cell_a = core.create_cell().expect("cell A creation failed");
    println!("  Cell A: {}", cell_a.raw());

    println!("  Creating Cell B...");
    let cell_b = core.create_cell().expect("cell B creation failed");
    println!("  Cell B: {}", cell_b.raw());
}

/// Deliberately triggers a page fault for exception-path testing.
///
/// This test accesses an unmapped virtual address so that the CPU raises
/// exception 14.
fn test_page_fault() {
    println!();
    println!("Testing page fault exception...");

    unsafe {
        let address = 0x0400_0000 as *mut u32;

        core::ptr::read_volatile(address);
    }
}

fn test_kernel_mapping_sharing() {
    use crate::memory::address_space::AddressSpace;
    use crate::memory::direct_map;

    println!();
    println!("Testing kernel mapping sharing...");

    let address_space = AddressSpace::new().expect("address-space creation failed");

    let kernel_virtual = direct_map::PHYSICAL_MEMORY_BASE;

    println!("  Kernel virtual address: 0x{:08x}", kernel_virtual);

    println!("  Checking kernel mapping...");

    assert!(address_space.is_mapped(kernel_virtual));

    println!("  Kernel mapping visibility: SUCCESS");

    let translated = address_space.translate(kernel_virtual);

    println!("  Translation: 0x{:08x}", translated.unwrap_or(0));

    assert_eq!(translated, Some(0));

    println!("  Kernel mapping translation: SUCCESS");
}

fn test_frame_allocator() {
    println!();
    println!("Testing frame allocator isolation...");

    for _ in 0..16 {
        let frame = memory::frame::allocate().expect("frame allocation failed");
        let address = frame.address();

        // Must not overlap any bootstrap region.
        assert!(
            address < arch::__bootstrap_start() || address >= arch::__bootstrap_stack_top(),
            "frame allocator returned a bootstrap frame: 0x{:08x}",
            address
        );

        println!("  Allocated frame: 0x{:08x}", address);
    }

    println!("  Frame allocator isolation: SUCCESS");
}

fn test_address_space_activation() {
    use crate::cpu::control;
    use crate::memory::address_space::AddressSpace;

    println!();
    println!("Testing address-space activation...");

    let mut space_a = AddressSpace::new().expect("address-space A creation failed");

    let mut space_b = AddressSpace::new().expect("address-space B creation failed");

    println!("  Address space A: 0x{:08x}", space_a.page_directory());

    println!("  Address space B: 0x{:08x}", space_b.page_directory());

    assert_ne!(space_a.page_directory(), space_b.page_directory());

    println!("  Private page directories: SUCCESS");

    let virtual_address = 0x0080_0000;

    let physical_a = 0x0010_0000;
    let physical_b = 0x0020_0000;

    println!("  Mapping A:");
    println!(
        "    VA 0x{:08x} -> PA 0x{:08x}",
        virtual_address, physical_a
    );

    assert!(space_a.map(virtual_address, physical_a, true, false,));

    println!("  Mapping B:");
    println!(
        "    VA 0x{:08x} -> PA 0x{:08x}",
        virtual_address, physical_b
    );

    assert!(space_b.map(virtual_address, physical_b, true, false,));

    assert_eq!(space_a.translate(virtual_address), Some(physical_a));

    assert_eq!(space_b.translate(virtual_address), Some(physical_b));

    println!("  Independent mappings: SUCCESS");

    /*
     * Activate A.
     */

    println!("  Activating address space A...");

    unsafe {
        space_a.activate();
    }

    let cr3 = control::read_cr3();

    println!("  CR3 = 0x{:08x}", cr3);

    assert_eq!(cr3, space_a.page_directory());

    println!("  Address-space A activation: SUCCESS");

    /*
     * Activate B.
     */

    println!("  Activating address space B...");

    unsafe {
        space_b.activate();
    }

    let cr3 = control::read_cr3();

    println!("  CR3 = 0x{:08x}", cr3);

    assert_eq!(cr3, space_b.page_directory());

    println!("  Address-space B activation: SUCCESS");

    /*
     * Switch back to A.
     */

    println!("  Switching back to A...");

    unsafe {
        space_a.activate();
    }

    let cr3 = control::read_cr3();

    println!("  CR3 = 0x{:08x}", cr3);

    assert_eq!(cr3, space_a.page_directory());

    println!("  Address-space switching: SUCCESS");
}

fn test_address_space_isolation() {
    use crate::memory::address_space::AddressSpace;

    println!();
    println!("Testing address-space isolation...");

    let mut space_a = AddressSpace::new().expect("space A creation failed");
    let mut space_b = AddressSpace::new().expect("space B creation failed");

    // A private VA that only A maps.
    let isolated_va = 0x0080_0000;
    let backing_pa = 0x0010_0000;

    println!(
        "  Space A maps VA 0x{:08x} -> PA 0x{:08x}",
        isolated_va, backing_pa
    );
    assert!(space_a.map(isolated_va, backing_pa, true, false));

    println!("  Space B does NOT map VA 0x{:08x}", isolated_va);
    assert!(!space_b.is_mapped(isolated_va));

    println!("  Activating A...");
    unsafe {
        space_a.activate();
    }

    println!("  Reading from A's mapping...");
    let value = unsafe { core::ptr::read_volatile(isolated_va as *const u32) };
    println!("  Read returned 0x{:08x}", value);
    println!("  Space A access: SUCCESS");

    println!("  Activating B...");
    unsafe {
        space_b.activate();
    }

    println!("  Reading from B's context (should fault)...");
    let _ = unsafe { core::ptr::read_volatile(isolated_va as *const u32) };
    // Never reached: the fault handler halts.
}
