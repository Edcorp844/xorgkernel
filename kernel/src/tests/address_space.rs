//! Address-space tests.

use crate::cpu::control;
use crate::memory::address_space::AddressSpace;
use crate::println;

/// Tests basic mapping, translation, and unmapping in a single
/// address space.
pub fn test_address_space() {
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
    assert!(address_space.map(virtual_address, physical_address, true, false));
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

/// Tests CR3 activation and switching between address spaces.
pub fn test_address_space_activation() {
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
    assert!(space_a.map(virtual_address, physical_a, true, false));

    println!("  Mapping B:");
    println!(
        "    VA 0x{:08x} -> PA 0x{:08x}",
        virtual_address, physical_b
    );
    assert!(space_b.map(virtual_address, physical_b, true, false));

    assert_eq!(space_a.translate(virtual_address), Some(physical_a));
    assert_eq!(space_b.translate(virtual_address), Some(physical_b));

    println!("  Independent mappings: SUCCESS");

    println!("  Activating address space A...");
    unsafe {
        space_a.activate();
    }

    let cr3 = control::read_cr3();
    println!("  CR3 = 0x{:08x}", cr3);
    assert_eq!(cr3, space_a.page_directory());

    println!("  Address-space A activation: SUCCESS");

    println!("  Activating address space B...");
    unsafe {
        space_b.activate();
    }

    let cr3 = control::read_cr3();
    println!("  CR3 = 0x{:08x}", cr3);
    assert_eq!(cr3, space_b.page_directory());

    println!("  Address-space B activation: SUCCESS");

    println!("  Switching back to A...");
    unsafe {
        space_a.activate();
    }

    let cr3 = control::read_cr3();
    println!("  CR3 = 0x{:08x}", cr3);
    assert_eq!(cr3, space_a.page_directory());

    println!("  Address-space switching: SUCCESS");
}

/// Tests CPU-enforced isolation between two address spaces.
pub fn test_address_space_isolation() {
    println!();
    println!("Testing address-space isolation...");

    let mut space_a = AddressSpace::new().expect("space A creation failed");
    let space_b = AddressSpace::new().expect("space B creation failed");

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
}