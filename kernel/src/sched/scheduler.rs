//! The task scheduler.
//!
//! The scheduler owns every live task and manages two kinds of
//! lists:
//!
//! - **Run queues.** One `IntrusiveList` per priority level. A
//!   task in a run queue is in the `Ready` state.
//! - **All tasks.** A single list containing every live task,
//!   regardless of state. Used for lookup by ID.
//!
//! There is no fixed-size array. The number of tasks is bounded
//! only by the size of the kernel heap, which is where the kernel
//! stacks come from.
//!
//! # Scheduling policy
//!
//! The scheduler picks the highest-priority non-empty run queue
//! and pops the task at its front. The `active` bitmask tracks
//! which priorities have tasks, so selection is O(1): a single
//! `leading_zeros` instruction finds the highest set bit.
//!
//! Within a priority level, tasks run round-robin: a task that is
//! re-enqueued (by a timer tick or a voluntary yield) goes to the
//! back of its queue, so the next task at the same priority runs
//! before it does.
//!
//! # Context switching
//!
//! [`crate::sched::schedule_and_switch`] selects the next task to
//! run and, if it differs from the current task, calls
//! `switch_context` to move execution to it.
//!
//! The scheduler's `current` field tracks which task is running.
//! Before any tasks are created, `current == 0`, meaning "the
//! kernel is running directly, not as a task". The first
//! `schedule_and_switch` transitions from this state into the
//! first scheduled task.
//!
//! # Fabric linkage
//!
//! `Scheduler::create` takes a [`CellId`] and a
//! [`CapabilityId`] alongside the usual task parameters, and
//! stores them on the new [`Task`]. See `task.rs`'s module
//! documentation for what the two fields mean.
//!
//! The scheduler does not dereference the address-space
//! capability. It stores it, hands it back through `Task`, and
//! leaves the actual activation to `schedule_and_switch`, which
//! resolves the capability through the fabric and reloads CR3 if
//! the target task's address space differs from the currently
//! active one.

use alloc::boxed::Box;

use crate::capability::capability::CapabilityId;
use crate::capability::cell::CellId;

use super::list::IntrusiveList;
use super::task::{NUM_PRIORITIES, Task, TaskState};

/// Byte offset of the `all_next` link in `Task`.
const ALL_NEXT: usize = core::mem::offset_of!(Task, all_next);

/// Byte offset of the `all_prev` link in `Task`.
const ALL_PREV: usize = core::mem::offset_of!(Task, all_prev);

/// Byte offset of the `run_next` link in `Task`.
const RUN_NEXT: usize = core::mem::offset_of!(Task, run_next);

/// Byte offset of the `run_prev` link in `Task`.
const RUN_PREV: usize = core::mem::offset_of!(Task, run_prev);

/// The scheduler.
pub struct Scheduler {
    /// One run queue per priority level.
    ///
    /// A task is in `run_queues[task.priority]` exactly when its
    /// state is `Ready`. The invariant is maintained by
    /// `make_ready` and `make_blocked`; `schedule` and `exit`
    /// update the queue contents but do not violate the
    /// invariant.
    run_queues: [IntrusiveList; NUM_PRIORITIES],

    /// All live tasks, in any state.
    ///
    /// This list owns the boxes: every `Task` that exists is
    /// reachable from it, and every `Task` in it is owned by the
    /// scheduler. `exit` is the only operation that removes a
    /// task from this list, and it drops the `Box` immediately
    /// afterward.
    all_tasks: IntrusiveList,

    /// Bitmask of non-empty run queues.
    ///
    /// Bit `i` is set iff `run_queues[i]` is non-empty. The
    /// scheduler uses `leading_zeros` on this mask to find the
    /// highest-priority ready task in O(1). The mask is
    /// maintained by every method that changes a run queue's
    /// length.
    active: u32,

    /// ID of the currently running task, or 0 if the kernel is
    /// running directly without a task.
    ///
    /// The sentinel 0 is safe because the scheduler never assigns
    /// ID 0 to a real task: `next_id` starts at 1 and wraps to 1.
    current: u32,

    /// Next task ID to assign.
    ///
    /// Increments monotonically. Wraps to 1 rather than 0 so that
    /// 0 remains reserved as the "no current task" sentinel.
    next_id: u32,
}

