//! Schedulable tasks.
//!
//! A `Task` is a kernel execution context: a saved register
//! state, a kernel stack, and the metadata the scheduler needs to
//! decide when the task runs next.
//!
//! # Intrusive links
//!
//! Tasks carry two independent pairs of intrusive links:
//!
//! - `all_next` / `all_prev` are used by the scheduler's
//!   all-tasks list, which contains every live task for its
//!   entire lifetime.
//! - `run_next` / `run_prev` are used by the current run queue
//!   when the task is in the `Ready` state, and are cleared
//!   otherwise.
//!
//! Having two pairs means a task can be in the all-tasks list and
//! a run queue at the same time. This is required: the scheduler
//! must be able to look up a task by ID (via the all-tasks list)
//! while the task is also queued for execution.
//!
//! The layout and offsets of these fields are fixed at compile
//! time. `scheduler.rs` uses `core::mem::offset_of!` to compute
//! the offsets passed to `IntrusiveList::new`, so the list code
//! does not depend on the field order and cannot drift from the
//! struct definition.
//!
//! # Fabric linkage
//!
//! Every task is linked to the capability fabric by two fields:
//!
//! - `cell` identifies the task's capability namespace. It is
//!   the set of capabilities the task is permitted to present to
//!   the fabric. A kernel task's cell holds the capabilities the
//!   kernel has decided to give it; a user cell's cell holds the
//!   capabilities granted to it by other cells.
//!
//! - `address_space` is the capability to the address space the
//!   task runs in. Kernel tasks share the kernel's address space,
//!   so this field is the kernel address space capability for
//!   every kernel task today. When user cells arrive, a cell's
//!   task will carry a capability to its own address space here,
//!   and the scheduler will activate that address space across
//!   the context switch.
//!
//! Both fields exist so that the scheduler and the fabric are
//! linked at the task boundary. Without them, a running task
//! would have no authority (no cell) and no address space (no
//! `address_space`), and the fabric would be a library that only
//! the tests call. With them, every running task is a
//! first-class fabric participant, and the prerequisite for
//! syscalls, IPC, and user mode is in place.
//!
//! # Assembly contract
//!
//! The `esp` field must be at offset 0 of the struct. The
//! `switch_context` routine reads and writes this field directly
//! through a raw pointer cast to `*mut u32`. `#[repr(C)]`
//! guarantees the field order.
//!
//! The `cell` and `address_space` fields are placed *after* the
//! assembly-visible prefix (`esp`, `id`, `name`, `state`,
//! `priority`) and *before* the intrusive links. This ordering
//! is deliberate: it keeps the fields the assembly touches at
//! stable, low offsets while keeping the fields the fabric needs
//! grouped together and near the identity fields. The offsets
//! used by `IntrusiveList` are computed at compile time via
//! `offset_of!`, so the link fields can move without breaking
//! the list code.

use core::ptr::NonNull;

use alloc::boxed::Box;

use crate::capability::capability::CapabilityId;
use crate::capability::cell::CellId;

// ---------------------------------------------------------------------
// Task state
// ---------------------------------------------------------------------

/// The scheduling state of a task.
///
/// The state machine is:
///
/// ```text
///              create
///                │
///                ▼
///   ┌───────── Ready ◄───────── wake ─────────┐
///   │           │                             │
///   │        schedule                         │
///   │           │                             │
///   │           ▼                             │
///   │        Running ───── block ─────► Blocked
///   │           │
///   │        exit
///   │           │
///   │           ▼
///   └──────► Dead
/// ```
///
/// Every state has a specific meaning to the scheduler:
///
/// - `Ready`: in a run queue, eligible for selection.
/// - `Running`: currently on the CPU. Not in any run queue.
/// - `Blocked`: waiting for an event. Not in any run queue.
/// - `Dead`: finished. Removed from every list; awaiting reaping.
///
/// A task in `Running` or `Blocked` is still in the all-tasks
/// list, so it can be looked up by ID. Only `Dead` tasks are
/// absent from every list.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TaskState {
    /// Runnable, sitting in a run queue.
    Ready,

    /// Currently executing on the CPU.
    Running,

    /// Waiting for an event. Not in any run queue.
    Blocked,

    /// Finished. Waiting to be reaped.
    Dead,
}

// ---------------------------------------------------------------------
// Priorities
// ---------------------------------------------------------------------

