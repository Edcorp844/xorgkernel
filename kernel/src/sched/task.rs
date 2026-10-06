//! Schedulable tasks.
//!
//! A `Task` is a kernel execution context: a saved register state, a
//! kernel stack, and the metadata the scheduler needs to decide when
//! the task runs next.
//!
//! # Intrusive links
//!
//! Tasks carry two independent pairs of intrusive links:
//!
//! - `all_next` / `all_prev` are used by the scheduler's all-tasks
//!   list, which contains every live task for its entire lifetime.
//! - `run_next` / `run_prev` are used by the current run queue when
//!   the task is in the `Ready` state, and are cleared otherwise.
//!
//! Having two pairs means a task can be in the all-tasks list and a
//! run queue at the same time. This is required: the scheduler must
//! be able to look up a task by ID (via the all-tasks list) while
//! the task is also queued for execution.
//!
//! The layout and offsets of these fields are fixed at compile time.
//! `scheduler.rs` uses `core::mem::offset_of!` to compute the
//! offsets passed to `IntrusiveList::new`, so the list code does not
//! depend on the field order and cannot drift from the struct
//! definition.
//!
//! # Assembly contract
//!
//! The `esp` field must be at offset 0 of the struct. The
//! `switch_context` routine reads and writes this field directly
//! through a raw pointer cast to `*mut u32`. `#[repr(C)]` guarantees
//! the field order.

use core::ptr::NonNull;

use alloc::boxed::Box;

// ---------------------------------------------------------------------
// Task state
// ---------------------------------------------------------------------

/// The scheduling state of a task.
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
pub const NUM_PRIORITIES: usize = 8;

/// Priority of the idle task.
pub const PRIORITY_IDLE: u8 = 0;

/// Priority of background kernel work.
pub const PRIORITY_LOW: u8 = 2;

/// Priority of ordinary kernel tasks.
pub const PRIORITY_NORMAL: u8 = 4;

/// Priority of driver and latency-sensitive tasks.
pub const PRIORITY_HIGH: u8 = 5;

/// Priority of real-time tasks. The highest level.
pub const PRIORITY_REALTIME: u8 = 7;

// ---------------------------------------------------------------------
// Task
// ---------------------------------------------------------------------

/// A schedulable kernel execution context.
#[repr(C)]
pub struct Task {
    // ---- Assembly-visible fields. ----
    /// Saved ESP.
    ///
    /// Must be at offset 0. `switch_context` reads and writes this
    /// field through a raw pointer.
    pub esp: u32,

    // ---- Identity. ----
    /// Stable identifier.
    pub id: u32,

    /// Human-readable name for diagnostics.
    pub name: &'static str,

    /// Current scheduling state.
    pub state: TaskState,

    /// Priority. Higher values are more urgent.
    pub priority: u8,

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
    /// `None` for the bootstrap context, which runs on the initial
    /// stack. Every real task has `Some(...)`.
    pub kernel_stack: Option<Box<[u8]>>,

    /// Base virtual address of the kernel stack.
    pub kernel_stack_base: u32,

    /// Size of the kernel stack, in bytes.
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
    /// Returns `None` if the heap cannot provide the requested
    /// stack size.
    pub fn create(
        id: u32,
        name: &'static str,
        entry: fn() -> !,
        priority: u8,
        stack_size: usize,
    ) -> Option<Self> {
        assert!(
            priority < NUM_PRIORITIES as u8,
            "task: priority out of range"
        );
        assert!(stack_size >= 256, "task: stack size too small");

        // Allocate the kernel stack from the heap.
        let kernel_stack = alloc::vec![0u8; stack_size].into_boxed_slice();

        let base = kernel_stack.as_ptr() as u32;
        let top = base + stack_size as u32;

        // Lay out the initial frame at the top of the stack.
        //
        // Writing directly into the stack bytes is safe because the
        // stack was just allocated and nothing else has touched it.
        let mut sp = top;
        unsafe {
            // Return address: the entry point.
            sp -= 4;
            core::ptr::write(sp as *mut u32, entry as usize as u32);

            // Saved callee-saved registers.
            //
            // The order here must match the pop order in
            // `switch_context`: the routine pops EBX, ESI, EDI,
            // EBP, so the stack from lowest to highest must be
            // EBX, ESI, EDI, EBP.
            sp -= 4;
            core::ptr::write(sp as *mut u32, 0); // EBP
            sp -= 4;
            core::ptr::write(sp as *mut u32, 0); // EDI
            sp -= 4;
            core::ptr::write(sp as *mut u32, 0); // ESI
            sp -= 4;
            core::ptr::write(sp as *mut u32, 0); // EBX
        }

        Some(Self {
            esp: sp,
            id,
            name,
            state: TaskState::Ready,
            priority,
            all_next: None,
            all_prev: None,
            run_next: None,
            run_prev: None,
            kernel_stack: Some(kernel_stack),
            kernel_stack_base: base,
            kernel_stack_size: stack_size as u32,
        })
    }

    /// Creates a placeholder task for the kernel bootstrap context.
    ///
    /// The bootstrap context represents `kernel_main` running on the
    /// initial stack, before any real task exists. It is passed as
    /// the `from` argument to the first `switch_context` call, so
    /// that the routine has somewhere to save the kernel's own
    /// registers.
    ///
    /// After the first switch, the bootstrap context is never used
    /// again: the kernel runs as the idle task or as one of the
    /// tasks it created. It is never scheduled, never observed by
    /// anything other than `switch_context`, and never reaped.
    ///
    /// The `esp` field is zero and is overwritten by
    /// `switch_context` on the first call.
    pub const fn bootstrap() -> Self {
        Self {
            esp: 0,
            id: 0,
            name: "kernel",
            state: TaskState::Running,
            priority: 0,
            all_next: None,
            all_prev: None,
            run_next: None,
            run_prev: None,
            kernel_stack: None,
            kernel_stack_base: 0,
            kernel_stack_size: 0,
        }
    }

    /// Returns whether the task is currently in the all-tasks list.
    pub fn is_linked_in_all(&self) -> bool {
        self.all_next.is_some() || self.all_prev.is_some()
    }

    /// Returns whether the task is currently in a run queue.
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
}
