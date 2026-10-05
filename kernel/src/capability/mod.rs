pub mod capability;
pub mod cell;
pub mod core;
pub mod itable;
pub mod object;
pub mod registry;

use crate::capability::core::CapabilityCore;

static mut CAPABILITY_CORE: CapabilityCore = CapabilityCore::new();

pub fn init() {
    println!("Initializing capability fabric...");
    println!("  ITable: 1024 entries");
}

pub fn core_mut() -> &'static mut CapabilityCore {
    unsafe { &mut *::core::ptr::addr_of_mut!(CAPABILITY_CORE) }
}
