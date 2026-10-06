//! Kernel test suite.
//!
//! The tests are grouped by the subsystem they exercise. Every
//! test module exposes one or more `pub fn test_*()` functions that
//! print progress to the console and panic on failure.
//!
//! The whole suite is invoked by [`run_all`], which `kernel_main`
//! calls once, after the substrate is initialized and before the
//! scheduler takes over.

pub mod address_space;
pub mod capability;
pub mod frame;
pub mod heap;
pub mod kernel_map;
pub mod memory;
pub mod scheduler;

/// Runs every test in the suite.
///
/// Tests run with interrupts disabled, so they cannot be
/// interleaved with timer ticks. They must finish before the
/// scheduler is given control.
pub fn run_all() {
    capability::test_capability_transfer();
    capability::test_cells();
    memory::test_memory_object_allocation();
    memory::test_map_memory();
    heap::test_heap();
    scheduler::test_scheduler();
    address_space::test_address_space();
    address_space::test_address_space_activation();
    kernel_map::test_kernel_mapping_sharing();
    frame::test_frame_allocator();
    address_space::test_address_space_isolation();
}