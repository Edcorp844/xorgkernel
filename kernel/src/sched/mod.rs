//! Scheduler primitives.
//!
//! This module provides the data structures and policy for
//! scheduling tasks: the `Task` type, per-priority run queues built
//! as intrusive linked lists, and a `Scheduler` that selects the
//! next task to run.
//!
//! Context switching is not yet implemented. `schedule` selects the
//! next task and updates its state, but the caller is responsible
//! for actually switching execution to it. When `switch_context` is
//! added, it will hook into the same primitives.

pub mod list;
pub mod scheduler;
pub mod task;

use scheduler::Scheduler;

/// The kernel's single scheduler.
static mut SCHEDULER: Scheduler = Scheduler::new();

/// Initializes the scheduler.
///
/// Must be called after the kernel heap is available, because task
/// creation allocates kernel stacks from the heap.
pub fn init() {
    println!("Initializing scheduler...");
    println!("  Priorities: {}", task::NUM_PRIORITIES);
}

/// Returns a mutable reference to the global scheduler.
pub fn scheduler_mut() -> &'static mut Scheduler {
    unsafe { &mut *core::ptr::addr_of_mut!(SCHEDULER) }
}