/// Number of priority levels.
///
/// Level 0 is the lowest priority (idle), level 7 the highest
/// (real-time). The scheduler keeps one run queue per level.
///
/// The value must be a power of two or less than 32 so that the
/// scheduler's `active` bitmask can represent every level. It is
/// 8 because eight levels are enough to distinguish idle,
/// background, ordinary, driver, and real-time work, and because
/// every additional level adds a run queue and a bitmask shift to
/// the scheduler's hot path.
pub const NUM_PRIORITIES: usize = 8;

/// Priority of the idle task.
///
/// The idle task runs only when no other task is ready. It
/// executes `hlt` in a loop, suspending the CPU until the next
/// interrupt. Putting it at the lowest priority ensures it never
/// preempts useful work.
pub const PRIORITY_IDLE: u8 = 0;

/// Priority of background kernel work.
///
/// Used for work that should yield to anything latency-sensitive
/// but must still make progress: deferred cleanup, periodic
/// scans, log flushing.
pub const PRIORITY_LOW: u8 = 2;

/// Priority of ordinary kernel tasks.
///
/// The default priority for a kernel task that has no special
/// latency requirement.
pub const PRIORITY_NORMAL: u8 = 4;

/// Priority of driver and latency-sensitive tasks.
///
/// Used for tasks whose latency directly affects the user's
/// experience: input handling, network receive, audio.
pub const PRIORITY_HIGH: u8 = 5;

/// Priority of real-time tasks. The highest level.
///
/// Used for tasks with hard deadlines: the timer tick, motor
/// control, flight-management loops. A task at this priority
/// preempts everything below it.
pub const PRIORITY_REALTIME: u8 = 7;

// ---------------------------------------------------------------------
// Task
// ---------------------------------------------------------------------

/// A schedulable kernel execution context.
///
/// See the module documentation for the task's role, the
/// intrusive-link design, and the fabric linkage.
#[repr(C)]
pub struct Task {
    // ---- Assembly-visible fields. ----
    /// Saved ESP.
    ///
    /// Must be at offset 0. `switch_context` reads and writes
    /// this field through a raw pointer. The saved stack holds
    /// the task's callee-saved registers (EBX, ESI, EDI, EBP)
    /// and, for a task that has not yet run, a frame that returns
    /// into the task's entry point.
    pub esp: u32,

    // ---- Identity. ----
    /// Stable identifier.
    ///
    /// Assigned by the scheduler at creation. Never reused: the
    /// scheduler's `next_id` increments monotonically and wraps
    /// to 1. IDs are opaque to everything except the scheduler's
    /// lookup path.
    pub id: u32,

    /// Human-readable name for diagnostics.
    ///
    /// Never dereferenced by the scheduler. Used only in boot
    /// logs and, later, in `ps`-like tools.
    pub name: &'static str,

    /// Current scheduling state.
    ///
    /// See [`TaskState`] for the state machine.
    pub state: TaskState,

    /// Priority. Higher values are more urgent.
    ///
    /// Must be less than [`NUM_PRIORITIES`]. Set at creation and
    /// never changed; priority inheritance, if it is added, will
    /// need a separate field so that the original priority can be
    /// restored.
    pub priority: u8,

    // ---- Fabric linkage. ----
    /// The task's capability namespace.
    ///
    /// A kernel task's cell holds the capabilities the kernel
    /// has granted it. The cell is created by the task's creator
    /// (see `Scheduler::create`'s callers) and populated with
    /// [`CapabilityCore::grant_capability`] before the task is
    /// scheduled for the first time.
    ///
    /// Every real task has a valid cell. The bootstrap context
    /// has [`CellId::INVALID`], because it runs before the
    /// fabric exists; see [`Task::bootstrap`].
    ///
    /// [`CapabilityCore::grant_capability`]:
    ///     crate::capability::core::CapabilityCore::grant_capability
    pub cell: CellId,

    /// The address space the task runs in.
    ///
    /// For a kernel task, this is the kernel address space
    /// capability. The kernel's own page directory is shared by
    /// every kernel task, so activating it on a context switch
    /// would only flush the TLB; the scheduler compares this
    /// field against the currently-active address space and
    /// skips the activation when they match.
    ///
    /// For a user cell, this is a capability to the cell's own
    /// address space. The scheduler activates it before switching
    /// to the task, so the task resumes running in its own
    /// virtual memory.
    ///
    /// The bootstrap context has [`CapabilityId::INVALID`]. The
    /// scheduler never activates an invalid capability; it treats
    /// it as "the current address space is already correct,"
    /// which is true for the bootstrap context because it runs in
    /// whatever address space `kernel_main` established.
    pub address_space: CapabilityId,

