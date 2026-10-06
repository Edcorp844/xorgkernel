//! Kernel mapping sharing tests.

use crate::memory::address_space::AddressSpace;
use crate::memory::direct_map;
use crate::println;

/// Tests that kernel mappings are visible in a new address space.
pub fn test_kernel_mapping_sharing() {
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