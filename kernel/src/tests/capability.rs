//! Capability fabric tests.

use crate::capability::capability::CapabilityRights;
use crate::capability::object::ObjectKind;
use crate::println;

/// Tests the fabric's capability transfer and revocation paths.
///
/// This exercises:
///
/// - object creation
/// - capability allocation
/// - delegation with rights attenuation
/// - SHARE enforcement
/// - object-wide revocation
pub fn test_capability_transfer() {
    println!();
    println!("Testing capability transfer...");

    let core = crate::capability::core_mut();

    let object = core
        .create_object(ObjectKind::Cell)
        .expect("object creation failed");

    println!("  Object created: {}", object.raw());

    let parent_rights = CapabilityRights::READ | CapabilityRights::WRITE | CapabilityRights::SHARE;

    let parent = core
        .allocate(object, parent_rights)
        .expect("parent capability allocation failed");

    println!("  Parent capability: 0x{:08x}", parent.raw());

    let child = core
        .transfer(parent, CapabilityRights::READ)
        .expect("READ transfer failed");

    println!("  Child capability:  0x{:08x}", child.raw());

    assert!(core.has_rights(child, CapabilityRights::READ));
    assert!(!core.has_rights(child, CapabilityRights::WRITE));
    assert!(!core.has_rights(child, CapabilityRights::EXECUTE));

    println!("  Rights attenuation: SUCCESS");

    assert_eq!(core.object(parent), Some(object));
    assert_eq!(core.object(child), Some(object));

    println!("  Object sharing: SUCCESS");

    let child_rw = core
        .transfer(parent, CapabilityRights::READ | CapabilityRights::WRITE)
        .expect("READ|WRITE transfer failed");

    assert!(core.has_rights(child_rw, CapabilityRights::READ));
    assert!(core.has_rights(child_rw, CapabilityRights::WRITE));

    println!("  Multi-right transfer: SUCCESS");

    let denied = core.transfer(parent, CapabilityRights::EXECUTE);
    assert!(denied.is_none());

    println!("  Excess-rights rejection: SUCCESS");

    let restricted = core
        .allocate(object, CapabilityRights::READ)
        .expect("restricted capability allocation failed");

    let denied = core.transfer(restricted, CapabilityRights::READ);
    assert!(denied.is_none());

    println!("  SHARE enforcement: SUCCESS");

    assert!(core.destroy_object(object));

    assert!(core.lookup(parent).is_none());
    assert!(core.lookup(child).is_none());
    assert!(core.lookup(child_rw).is_none());
    assert!(core.lookup(restricted).is_none());

    println!("  Object-wide revocation: SUCCESS");
}

/// Tests execution cell creation and management.
pub fn test_cells() {
    println!();
    println!("Testing execution cells...");

    println!(
        "  CapabilityCore size: {} bytes",
        core::mem::size_of::<crate::capability::core::CapabilityCore>()
    );
    let core = crate::capability::core_mut();

    println!("  CapabilityCore address: 0x{:08x}", core as *mut _ as u32);

    let object = core
        .create_object(ObjectKind::Cell)
        .expect("object creation failed");

    println!("  Object created: {}", object.raw());

    println!("  Creating Cell A...");
    let cell_a = core.create_cell().expect("cell A creation failed");
    println!("  Cell A: {}", cell_a.raw());

    println!("  Creating Cell B...");
    let cell_b = core.create_cell().expect("cell B creation failed");
    println!("  Cell B: {}", cell_b.raw());
}
