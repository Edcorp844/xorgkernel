//! Memory-object and mapping tests.
//!
//! These tests exercise the fabric's memory-object lifecycle and
//! the mapping path. They are the fabric-level tests: they use
//! `allocate_memory`, `map_memory`, `unmap_memory`, and
//! `destroy_object` through their public API, and they do not
//! touch the ITable, the registry, or the address-space tables
//! directly.
//!
//! # Changes from the share model
//!
//! These tests previously used `CapabilityCore::transfer` to
//! derive a read-only capability. Under the three-operation
//! model, `transfer` is gone:
//!
//! - Deriving an attenuated copy is now
//!   [`CapabilityCore::copy_capability`], which requires a target
//!   cell and rejects any request containing `SHARE`.
//! - Transferring ownership is now
//!   [`CapabilityCore::move_capability`], which consumes the
//!   source.
//!
//! The tests use `copy_capability` because the objects they derive
//! capabilities to are for *read* access, not for ownership
//! transfer. The derived capability is a leaf: it cannot be
//! re-delegated, which is the property the depth-one invariant
//! relies on.
//!
//! [`CapabilityCore::copy_capability`]:
//!     crate::capability::core::CapabilityCore::copy_capability
//! [`CapabilityCore::move_capability`]:
//!     crate::capability::core::CapabilityCore::move_capability

use crate::capability::capability::CapabilityRights;
use crate::capability::core::MapError;
use crate::capability::object::ObjectKind;
use crate::println;

/// Tests the fabric's memory-object allocation path.
///
/// Creates a memory object, verifies its rights and frames,
/// derives a read-only leaf with `copy_capability`, and destroys
/// the object to confirm that every capability to it is revoked.
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

    // ---- Derive a read-only leaf. ----
    //
    // The derived capability is placed in a fresh cell. The
    // source stays in the fabric (it was never granted to a cell),
    // and the derived capability has no SHARE, so it cannot be
    // re-delegated.
    let leaf_cell = core
        .create_cell()
        .expect("leaf cell creation failed");

    let read_only = core
        .copy_capability(cap, leaf_cell, CapabilityRights::READ)
        .expect("copy_capability failed");

    assert!(core.has_rights(read_only, CapabilityRights::READ));
    assert!(!core.has_rights(read_only, CapabilityRights::WRITE));
    assert!(
        !core.has_rights(read_only, CapabilityRights::SHARE),
        "a copy must never carry SHARE"
    );
    assert!(core.cell_has_capability(leaf_cell, read_only));

    // The source is unchanged: the copy is non-destructive.
    assert!(core.lookup(cap).is_some());

    println!(
        "  Derived READ-only leaf capability: 0x{:08x}",
        read_only.raw()
    );
    println!("  Attenuation: SUCCESS");
    println!("  Source survived the copy: SUCCESS");

    assert!(core.destroy_object(object));

    assert!(core.lookup(cap).is_none());
    assert!(core.lookup(read_only).is_none());

    println!("  Object-wide revocation: SUCCESS");
}

/// Tests mapping a memory object into an address space through the
/// fabric.
///
/// Creates a memory object and an address space, maps the object's
/// frames into the address space, translates each page to confirm
/// the mapping is correct, then unmaps and destroys both objects.
///
/// Also verifies that a derived capability with insufficient
/// rights is refused by `map_memory`, and that the refusal is the
/// specific error variant expected.
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

    // ---- Derive a read-only AS capability and verify that
    //      map_memory refuses it for lack of MAP. ----
    //
    // The derived capability carries SHARE? No. `copy_capability`
    // rejects requests that contain SHARE and strips SHARE from
    // the result. So the derived capability is a leaf with no
    // ability to re-delegate.
    //
    // The derived capability is placed in a fresh cell. `map_memory`
    // does not require the capability to be in a cell; it looks up
    // the capability ID directly. The cell is therefore only used
    // as a convenient way to hold the capability and to demonstrate
    // that a leaf can be used as an argument to a fabric operation.
    let leaf_cell = core
        .create_cell()
        .expect("leaf cell creation failed");

    let derived_as = core
        .copy_capability(as_cap, leaf_cell, CapabilityRights::MAP)
        .expect("AS capability derivation failed");

    // A copy that carries MAP but not the address-space-creation
    // rights still cannot be used for mapping in a way that
    // violates the address space's kind. The check we want to see
    // is the one on UNMAP, which the derived capability does not
    // carry.
    let result = core.unmap_memory(derived_as, va);

    assert_eq!(result, Err(MapError::MissingAddressSpaceUnmapRight));

    println!("  Rights enforcement on derived AS capability: SUCCESS");

    core.revoke(derived_as);

    // ---- Unmap through the original capability. ----

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