//! Memory-object and mapping tests.

use crate::capability::capability::CapabilityRights;
use crate::capability::core::MapError;
use crate::capability::object::ObjectKind;
use crate::println;

/// Tests the fabric's memory-object allocation path.
pub fn test_memory_object_allocation() {
    println!();
    println!("Testing memory object allocation...");

    let core = crate::capability::core_mut();

    let rights = CapabilityRights::MAP
        | CapabilityRights::READ
        | CapabilityRights::WRITE
        | CapabilityRights::SHARE;

    let (object, cap) = core
        .allocate_memory(4, rights)
        .expect("memory object allocation failed");

    println!("  Object created: {}", object.raw());
    println!("  Capability:     0x{:08x}", cap.raw());

    assert_eq!(
        core.lookup_object_kind(object),
        Some(ObjectKind::MemoryObject)
    );

    println!("  Object kind: MemoryObject: SUCCESS");

    assert!(core.has_rights(cap, CapabilityRights::MAP));
    assert!(core.has_rights(cap, CapabilityRights::READ));
    assert!(core.has_rights(cap, CapabilityRights::WRITE));
    assert!(!core.has_rights(cap, CapabilityRights::EXECUTE));
    assert!(core.has_rights(cap, CapabilityRights::SHARE));

    println!("  Rights: MAP|READ|WRITE|SHARE: SUCCESS");

    let mem = core
        .memory_object(cap)
        .expect("memory object lookup failed");

    assert_eq!(mem.page_count(), 4);

    println!("  Frames: {}", mem.page_count());

    for i in 0..4 {
        let frame = mem.frame(i).expect("frame missing");
        println!("    Frame {}: 0x{:08x}", i, frame.address());
    }

    let read_only = core
        .transfer(cap, CapabilityRights::READ)
        .expect("transfer failed");

    assert!(core.has_rights(read_only, CapabilityRights::READ));
    assert!(!core.has_rights(read_only, CapabilityRights::WRITE));

    println!("  Derived READ-only capability: 0x{:08x}", read_only.raw());
    println!("  Attenuation: SUCCESS");

    assert!(core.destroy_object(object));

    assert!(core.lookup(cap).is_none());
    assert!(core.lookup(read_only).is_none());

    println!("  Object-wide revocation: SUCCESS");
}

/// Tests mapping a memory object into an address space through the
/// fabric.
pub fn test_map_memory() {
    println!();
    println!("Testing map_memory...");

    let core = crate::capability::core_mut();

    let mem_rights = CapabilityRights::MAP
        | CapabilityRights::READ
        | CapabilityRights::WRITE
        | CapabilityRights::SHARE;

    let (mo_id, mo_cap) = core
        .allocate_memory(4, mem_rights)
        .expect("memory object allocation failed");

    println!("  Memory object: {}", mo_id.raw());
    println!("  MO capability: 0x{:08x}", mo_cap.raw());

    let as_rights = CapabilityRights::MAP
        | CapabilityRights::UNMAP
        | CapabilityRights::ACTIVATE
        | CapabilityRights::SHARE;

    let (as_id, as_cap) = core
        .allocate_address_space(as_rights)
        .expect("address space allocation failed");

    println!("  Address space: {}", as_id.raw());
    println!("  AS capability: 0x{:08x}", as_cap.raw());

    let va = 0x0080_0000;

    println!(
        "  Mapping VA 0x{:08x} <- memory object {}...",
        va,
        mo_id.raw()
    );

    core.map_memory(as_cap, mo_cap, va, true, false)
        .expect("map_memory failed");

    println!("  Mapping: SUCCESS");

    let aspace = core
        .address_space(as_cap)
        .expect("address space lookup failed");

    for i in 0..4 {
        let page_va = va + i * 4096;
        let translated = aspace.translate(page_va);

        assert!(
            translated.is_some(),
            "page {} not mapped after map_memory",
            i
        );

        println!(
            "    VA 0x{:08x} -> PA 0x{:08x}",
            page_va,
            translated.unwrap()
        );
    }

    println!("  Translation: SUCCESS");

    let ro_as = core
        .transfer(as_cap, CapabilityRights::SHARE)
        .expect("AS capability derivation failed");

    let result = core.map_memory(ro_as, mo_cap, va + 0x10_0000, true, false);

    assert_eq!(result, Err(MapError::MissingAddressSpaceMapRight));

    println!("  Rights enforcement on AS: SUCCESS");

    core.revoke(ro_as);

    println!("  Unmapping...");

    for i in 0..4 {
        let page_va = va + i * 4096;
        let unmapped = core
            .unmap_memory(as_cap, page_va)
            .expect("unmap_memory failed");

        println!(
            "    VA 0x{:08x} unmapped (was PA 0x{:08x})",
            page_va, unmapped
        );
    }

    let aspace = core
        .address_space(as_cap)
        .expect("address space lookup failed");

    assert!(!aspace.is_mapped(va));

    println!("  Unmapping: SUCCESS");

    assert!(core.destroy_object(as_id));
    assert!(core.destroy_object(mo_id));

    assert!(core.lookup(as_cap).is_none());
    assert!(core.lookup(mo_cap).is_none());

    println!("  Object-wide revocation: SUCCESS");
}
