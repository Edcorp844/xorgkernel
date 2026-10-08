//! Scheduler primitives.
//!
//! This module provides the data structures and policy for
//! scheduling tasks: the [`Task`] type, per-priority run queues
//! built as intrusive linked lists, a [`Scheduler`] that selects
//! the next task to run, and the [`schedule_and_switch`] entry
//! point that actually moves execution between tasks.
//!
//! # Structure
//!
//! The scheduler's state lives in [`SCHEDULER`], a single
//! instance placed in a static. Like the capability core, it is
//! `const`-constructed and reached through [`scheduler_mut`]. The
//! safety contract is the same: the kernel is single-threaded and
//! the scheduler is only touched from contexts that cannot
//! preempt each other. See `capability/mod.rs` for the full
//! discussion of why a static-with-a-discipline was chosen over a
//! lock.
//!
//! # The bootstrap context
//!
//! Before the first task is created, the kernel runs directly:
//! there is no `Task` whose saved registers describe the current
//! CPU state, and no address space capability to activate. The
//! first call to [`schedule_and_switch`] must save the current
//! registers somewhere and load the first task's registers from
//! its kernel stack.
//!
//! [`BOOTSTRAP_TASK`] provides the "somewhere". It is a `Task`
//! that is never scheduled, never reaped, and never observed by
//! anything except `switch_context`. Its only purpose is to give
//! `switch_context` a valid `from` pointer on the first switch.
//! After that switch, the bootstrap context is dead: the kernel
//! runs as the idle task or as one of the tasks it created, and
//! the bootstrap task's saved registers are never restored.
//!
//! # Address-space switching
//!
//! [`schedule_and_switch`] resolves the outgoing and incoming
//! tasks, checks whether the incoming task's address space
//! differs from the one currently active, and activates it if
//! so. The check is a CR3 comparison, not a capability
//! comparison: the scheduler asks the fabric for the page
//! directory address behind the incoming task's capability, and
//! compares it to the value already in CR3.
//!
//! This is deliberate. Two capabilities that name the same
//! address space (for example, the kernel address space
//! capability and a derived read-only capability to it) should
//! not cause a CR3 reload, because the page directory is the
//! same and the reload would only flush the TLB. Comparing page
//! directory addresses catches that case and skips the write.
//!
//! For kernel tasks today, every task's address space is the
//! kernel address space, so the comparison always matches and CR3
//! is never reloaded on a context switch. When user cells land,
//! their tasks will carry capabilities to their own address
//! spaces, and the write will happen exactly when it should.

pub mod list;
pub mod scheduler;
pub mod task;

use crate::capability::capability::CapabilityId;

use scheduler::Scheduler;
use task::Task;

/// The kernel's single scheduler.
///
/// Constructed at load time by `Scheduler::new`. Its run queues
/// and all-tasks list are empty; its `current` field is 0, which
/// the scheduler interprets as "no task is running, the kernel is
/// running directly".
///
/// Access is through [`scheduler_mut`]. See the module
/// documentation for the safety contract.
static mut SCHEDULER: Scheduler = Scheduler::new();

/// The kernel's bootstrap context.
///
/// This is a `Task` that represents the execution context of
/// `kernel_main` before the first real task is switched to. It
/// exists so that `switch_context` has a valid `from` pointer on
/// the first switch: the routine saves the kernel's current
/// callee-saved registers into `BOOTSTRAP_TASK.esp`, then loads
/// the first task's registers from that task's kernel stack.
///
/// After the first switch, `BOOTSTRAP_TASK` is never used again.
/// It is not in the all-tasks list, not in any run queue, and
/// never scheduled. Its `esp` field holds the saved registers of
/// the kernel's original context, which are never restored. If
/// for some reason the kernel were to switch back to it, the
/// effect would be to resume execution after the first
/// `schedule_and_switch` call in `kernel_main`, which would then
/// return immediately and fall through to the unreachable loop at
/// the end of `kernel_main`. This never happens because no
/// scheduler path can select `BOOTSTRAP_TASK`: it has no ID that
/// `task_ptr` can resolve.
static mut BOOTSTRAP_TASK: Task = Task::bootstrap();