impl Scheduler {
    /// Creates an empty scheduler.
    ///
    /// The run queues, the all-tasks list, and the `active`
    /// bitmask are all zero-initialized. `current` is 0, meaning
    /// "the kernel is running directly, not as a task".
    ///
    /// `Scheduler::new` is `const` so that the kernel's single
    /// scheduler can be placed in a `static` and constructed at
    /// load time.
    pub const fn new() -> Self {
        Self {
            run_queues: [const { IntrusiveList::new(RUN_NEXT, RUN_PREV) }; NUM_PRIORITIES],
            all_tasks: IntrusiveList::new(ALL_NEXT, ALL_PREV),
            active: 0,
            current: 0,
            next_id: 1,
        }
    }

    /// Creates a task and registers it with the scheduler.
    ///
    /// The task is placed in the `Ready` state and enqueued on
    /// its priority's run queue. It becomes eligible for
    /// selection by the next `schedule` call.
    ///
    /// # Arguments
    ///
    /// - `name`: a human-readable name for diagnostics.
    /// - `entry`: the function the task starts executing. See
    ///   [`Task::create`] for why this returns `!`.
    /// - `priority`: the task's priority, less than
    ///   [`NUM_PRIORITIES`].
    /// - `stack_size`: the size of the task's kernel stack, in
    ///   bytes.
    /// - `cell`: the task's capability namespace. Every real task
    ///   has a cell; the caller is responsible for creating it
    ///   and for populating it with the capabilities the task
    ///   should hold. The scheduler does not create or populate
    ///   the cell, and does not check its contents. This
    ///   separation keeps the scheduler's task-creation path free
    ///   of fabric dependencies.
    /// - `address_space`: the capability to the task's address
    ///   space. For a kernel task this is the kernel address
    ///   space capability; for a user cell it is the capability
    ///   to the cell's own address space. The scheduler does not
    ///   dereference the capability. It stores it for the
    ///   context-switch path, which resolves it through the
    ///   fabric when a switch to a different address space is
    ///   required.
    ///
    /// # Return value
    ///
    /// Returns the task's ID on success. Returns `None` if the
    /// heap cannot provide the kernel stack, or if any of
    /// [`Task::create`]'s preconditions is violated. In the
    /// latter case the assertion fires before this method
    /// returns, so `None` is only produced by heap exhaustion in
    /// practice.
    pub fn create(
        &mut self,
        name: &'static str,
        entry: fn() -> !,
        priority: u8,
        stack_size: usize,
        cell: CellId,
        address_space: CapabilityId,
    ) -> Option<u32> {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        if self.next_id == 0 {
            self.next_id = 1;
        }

        let task = Box::new(Task::create(
            id,
            name,
            entry,
            priority,
            stack_size,
            cell,
            address_space,
        )?);

        // Leak the box. The scheduler reclaims it on `exit`.
        //
        // Ownership transfer: `Box::into_raw` consumes the box
        // and hands the scheduler a raw pointer. From this point
        // on, the task is owned by the all-tasks list, and
        // `exit` is the only method that may drop it.
        let task_ptr = Box::into_raw(task);

        unsafe {
            // Register in the all-tasks list.
            self.all_tasks.push_back(task_ptr);

            // Enqueue on the run queue.
            let priority_index = priority as usize;
            self.run_queues[priority_index].push_back(task_ptr);
            self.active |= 1 << priority_index;
        }

        Some(id)
    }

    /// Returns a reference to a task by ID.
    ///
    /// Linear in the number of live tasks. Fine for the current
    /// scale; a hash table can replace it later.
    ///
    /// # Safety of the iterator
    ///
    /// The call to `all_tasks.iter()` is unsafe because the
    /// iterator does not enforce that no concurrent mutation
    /// occurs. This is safe because the scheduler is
    /// single-threaded and `task` does not mutate anything.
    pub fn task(&self, id: u32) -> Option<&Task> {
        unsafe { self.all_tasks.iter().find(|t| t.id == id) }
    }

    /// Returns a raw pointer to a task by ID, or null if not
    /// found.
    ///
    /// The pointer is valid until the task is reaped by `exit`.
    /// Callers must not hold the pointer across an `exit` call.
    ///
    /// ID 0 is the sentinel for "no current task" and never names
    /// a real task, so it returns null immediately without
    /// walking the list.
    pub fn task_ptr(&self, id: u32) -> *mut Task {
        if id == 0 {
            return core::ptr::null_mut();
        }
        unsafe {
            for task in self.all_tasks.iter() {
                if task.id == id {
                    return task as *const Task as *mut Task;
                }
            }
        }
        core::ptr::null_mut()
    }

    /// Returns the ID of the currently running task, or 0 if
    /// none.
    ///
    /// `current == 0` means the kernel is running directly, not
    /// as a task. This is true between `kernel_main` and the
    /// first `schedule_and_switch`, and never again afterward.
    pub const fn current(&self) -> u32 {
        self.current
    }

