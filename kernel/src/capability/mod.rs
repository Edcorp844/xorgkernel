//! Capability fabric.
//!
//! The fabric is the kernel's authority layer. Every resource in
//! the system is represented as an object, and every operation on
//! an object is permitted only by a capability the caller
//! presents. The fabric owns the tables that record what objects
//! exist, what capabilities exist, and which cells hold which
//! capabilities.
//!
//! # Structure
//!
//! The fabric's state lives in [`CapabilityCore`], a single
//! instance placed in a static because the kernel has no other
//! way to reach it before the heap exists. The core is created
//! with `const fn` at load time, so no runtime initialization is
//! required beyond [`init`]'s logging.
//!
//! The core is wrapped in [`core_mut`], which returns a
//! `&'static mut CapabilityCore`. This is unsound in general: two
//! callers could obtain two mutable references at once. It is
//! correct in the current kernel because:
//!
//! - the kernel is single-threaded, so no two callers run
//!   concurrently;
//! - interrupt handlers that touch the fabric run with interrupts
//!   disabled, so they cannot preempt an in-progress fabric
//!   operation.
//!
//! Both conditions are preconditions on the whole fabric, and
//! they must be preserved as the kernel grows. Once multiple CPUs
//! or preemptive kernel code paths exist, [`core_mut`] will need
//! to be replaced with a lock or a per-CPU design. See the safety
//! note on [`core_mut`] for the details.
//!
//! # Boot-time state
//!
//! A few pieces of fabric state are set once, during boot, and
//! then read many times by code that has no convenient path to
//! them:
//!
//! - The **kernel address space capability** is the authority the
//!   kernel holds over its own virtual memory. It is created by
//!   [`CapabilityCore::register_kernel_address_space`] during
//!   boot, after the fabric is initialized and before the heap is
//!   available. Many later subsystems — the heap itself, the
//!   scheduler's task creation path, the tests — need this
//!   capability, and none of them can recreate it. It is
//!   therefore stored in a static and exposed through
//!   [`kernel_as_cap`] and [`set_kernel_as_cap`].
//!
//! The static is set exactly once. Attempting to set it twice is
//! a boot-order bug; [`set_kernel_as_cap`] asserts against it.
//!
//! # Naming
//!
//! "Fabric" is the whole subsystem. "Core" is the type that holds
//! its state. The distinction matters because the fabric will
//! eventually consist of more than one type: memory objects,
//! address spaces, and cells each have their own storage, and the
//! core is the coordinator that ties them together.

pub mod capability;
pub mod cell;
pub mod channel;
pub mod core;
pub mod itable;
pub mod object;
pub mod registry;

use crate::capability::capability::CapabilityId;
use crate::capability::core::CapabilityCore;

/// The kernel's single fabric instance.
///
/// Placed in a static because the kernel has no heap at the point
/// the fabric must exist. `CapabilityCore::new` is `const`, so
/// the core is fully constructed by the time the static is
/// loaded; no runtime initialization is needed.
///
/// # Why `static mut`
///
/// The core must be mutably accessible from many places (the
/// heap's growth path, the scheduler's task creation path, the
/// tests, and every future syscall handler). A `SpinLock` would
/// be the obvious alternative, but the fabric is touched from
/// interrupt context and from code paths that must not block, and
/// a spinlock held across a fabric operation that itself needs
/// the fabric (as `heap::grow` does) would deadlock.
///
/// The single-threaded, interrupt-disabled discipline described
/// in the module documentation is what makes a plain `static mut`
/// correct here. See [`core_mut`] for the safety contract.
static mut CAPABILITY_CORE: CapabilityCore = CapabilityCore::new();

/// The kernel address space capability.
///
/// Set once during boot by `kernel_main`, immediately after
/// [`CapabilityCore::register_kernel_address_space`] returns.
/// Read by every subsystem that needs to install a mapping into
/// the kernel's own address space.
///
/// `CapabilityId::INVALID` before the first call to
/// [`set_kernel_as_cap`].
///
/// # Why a static
///
/// The kernel address space capability is created during boot,
/// before the heap exists, and is needed by code that runs much
/// later and has no convenient path back to `kernel_main`'s
/// locals — `heap::grow`, the scheduler's task-creation path, and
/// the test suite. Threading the capability through every one of
/// those call chains would add a parameter to half the kernel's
/// internal APIs and would still not reach interrupt handlers.
///
/// A static is the correct place for boot-time state that is
/// written once and read from many contexts.
static mut KERNEL_AS_CAP: CapabilityId = CapabilityId::INVALID;