    // ---- Links for the all-tasks list. ----
    /// Next task in the all-tasks list.
    pub all_next: Option<NonNull<Task>>,

    /// Previous task in the all-tasks list.
    pub all_prev: Option<NonNull<Task>>,

    // ---- Links for the run queue. ----
    /// Next task in the current run queue.
    pub run_next: Option<NonNull<Task>>,

    /// Previous task in the current run queue.
    pub run_prev: Option<NonNull<Task>>,

    // ---- Resources. ----
    /// Kernel stack, owned by the task.
    ///
    /// `None` for the bootstrap context, which runs on the
    /// initial stack. Every real task has `Some(...)`, allocated
    /// from the kernel heap.
    ///
    /// The stack is owned, not borrowed: when the task is reaped,
    /// the `Box` is dropped and the frames return to the frame
    /// allocator.
    pub kernel_stack: Option<Box<[u8]>>,

    /// Base virtual address of the kernel stack.
    ///
    /// Cached because `kernel_stack` is a `Box`, and reaching
    /// through it to get the base address on every context switch
    /// would be a load that the scheduler does not need.
    pub kernel_stack_base: u32,

    /// Size of the kernel stack, in bytes.
    ///
    /// Cached for the same reason as `kernel_stack_base`.
    pub kernel_stack_size: u32,
}

impl Task {
    /// Creates a task with the given parameters.
    ///
    /// Allocates a kernel stack from the heap and lays out an
    /// initial frame so that when `switch_context` later switches
    /// to this task, it begins executing `entry`.
    ///
    /// The initial frame matches what `switch_context` expects:
    ///
    /// ```text
    ///   [higher addresses]
    ///   +-----------------------+
    ///   | entry point address   |  <- the return address
    ///   +-----------------------+
    ///   | 0  (EBP)              |
    ///   | 0  (EDI)              |
    ///   | 0  (ESI)              |
    ///   | 0  (EBX)              |  <- saved ESP points here
    ///   +-----------------------+
    ///   [lower addresses]
    /// ```
    ///
    /// # Arguments
    ///
    /// - `id`: the task's identifier, assigned by the scheduler.
    /// - `name`: a human-readable name, used only in diagnostics.
    /// - `entry`: the function the task starts executing. Must
    ///   return `!`; see below.
    /// - `priority`: the task's priority, less than
    ///   [`NUM_PRIORITIES`].
    /// - `stack_size`: the size of the kernel stack in bytes.
    ///   Must be at least 256, and should be a multiple of the
    ///   page size for the heap to allocate it efficiently.
    /// - `cell`: the task's capability namespace. Must be valid.
    /// - `address_space`: the capability to the task's address
    ///   space. Must not be [`CapabilityId::INVALID`].
    ///
    /// # Why `entry` returns `!`
    ///
    /// When the task is first scheduled, `switch_context` returns
    /// directly into `entry` with no return address on the stack.
    /// If `entry` were to return, the CPU would jump to whatever
    /// garbage lies below it. Declaring the entry point as
    /// returning `!` makes the compiler enforce this at every
    /// call site.
    ///
    /// # Return value
    ///
    /// Returns `None` if the heap cannot provide the requested
    /// stack size, or if a precondition is violated. Preconditions
    /// are checked with `assert!` rather than returned as errors,
    /// because violating them is a programming error: the caller
    /// either has a constant priority out of range or has passed
    /// an invalid cell or address space, and neither is a runtime
    /// condition the task-creation path is expected to handle.
    pub fn create(
        id: u32,
        name: &'static str,
        entry: fn() -> !,
        priority: u8,
        stack_size: usize,
        cell: CellId,
        address_space: CapabilityId,
    ) -> Option<Self> {
        assert!(
            priority < NUM_PRIORITIES as u8,
            "task: priority out of range"
        );
        assert!(stack_size >= 256, "task: stack size too small");
        assert!(cell.is_valid(), "task: invalid cell");
        assert!(
            address_space != CapabilityId::INVALID,
            "task: invalid address space"
        );

        // Allocate the kernel stack from the heap.
        let kernel_stack = alloc::vec![0u8; stack_size].into_boxed_slice();

        let base = kernel_stack.as_ptr() as u32;
        let top = base + stack_size as u32;

        // Lay out the initial frame at the top of the stack.
        //
        // Writing directly into the stack bytes is safe because
        // the stack was just allocated and nothing else has
        // touched it.
        //
        // The order of the register pushes must match the pop
        // order in `switch_context`: the routine pops EBX, ESI,
        // EDI, EBP, so the stack from lowest to highest must be
        // EBX, ESI, EDI, EBP. Getting this wrong produces a task
        // that starts with garbage in its callee-saved registers
        // and crashes on the first non-trivial operation.
        let mut sp = top;
        unsafe {
            // Return address: the entry point.
            sp -= 4;
            ::core::ptr::write(sp as *mut u32, entry as usize as u32);

            // Saved callee-saved registers, in the order
            // `switch_context` will pop them.
            sp -= 4;
            ::core::ptr::write(sp as *mut u32, 0); // EBP
            sp -= 4;
            ::core::ptr::write(sp as *mut u32, 0); // EDI
            sp -= 4;
            ::core::ptr::write(sp as *mut u32, 0); // ESI
            sp -= 4;
            ::core::ptr::write(sp as *mut u32, 0); // EBX
        }

        Some(Self {
            esp: sp,
            id,
            name,
            state: TaskState::Ready,
            priority,
            cell,
            address_space,
            all_next: None,
            all_prev: None,
            run_next: None,
            run_prev: None,
            kernel_stack: Some(kernel_stack),
            kernel_stack_base: base,
            kernel_stack_size: stack_size as u32,
        })
    }

