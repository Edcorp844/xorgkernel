//! Scheduler primitives.
//!
//! This module provides the data structures and policy for
//! scheduling tasks: the `Task` type, per-priority run queues built
//! as intrusive linked lists, a `Scheduler` that selects the next
//! task to run, and the `schedule_and_switch` entry point that
//! actually moves execution between tasks.

pub mod list;
pub mod scheduler;
pub mod task;

use scheduler::Scheduler;
use task::Task;

/// The kernel's single scheduler.
static mut SCHEDULER: Scheduler = Scheduler::new();

/// The kernel's bootstrap context.
///
/// This is a `Task` structure that wraps the execution context of
/// `kernel_main` before the first real task is switched to. It
/// exists so that `switch_context` can save the kernel's own
/// registers on the initial stack and return into the idle task
/// the first time the scheduler runs.
///
/// After the first switch, this context is never used again: the
/// kernel runs as the idle task or as one of the tasks it created.
static mut BOOTSTRAP_TASK: Task = Task::bootstrap();

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

/// Returns a mutable pointer to the bootstrap task.
///
/// Used by `schedule_and_switch` and by the boot sequence.
pub fn bootstrap_task_ptr() -> *mut Task {
    core::ptr::addr_of_mut!(BOOTSTRAP_TASK)
}

unsafe extern "C" {
    unsafe fn switch_context(from: *mut Task, to: *const Task);
}

/// Selects the next task to run and switches to it.
///
/// The current task is re-enqueued on its priority's run queue
/// before scheduling, so that it will be picked again on a future
/// tick or yield. Then the scheduler selects the highest-priority
/// ready task and switches to it.
///
/// If no task is ready, or if the selected task is the same as the
/// one that was already running, this returns without switching.
///
/// Called from the timer interrupt handler on the way out, and from
/// `yield_task` when a task voluntarily gives up the CPU.
///
/// # Safety
///
/// Must be called with interrupts disabled, or from a context where
/// the scheduler's state cannot be observed concurrently. The
/// current implementation is single-CPU, so this always holds.
pub unsafe fn schedule_and_switch() {
    let (from, to) = {
        let sched = scheduler_mut();

        // Capture the current task ID before scheduling. `schedule`
        // overwrites `sched.current` with the newly selected task,
        // so we need to read the previous value first.
        let previous_id = sched.current();

        // If there is a current task, put it back on its run queue
        // so it can be scheduled again. The bootstrap context
        // (previous_id == 0) is not a real task and is not
        // re-enqueued.
        if previous_id != 0 {
            sched.make_ready(previous_id);
        }

        // Select the next task. This updates `sched.current`.
        let next_id = match sched.schedule() {
            Some(id) => id,
            None => return,
        };

        // If the scheduled task is the same as the one that was
        // already running, there is nothing to do.
        if previous_id == next_id {
            return;
        }

        // Determine the task pointers. `previous_id == 0` means the
        // kernel was running directly, before any task was
        // scheduled; in that case the "from" context is the
        // bootstrap task.
        let from = if previous_id == 0 {
            bootstrap_task_ptr()
        } else {
            sched.task_ptr(previous_id)
        };

        let to = sched.task_ptr(next_id);

        (from, to)
    };

    unsafe {
        switch_context(from, to);
    }
}

/// Voluntarily yields the CPU to the next ready task.
///
/// The current task is re-enqueued at its priority by
/// `schedule_and_switch`, then the scheduler picks the next task to
/// run.
pub fn yield_task() {
    unsafe {
        schedule_and_switch();
    }
}