/// Initializes the scheduler.
///
/// Prints the scheduler's configuration. The scheduler's state is
/// already constructed by the time this runs: `Scheduler::new` is
/// `const`, so the run queues and all-tasks list are fully
/// initialized before `kernel_main` is entered.
///
/// Must be called after the kernel heap is available, because
/// task creation allocates kernel stacks from the heap. In
/// `kernel_main` this happens after `memory::heap::init`, after
/// the test suite has run, and before the first task is created.
pub fn init() {
    println!("Initializing scheduler...");
    println!("  Priorities: {}", task::NUM_PRIORITIES);
}

/// Returns a mutable reference to the global scheduler.
///
/// # Safety
///
/// The caller must ensure that no other call to `scheduler_mut`
/// is live at the same time, and that no interrupt handler can
/// preempt the current context while the reference is held. See
/// the module documentation for the conditions under which this
/// holds, and `capability/mod.rs` for the same discussion applied
/// to the fabric.
///
/// The function is safe because the discipline is enforced by
/// convention rather than by the type system. It is the caller's
/// responsibility to preserve it.
pub fn scheduler_mut() -> &'static mut Scheduler {
    unsafe { &mut *core::ptr::addr_of_mut!(SCHEDULER) }
}

/// Returns a mutable pointer to the bootstrap task.
///
/// Used by [`schedule_and_switch`] to obtain a valid `from`
/// pointer on the first switch, and by the boot sequence to
/// prime the bootstrap context if any priming is ever needed.
/// The pointer is stable for the lifetime of the kernel: the
/// bootstrap task is a static and is never freed.
pub fn bootstrap_task_ptr() -> *mut Task {
    core::ptr::addr_of_mut!(BOOTSTRAP_TASK)
}

unsafe extern "C" {
    /// Saves the outgoing task's callee-saved registers and loads
    /// the incoming task's.
    ///
    /// Defined in `context.S`. See that file for the register
    /// layout and the exact contract.
    unsafe fn switch_context(from: *mut Task, to: *const Task);
}