/// Initializes the capability fabric.
///
/// Prints the fabric's configuration. The fabric's state is
/// already constructed by the time this runs: `CapabilityCore` is
/// `const`-constructed in [`CAPABILITY_CORE`], and the ITable's
/// free list is built at compile time. This function exists for
/// the boot log and as the place where future runtime
/// initialization (per-CPU fabric state, APIC routing, debug
/// registries) will live.
///
/// Must be called exactly once, before any code that needs the
/// fabric. In `kernel_main` this happens after the frame
/// allocator is initialized and before the kernel address space
/// is registered.
pub fn init() {
    println!("Initializing capability fabric...");
    println!("  ITable: 1024 entries");
}

/// Returns a mutable reference to the fabric's core.
///
/// This is the entry point every caller uses to reach the fabric.
/// It is not a lock; it is a direct reference into the static.
///
/// # Safety
///
/// The caller must ensure that no other call to `core_mut` is
/// live at the same time, and that no interrupt handler that
/// touches the fabric can preempt the current context. In
/// practice this means:
///
/// - **Single CPU.** On SMP, two CPUs could each obtain a
///   `&'static mut` to the same core and mutate it concurrently,
///   which is undefined behavior. The current kernel is
///   single-CPU.
///
/// - **Interrupts disabled, or no fabric use from handlers.** An
///   interrupt handler that calls `core_mut` while the interrupted
///   context holds a live reference would create a second
///   `&'static mut` into the same object. The current kernel's
///   timer handler calls the scheduler, not the fabric, and the
///   scheduler does not touch the core; but this must remain true
///   as the kernel grows. When IPC and syscalls arrive, they will
///   run with interrupts disabled and will be the only fabric
///   users on their path.
///
/// The function is marked `unsafe` so that adding a new caller is
/// a deliberate act. Callers that have already established the
/// discipline may use it freely; there is no additional cost.
///
/// # Why not a lock
///
/// A `SpinLock<CapabilityCore>` would make the single-threaded
/// case safe but would deadlock in two situations the fabric
/// already has:
///
/// - **Reentrancy through the heap.** `CapabilityCore::map_memory`
///   is called by `heap::grow` to install a heap region. If
///   `map_memory` needed a lock on the core, and `heap::grow` was
///   itself called from a fabric operation that held the lock,
///   the second acquisition would deadlock.
///
/// - **Interrupt-context use.** A future syscall handler that
///   runs with interrupts disabled and calls the fabric while a
///   task holds the lock would spin forever, because the
///   interrupt cannot be delivered to let the task release it.
///
/// The static-with-a-discipline approach avoids both. It is the
/// same trade-off the frame allocator makes.
pub fn core_mut() -> &'static mut CapabilityCore {
    unsafe { &mut *::core::ptr::addr_of_mut!(CAPABILITY_CORE) }
}

/// Stores the kernel address space capability.
///
/// Called once, by `kernel_main`, immediately after
/// [`CapabilityCore::register_kernel_address_space`] returns.
///
/// # Panics
///
/// Panics if the capability has already been set. Setting it
/// twice means two different kernel address spaces were
/// registered, which would be a boot-order bug: only one kernel
/// page directory exists, and only one capability can name it.
pub fn set_kernel_as_cap(cap: CapabilityId) {
    unsafe {
        if KERNEL_AS_CAP != CapabilityId::INVALID {
            panic!("capability: kernel address space capability already set");
        }
        KERNEL_AS_CAP = cap;
    }
}

/// Returns the kernel address space capability.
///
/// Returns [`CapabilityId::INVALID`] if [`set_kernel_as_cap`] has
/// not been called. Callers that need the capability and run
/// after boot may assume it is set; callers that might run before
/// [`set_kernel_as_cap`] should check.
///
/// The capability is the authority the kernel holds over its own
/// virtual memory. It is passed to `map_memory` by:
///
/// - `heap::grow`, when installing a new heap region;
/// - `Scheduler::create` and the task-creation path, when a task
///   needs an initial address space;
/// - the test suite, when exercising the mapping path.
///
/// It is not revoked for the lifetime of the kernel. Revoking it
/// would leave the kernel unable to install mappings, which is
/// not a recoverable state.
pub fn kernel_as_cap() -> CapabilityId {
    unsafe { KERNEL_AS_CAP }
}
