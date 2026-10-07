//! Kernel test suite.
//!
//! The tests are grouped by the subsystem they exercise. Every
//! test module exposes one or more `pub fn test_*()` functions that
//! print progress to the console and panic on failure.
//!
//! The whole suite is invoked by [`run_all`], which `kernel_main`
//! calls once, after the substrate is initialized and before the
//! scheduler takes over.
//!
//! # When tests run
//!
//! `run_all` is called from `kernel_main` after the capability
//! fabric, the kernel address space registration, and the kernel
//! heap have all been initialized, and before `sched::init`. The
//! tests therefore have the full substrate available to them: the
//! frame allocator, the fabric, the heap, and the address-space
//! machinery. They do **not** have the scheduler: no tasks exist
//! yet, interrupts are disabled, and calling `schedule_and_switch`
//! would lose control of the CPU because it never returns.
//!
//! This placement is deliberate. Tests that exercise the fabric,
//! the heap, and the address-space code need those subsystems to be
//! up. Tests that exercise the scheduler's bookkeeping need the
//! scheduler's static to be reachable, which it is — the static is
//! constructed at load time and does not require `sched::init` to
//! have run. `sched::init` itself only prints a log line; no
//! state is initialized by it.
//!
//! # Interrupts
//!
//! Interrupts are disabled for the entire duration of `run_all`.
//! This is required: several tests allocate frames, map pages, and
//! mutate the fabric's tables, and a timer tick in the middle of
//! any of those would call `schedule_and_switch`, which would
//! switch away from the test and never return. The tests must
//! finish before `setup_interrupts` enables interrupts, which is
//! the case in `kernel_main`.
//!
//! # Test style
//!
//! Every test prints a short banner and a sequence of `SUCCESS`
//! lines, then `assert!`s its invariants. A failure panics with the
//! assertion message, which is printed by the kernel's panic
//! handler along with the location. On success, the serial console
//! shows a readable trace of what was exercised, which is useful
//! when the boot log is being scanned for a specific subsystem.
//!
//! The `SUCCESS` lines are not decorative. They are the mechanism
//! by which a developer running the kernel in QEMU can see how far
//! boot got when the machine hangs or triple-faults without
//! producing a panic message. A missing `SUCCESS` line localizes
//! the fault to the test that did not complete.
//!
//! # Capability tests
//!
//! The capability module's public entry point is still named
//! `test_capability_transfer`, but it now runs six focused tests
//! covering the fabric's three-operation model:
//!
//! - `test_object_lifecycle` — object creation, capability
//!   allocation, object-wide revocation.
//! - `test_copy_capability` — the leaf-only duplication
//!   operation: source survives, target gets a SHARE-stripped
//!   copy.
//! - `test_affine_invariant` — the paper's Theorem 1: after a
//!   move, the source is consumed and at most one Owned token
//!   exists.
//! - `test_share_stripping` — requests for `SHARE` in a copy are
//!   rejected, not silently stripped.
//! - `test_move_failure_modes` — `move_capability` preserves its
//!   source on failure; `try_move_capability` consumes it.
//! - `test_cross_cell_operations` — `copy_between_cells` and
//!   `move_between_cells` maintain the source and target cells'
//!   namespaces correctly.
//!
//! The `transfer` name survives only as the entry point's name, so
//! that `run_all` does not need to change when the capability test
//! suite is restructured. The individual tests have names that
//! match the fabric's current vocabulary.

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
///
/// # Order
///
/// The order below is not arbitrary. Earlier tests exercise more
/// primitive subsystems than later ones, so a failure in an early
/// test localizes to a small surface. Several later tests depend
/// on state established by earlier ones:
///
/// - `capability::test_capability_transfer` allocates and destroys
///   objects through the fabric. Later fabric tests assume the
///   fabric has been exercised at least once and its tables are in
///   a known state.
/// - `memory::test_memory_object_allocation` and
///   `memory::test_map_memory` allocate frames and install
///   mappings, so the frame allocator and the address-space code
///   must be working. They are run before the heap test because
///   the heap's own regions are installed through the fabric's
///   mapping path; if that path is broken, the heap test will
///   report a failure that is really a mapping failure.
/// - `heap::test_heap` depends on the heap, which depends on the
///   fabric's `allocate_memory` and `map_memory`. Both are
///   exercised by the memory tests that run before it.
/// - `scheduler::test_scheduler` and
///   `scheduler::test_task_fabric_linkage` create cells and use
///   the kernel address space capability. Both require the fabric
///   to be working and the kernel address space to have been
///   registered. The kernel address space registration happens in
///   `kernel_main` before `run_all` is called.
/// - `address_space::test_address_space` and its companions
///   allocate page directories and exercise CR3 activation. They
///   run after the scheduler tests because they touch CR3
///   directly and would otherwise perturb the address space the
///   scheduler tests assume is active.
/// - `frame::test_frame_allocator` allocates frames without
///   freeing them. It runs last among the allocators so that the
///   frames it leaks do not affect the counts that earlier tests
///   observe.
///
/// A test that fails and takes the kernel down will, by definition,
/// prevent the tests after it from running. The order above
/// minimizes the chance that an early failure is masked by a later
/// one.
pub fn run_all() {
    capability::test_capability_operations();
    capability::test_cells();
    memory::test_memory_object_allocation();
    memory::test_map_memory();
    heap::test_heap();
    scheduler::test_scheduler();
    scheduler::test_task_fabric_linkage();
    address_space::test_address_space();
    address_space::test_address_space_activation();
    kernel_map::test_kernel_mapping_sharing();
    frame::test_frame_allocator();
    address_space::test_address_space_isolation();
}