    /// Sets the current task ID.
    ///
    /// Called by `schedule_and_switch` after selecting a task,
    /// and by the bootstrap code when transitioning from
    /// kernel-direct execution to the first task.
    ///
    /// Setting `current` to 0 is only correct during bootstrap;
    /// setting it to 0 after the first task has run would make
    /// the scheduler forget which task's registers are on the
    /// CPU, and the next switch would save them into the wrong
    /// place.
    pub fn set_current(&mut self, id: u32) {
        self.current = id;
    }

    /// Marks a task as `Ready` and enqueues it.
    ///
    /// If the task is already ready, this is a no-op. The no-op
    /// matters because `schedule_and_switch` calls `make_ready`
    /// on the outgoing task before selecting the next one; a task
    /// that was never blocked would otherwise be inserted into
    /// its run queue twice.
    ///
    /// The caller must ensure that `id` names a live task. A
    /// non-existent ID is silently ignored, which is the same
    /// behavior as a task that has already exited: there is
    /// nothing to enqueue.
    pub fn make_ready(&mut self, id: u32) {
        let task_ptr = self.task_ptr(id);
        if task_ptr.is_null() {
            return;
        }

        let task = unsafe { &mut *task_ptr };

        if task.state == TaskState::Ready {
            return;
        }

        task.state = TaskState::Ready;
        let priority_index = task.priority as usize;

        unsafe {
            self.run_queues[priority_index].push_back(task_ptr);
        }
        self.active |= 1 << priority_index;
    }

    /// Marks a task as `Blocked` and removes it from its run
    /// queue.
    ///
    /// If the task is not currently in the `Ready` state, this is
    /// a no-op. Blocking a task that is already blocked, or one
    /// that has exited, has no effect.
    ///
    /// When the last task at a priority level is blocked, the
    /// corresponding bit in `active` is cleared so that the next
    /// `schedule` call does not look at an empty queue.
    pub fn make_blocked(&mut self, id: u32) {
        let task_ptr = self.task_ptr(id);
        if task_ptr.is_null() {
            return;
        }

        let task = unsafe { &mut *task_ptr };

        if task.state != TaskState::Ready {
            return;
        }

        let priority_index = task.priority as usize;

        unsafe {
            self.run_queues[priority_index].remove(task_ptr);
        }

        if self.run_queues[priority_index].is_empty() {
            self.active &= !(1 << priority_index);
        }

        task.state = TaskState::Blocked;
    }