    /// Creates a placeholder task for the kernel bootstrap
    /// context.
    ///
    /// The bootstrap context represents `kernel_main` running on
    /// the initial stack, before any real task exists. It is
    /// passed as the `from` argument to the first
    /// `switch_context` call, so that the routine has somewhere
    /// to save the kernel's own registers.
    ///
    /// After the first switch, the bootstrap context is never
    /// used again: the kernel runs as the idle task or as one of
    /// the tasks it created. It is never scheduled, never
    /// observed by anything other than `switch_context`, and
    /// never reaped.
    ///
    /// The `esp` field is zero and is overwritten by
    /// `switch_context` on the first call.
    ///
    /// The `cell` and `address_space` fields are
    /// [`CellId::INVALID`] and [`CapabilityId::INVALID`]
    /// respectively. The bootstrap context runs before the fabric
    /// exists, so it has no cell and no address-space capability.
    /// The scheduler treats the invalid address space as "leave
    /// CR3 alone," which is correct: the bootstrap context runs
    /// in whatever address space `kernel_main` established.
    pub const fn bootstrap() -> Self {
        Self {
            esp: 0,
            id: 0,
            name: "kernel",
            state: TaskState::Running,
            priority: 0,
            cell: CellId::INVALID,
            address_space: CapabilityId::INVALID,
            all_next: None,
            all_prev: None,
            run_next: None,
            run_prev: None,
            kernel_stack: None,
            kernel_stack_base: 0,
            kernel_stack_size: 0,
        }
    }

    /// Returns whether the task is currently in the all-tasks
    /// list.
    ///
    /// Used by the scheduler as a debug assertion before pushing
    /// a task onto the list, and by `exit` before removing it.
    /// Not a reliable membership test for a task that is the sole
    /// member of a list: such a task has both links `None`, the
    /// same as a task that is in no list at all. The scheduler
    /// tracks membership by state, not by this method.
    pub fn is_linked_in_all(&self) -> bool {
        self.all_next.is_some() || self.all_prev.is_some()
    }

    /// Returns whether the task is currently in a run queue.
    ///
    /// See [`Task::is_linked_in_all`] for the caveat about
    /// singleton membership.
    pub fn is_linked_in_run(&self) -> bool {
        self.run_next.is_some() || self.run_prev.is_some()
    }

    /// Returns the task's ID.
    pub const fn id(&self) -> u32 {
        self.id
    }

    /// Returns the task's name.
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// Returns the task's current state.
    pub const fn state(&self) -> TaskState {
        self.state
    }

    /// Returns the task's priority.
    pub const fn priority(&self) -> u8 {
        self.priority
    }

    /// Returns the task's capability namespace.
    ///
    /// The cell is created by the task's creator and is stable
    /// for the task's lifetime. It is not revoked when the task
    /// exits; the task's creator is responsible for destroying
    /// the cell if it should not outlive the task.
    pub const fn cell(&self) -> CellId {
        self.cell
    }

    /// Returns the capability to the task's address space.
    ///
    /// For a kernel task this is the kernel address space
    /// capability. For a user cell it is the capability to the
    /// cell's own address space.
    pub const fn address_space(&self) -> CapabilityId {
        self.address_space
    }
}