/// Selects the next task to run and switches to it.
///
/// The current task is re-enqueued on its priority's run queue
/// before scheduling, so that it will be picked again on a future
/// tick or yield. Then the scheduler selects the highest-priority
/// ready task, activates its address space if that differs from
/// the currently-active one, and switches to it.
///
/// If no task is ready, or if the selected task is the same as
/// the one that was already running, this returns without
/// switching.
///
/// Called from the timer interrupt handler on the way out, and
/// from [`yield_task`] when a task voluntarily gives up the CPU.
///
/// # Phases
///
/// The function is split into three phases so that no borrow of
/// the scheduler is live across the fabric call or the context
/// switch:
///
/// 1. **Select.** Borrow the scheduler, re-enqueue the outgoing
///    task, select the next one, and gather the raw pointers and
///    the incoming task's address-space capability. All of the
///    borrowed data is copied out before this phase ends, so the
///    scheduler borrow is released when the block closes.
///
/// 2. **Activate.** Resolve the incoming task's address space
///    through the fabric, compare its page directory to the one
///    currently in CR3, and write CR3 if they differ. This phase
///    borrows the fabric, not the scheduler.
///
/// 3. **Switch.** Call `switch_context` with the two raw
///    pointers. No borrow of either the scheduler or the fabric
///    is live.
///
/// The split matters because `switch_context` never returns: it
/// saves the outgoing context and immediately begins executing
/// the incoming one. Any reference held across it would have its
/// lifetime extended into code that is not the caller, which the
/// borrow checker cannot express and which would be unsound if it
/// could.
///
/// # Safety
///
/// Must be called with interrupts disabled, or from a context
/// where the scheduler's state cannot be observed concurrently.
/// The current implementation is single-CPU, so this always
/// holds. The function additionally requires that the scheduler's
/// internal state (the all-tasks list and the run queues) is
/// consistent, which is an invariant maintained by every method
/// on `Scheduler`.
pub unsafe fn schedule_and_switch() {
    // ---- Phase 1: select. ----

    let (from_ptr, to_ptr, to_as_cap) = {
        let sched = scheduler_mut();

        // Capture the current task ID before scheduling.
        // `schedule` overwrites `sched.current` with the newly
        // selected task, so we need to read the previous value
        // first.
        let previous_id = sched.current();

        // If there is a current task, put it back on its run
        // queue so it can be scheduled again. The bootstrap
        // context (previous_id == 0) is not a real task and is
        // not re-enqueued.
        if previous_id != 0 {
            sched.make_ready(previous_id);
        }

        // Select the next task. This updates `sched.current`.
        let next_id = match sched.schedule() {
            Some(id) => id,
            None => return,
        };

        // If the scheduled task is the same as the one that was
        // already running, there is nothing to do. Returning is
        // correct: the caller's context is already the one the
        // scheduler wants to run.
        //
        // This can happen when the outgoing task is the only
        // ready task at its priority level: re-enqueueing it
        // makes it the front of its own queue, and `schedule`
        // selects it again.
        if previous_id == next_id {
            return;
        }

        // Determine the raw task pointers.
        //
        // `previous_id == 0` means the kernel was running
        // directly, before any task was scheduled; in that case
        // the "from" context is the bootstrap task. The bootstrap
        // task's address space is invalid, but we never activate
        // it: it is the *from* context, and only the *to* context
        // determines which address space to switch into.
        let from_ptr = if previous_id == 0 {
            bootstrap_task_ptr()
        } else {
            sched.task_ptr(previous_id)
        };

        let to_ptr = sched.task_ptr(next_id);

        // `to_ptr` is non-null because `schedule` selected
        // `next_id` from the all-tasks list, so the ID names a
        // live task. The null check is defensive: if the
        // scheduler's invariants were ever violated, switching to
        // a null task would fault in `switch_context` with no
        // diagnostic. Returning is safer.
        if to_ptr.is_null() {
            return;
        }

        let to_as_cap = unsafe { (*to_ptr).address_space };

        (from_ptr, to_ptr, to_as_cap)
    };
    // ---- Phase 2: update the TSS's kernel stack pointer. ----
    //
    // The TSS's `esp0` field is what the CPU loads into ESP when
    // a privilege transition from CPL 3 to CPL 0 occurs. It must
    // point at the *incoming* task's kernel stack top, or the
    // first trap from user mode will push its frame onto the
    // previous task's kernel stack.
    //
    // The top is `base + size`. `Task::create` lays out the
    // initial frame below the top, so the top is the address the
    // CPU should point at on entry.
    //
    // This runs on every switch, even for kernel-to-kernel
    // switches. Kernel tasks never trap from CPL 3, so the value
    // is not used in that case, but keeping it current means a
    // task that is promoted to user mode later will have the
    // right `esp0` from the moment of its first trap.

    {
        let to = unsafe { &*to_ptr };
        let stack_top = to.kernel_stack_base + to.kernel_stack_size;
        crate::cpu::tss::set_kernel_stack(stack_top);
    }

    // ---- Phase 3: activate the target address space if needed. ----

    if to_as_cap != CapabilityId::INVALID {
        let core = crate::capability::core_mut();

        if let Some(aspace) = core.address_space(to_as_cap) {
            let target_pd = aspace.page_directory();
            let current_pd = crate::cpu::control::read_cr3();

            if target_pd != current_pd {
                unsafe {
                    crate::cpu::control::write_cr3(target_pd);
                }
            }
        }
    }

    // ---- Phase 4: register switch. ----

    unsafe {
        switch_context(from_ptr, to_ptr);
    }
}

/// Voluntarily yields the CPU to the next ready task.
///
/// The current task is re-enqueued at its priority by
/// [`schedule_and_switch`], then the scheduler picks the next
/// task to run. If no other task is ready at the same priority
/// or above, the current task is selected again and this function
/// returns immediately.
///
/// This is the cooperative counterpart to the timer tick. A task
/// that is about to block on something, or that has finished a
/// unit of work and wants to be fair to its peers, calls this
/// instead of spinning.
///
/// # Difference from the timer tick
///
/// The timer handler calls [`schedule_and_switch`] directly. A
/// task calls `yield_task`, which is the same call wrapped in a
/// name that says "this is cooperative". The two paths are
/// identical in effect; the distinction is documentation.
pub fn yield_task() {
    unsafe {
        schedule_and_switch();
    }
}