    /// Marks a task as `Dead` and removes it from the scheduler.
    ///
    /// The kernel stack and other resources are released when the
    /// `Box<Task>` is dropped. After this call, the task's ID
    /// no longer names anything, and any raw pointer obtained
    /// from `task_ptr` is dangling.
    ///
    /// # Order of operations
    ///
    /// 1. **Break the task's outstanding borrows.** If the task
    ///    was the lender of a capability over an IPC channel, the
    ///    lender's slot must be reverted before the task's cell
    ///    is dropped. This happens first because it needs the
    ///    cell's borrow state, which is destroyed in step 4.
    ///
    ///    The break is done by the fabric, through
    ///    [`CapabilityCore::break_borrows_for_cell`], which scans
    ///    every channel's ring for `BorrowIn` messages whose
    ///    lender is the exiting task's cell. See that method's
    ///    documentation for the two directions of a borrow and
    ///    which one is handled at task exit.
    ///
    /// 2. If the task is `Ready`, remove it from its run queue
    ///    and clear the `active` bit if the queue becomes empty.
    ///    A task in `Running` or `Blocked` state is already
    ///    absent from every run queue, so no removal is needed.
    ///
    /// 3. Mark the task `Dead`. This is done before removing it
    ///    from the all-tasks list because `Task::is_linked_in_all`
    ///    is not a reliable membership test for a singleton list,
    ///    and the state is the authoritative signal.
    ///
    /// 4. Remove the task from the all-tasks list and drop the
    ///    box. The drop frees the kernel stack. The cell itself
    ///    is *not* dropped here: it is owned by the fabric, not
    ///    by the task, and its lifetime is the fabric's
    ///    responsibility. See the note on cell ownership below.
    ///
    /// # Cell ownership
    ///
    /// The task holds a `CellId`, not a `Cell`. The cell lives in
    /// the fabric's cell table. When a task exits, the fabric is
    /// *not* automatically told to destroy the cell — a cell can
    /// outlive a task (for example, if a parent creates a cell
    /// and hands it to a child, the parent may still hold a
    /// reference to it after the child exits).
    ///
    /// The caller of `exit` is responsible for destroying the
    /// cell if it should not outlive the task. In the current
    /// kernel, no caller destroys cells on task exit; the cells
    /// created in `kernel_main` and in the test suite live for
    /// the kernel's lifetime. When a real `exit` syscall exists,
    /// it will need to decide whether to destroy the exiting
    /// task's cell, and that decision belongs in the syscall
    /// handler, not here.
    ///
    /// # The `current` field
    ///
    /// The `current` field is not touched. If the caller is
    /// reaping the currently-running task (which would mean the
    /// task exited voluntarily via a future `exit` syscall), the
    /// caller must separately arrange for `current` to be
    /// updated, or the next `schedule_and_switch` will try to
    /// save the outgoing context into a freed box. The current
    /// kernel does not reap running tasks; this is a precondition
    /// for using `exit`.
    pub fn exit(&mut self, id: u32) {
        let task_ptr = self.task_ptr(id);
        if task_ptr.is_null() {
            return;
        }

        let task = unsafe { &mut *task_ptr };
        let priority_index = task.priority as usize;

        // ---- Step 1: break the task's outstanding borrows. ----
        //
        // The task's cell may have slots in the `BorrowedOut`
        // state. Each corresponds to a `BorrowIn` message in some
        // channel's ring. Breaking the borrows releases the
        // lender slots back to `Owned` and removes the messages.
        //
        // This must run before the cell's storage is dropped.
        // The cell is owned by the fabric and is not dropped
        // here, but the borrow state lives in the cell, and the
        // fabric's `break_borrows_for_cell` reads it. If the
        // cell is destroyed first (by a future caller that
        // decides to clean up on exit), the borrow state is gone
        // and the borrow cannot be broken.
        let cell = task.cell;

        if cell.is_valid() {
            let core = crate::capability::core_mut();
            let broken = core.break_borrows_for_cell(cell);

            if broken > 0 {
                println!(
                    "scheduler: broke {} borrow(s) for exiting task '{}'",
                    broken, task.name
                );
            }
        }

        // ---- Step 2: remove from the run queue if Ready. ----

        if task.state == TaskState::Ready {
            unsafe {
                self.run_queues[priority_index].remove(task_ptr);
            }
            if self.run_queues[priority_index].is_empty() {
                self.active &= !(1 << priority_index);
            }
        }

        // ---- Step 3: mark Dead. ----

        task.state = TaskState::Dead;

        // ---- Step 4: remove from the all-tasks list and drop. ----

        unsafe {
            self.all_tasks.remove(task_ptr);
            drop(Box::from_raw(task_ptr));
        }
    }

    /// Selects the next task to run.
    ///
    /// Returns the ID of the highest-priority ready task, or
    /// `None` if no task is ready.
    ///
    /// # Selection
    ///
    /// The `active` bitmask has bit `i` set iff `run_queues[i]`
    /// is non-empty. `leading_zeros` gives the number of leading
    /// zero bits; subtracting it from 31 gives the index of the
    /// highest set bit, which is the highest-priority non-empty
    /// queue.
    ///
    /// The selected task is popped from the front of its run
    /// queue, so within a priority level, tasks run in FIFO order.
    /// A task that is re-enqueued goes to the back, which gives
    /// round-robin behavior at each priority.
    ///
    /// # State transition
    ///
    /// The selected task's state is changed from `Ready` to
    /// `Running`, and `current` is updated to its ID. Both
    /// changes are made before this method returns, so a caller
    /// that switches to the returned task sees the correct
    /// scheduler state.
    pub fn schedule(&mut self) -> Option<u32> {
        if self.active == 0 {
            return None;
        }

        // Find the highest set bit.
        //
        // `active` is non-zero (checked above), so `leading_zeros`
        // is in 0..=31 and the subtraction is well-defined.
        let priority_index = (31 - self.active.leading_zeros()) as usize;

        // Pop the front of that queue.
        //
        // The cast is safe because `active` has the corresponding
        // bit set, so the queue is non-empty.
        let task_ptr = unsafe { self.run_queues[priority_index].pop_front() };

        if task_ptr.is_null() {
            return None;
        }

        // Update the active mask if the queue is now empty.
        if self.run_queues[priority_index].is_empty() {
            self.active &= !(1 << priority_index);
        }

        let task = unsafe { &mut *task_ptr };
        task.state = TaskState::Running;
        let id = task.id;
        self.current = id;

        Some(id)
    }
}
