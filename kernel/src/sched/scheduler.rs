//! The task scheduler.
//!
//! The scheduler owns every live task and manages two kinds of
//! lists:
//!
//! - **Run queues.** One `IntrusiveList` per priority level. A task
//!   in a run queue is in the `Ready` state.
//! - **All tasks.** A single list containing every live task,
//!   regardless of state. Used for lookup by ID.
//!
//! There is no fixed-size array. The number of tasks is bounded only
//! by the size of the kernel heap, which is where the kernel stacks
//! come from.
//!
//! # Scheduling policy
//!
//! The scheduler picks the highest-priority non-empty run queue and
//! pops the task at its front. The `active` bitmask tracks which
//! priorities have tasks, so selection is O(1).
//!
//! # Context switching
//!
//! `schedule_and_switch` (in `mod.rs`) selects the next task to run
//! and, if it differs from the current task, calls `switch_context`
//! to move execution to it.
//!
//! The scheduler's `current` field tracks which task is running.
//! Before any tasks are created, `current == 0`, meaning "the
//! kernel is running directly, not as a task". The first
//! `schedule_and_switch` transitions from this state into the first
//! scheduled task.

use alloc::boxed::Box;

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
    run_queues: [IntrusiveList; NUM_PRIORITIES],

    /// All live tasks, in any state. This list owns the boxes.
    all_tasks: IntrusiveList,

    /// Bitmask of non-empty run queues.
    active: u32,

    /// ID of the currently running task, or 0 if the kernel is
    /// running directly without a task.
    current: u32,

    /// Next task ID to assign.
    next_id: u32,
}

impl Scheduler {
    /// Creates an empty scheduler.
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
    /// The task is placed in the `Ready` state and enqueued on its
    /// priority's run queue.
    ///
    /// The `entry` parameter has type `fn() -> !` because a task
    /// must never return. When the task is first scheduled,
    /// `switch_context` returns directly into the entry function
    /// with no return address on the stack; if the function were to
    /// return, the CPU would jump to whatever garbage lies below it.
    /// Declaring the entry point as returning `!` makes the compiler
    /// enforce this at every call site.
    ///
    /// Returns the task's ID, or `None` if the heap cannot provide
    /// the kernel stack.
    pub fn create(
        &mut self,
        name: &'static str,
        entry: fn() -> !,
        priority: u8,
        stack_size: usize,
    ) -> Option<u32> {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        if self.next_id == 0 {
            self.next_id = 1;
        }

        let task = Box::new(Task::create(id, name, entry, priority, stack_size)?);

        // Leak the box. The scheduler reclaims it on `exit`.
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
    pub fn task(&self, id: u32) -> Option<&Task> {
        unsafe { self.all_tasks.iter().find(|t| t.id == id) }
    }

    /// Returns a raw pointer to a task by ID, or null if not found.
    ///
    /// The pointer is valid until the task is reaped by `exit`.
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

    /// Returns the ID of the currently running task, or 0 if none.
    pub const fn current(&self) -> u32 {
        self.current
    }

    /// Sets the current task ID.
    ///
    /// Called by `schedule_and_switch` after selecting a task, and
    /// by the bootstrap code when transitioning from kernel-direct
    /// execution to the first task.
    pub fn set_current(&mut self, id: u32) {
        self.current = id;
    }

    /// Marks a task as `Ready` and enqueues it.
    ///
    /// If the task is already ready, this is a no-op.
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

    /// Marks a task as `Blocked` and removes it from its run queue.
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
    /// `Box<Task>` is dropped.
    pub fn exit(&mut self, id: u32) {
        let task_ptr = self.task_ptr(id);
        if task_ptr.is_null() {
            return;
        }

        let task = unsafe { &mut *task_ptr };
        let priority_index = task.priority as usize;

        if task.state == TaskState::Ready {
            unsafe {
                self.run_queues[priority_index].remove(task_ptr);
            }
            if self.run_queues[priority_index].is_empty() {
                self.active &= !(1 << priority_index);
            }
        }

        task.state = TaskState::Dead;

        unsafe {
            self.all_tasks.remove(task_ptr);
            drop(Box::from_raw(task_ptr));
        }
    }

    /// Selects the next task to run.
    ///
    /// Returns the ID of the highest-priority ready task, or `None`
    /// if no task is ready.
    pub fn schedule(&mut self) -> Option<u32> {
        if self.active == 0 {
            return None;
        }

        // Find the highest set bit.
        let priority_index = (31 - self.active.leading_zeros()) as usize;

        // Pop the front of that queue.
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
