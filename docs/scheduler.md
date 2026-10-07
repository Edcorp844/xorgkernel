# Scheduler Design

This document describes the kernel's scheduler: what it does,
how it is structured, and why it was designed the way it was.

For the code, see `kernel/src/sched/`. For the invariants the
scheduler maintains, see `substrate-contract.md`. For the boot
sequence that brings the scheduler online, see
`boot-sequence.md`.

## Purpose

The scheduler decides which task runs next on the CPU. It has
three responsibilities:

1. Maintain the set of live tasks and their states.
2. Select the next task to run according to a policy.
3. Perform the context switch that moves execution from the
   current task to the next.

It does not create tasks, destroy them, or decide when they
should block or wake. Those are the caller's responsibilities.
The scheduler's job is policy and mechanism, not lifecycle
management.

## Design goals

**Preemptive.** A task runs until the timer interrupts it or
it voluntarily yields. It cannot hold the CPU indefinitely.
This is required for any system that runs untrusted code, and
for any system with latency-sensitive work.

**Priority-based.** Higher-priority tasks run before
lower-priority tasks. Within a priority level, tasks share the
CPU fairly. This is required for real-time work, where a
missed deadline is a failure.

**O(1) selection.** The scheduler does not scan a list of
tasks to decide which runs next. It looks at a bitmask and
pops a queue. This is required for predictable latency.

**Bounded memory.** A task costs a `Task` structure plus its
kernel stack. Both come from the heap. There is no fixed cap
on the number of tasks; the cap is the heap's size. This is
required for a system that runs an arbitrary number of cells.

**Intrusive lists.** The scheduler does not allocate separate
list nodes. Each task carries its own links. This avoids
allocation on the hot path and makes insertion and removal
O(1) with no failure mode.

## Structure

The scheduler is a single `Scheduler` structure, stored as a
static in `sched/mod.rs`. It contains three things.

**One run queue per priority level.** There are eight
priorities, numbered 0 (idle) through 7 (real-time). Each
priority has an `IntrusiveList` of `Task`s in the `Ready`
state. A task is in exactly one run queue when it is ready,
and in none when it is not.

**An all-tasks list.** A single `IntrusiveList` containing
every live task, regardless of state. Used for lookup by ID.
Every live task is in this list for its entire lifetime.

**A small amount of bookkeeping.** A bitmask of non-empty run
queues, the ID of the current task, and the next task ID to
assign.

## Task states

A task is always in one of four states.

- **Ready.** The task is runnable and sits in a run queue.
- **Running.** The task is currently executing on the CPU.
  There is at most one running task at a time on a single-CPU
  system.
- **Blocked.** The task is waiting for an event. It is not in
  any run queue.
- **Dead.** The task has exited and is waiting to be reaped.

Only the transition `Ready -> Running` and `Running -> Ready`
are managed by the scheduler itself. The other transitions are
managed by the caller: `make_blocked` for `Running ->
Blocked`, `make_ready` for `Blocked -> Ready`, and `exit` for
any state to `Dead`.

## Scheduling policy

The scheduler selects the highest-priority non-empty run queue
and pops its front element. This is a strict priority policy:
a task at priority 5 always runs before a task at priority 4,
regardless of how long the lower-priority task has been
waiting.

Within a priority level, tasks share the CPU in a round-robin
fashion. A task that has just run is re-enqueued at the tail
of its queue, so the next task at that priority runs.

Strict priority has a known problem: priority inversion. If a
low-priority task holds a lock that a high-priority task
wants, the high-priority task blocks behind the low-priority
one, and any medium-priority task can preempt the low-priority
task indefinitely. The classic case is the Mars Pathfinder
mission, where the priority inversion caused repeated
watchdog resets.

The solution is priority inheritance: when a high-priority
task blocks on a lock held by a lower-priority task, the
holder temporarily inherits the higher priority. The
scheduler currently does not implement this, because there
are no locks yet. It will be added when the first blocking
synchronization primitive is introduced.

## Selection

`schedule` runs in O(1) time.

1. If the active bitmask is zero, no task is ready. Return
   `None`.
2. Find the highest set bit in the bitmask. This is done with
   a single `leading_zeros` instruction.
3. Pop the front element of that priority's run queue.
4. If the queue is now empty, clear its bit in the mask.
5. Set the task's state to `Running`, update the scheduler's
   `current` field, and return the task's ID.

Because the bitmask is a single `u32` and the queue pop is a
linked-list operation, the whole selection is a fixed number
of instructions regardless of how many tasks are live.

## Context switching

`schedule_and_switch` is the entry point that callers use. It
performs the selection and, if a different task was selected,
calls `switch_context`.

1. Capture the current task ID. This is read **before**
   selection, because selection updates the `current` field.
2. If the current task ID is not zero, re-enqueue the current
   task on its priority's run queue. Zero means "the kernel is
   running directly, not as a task"; the bootstrap context is
   not re-enqueued.
3. Select the next task.
4. If the selected task is the same as the current task,
   return without switching. This happens when the current
   task is the only ready task at its priority.
5. Determine the two task pointers. If the current task ID is
   zero, the "from" pointer is the bootstrap task. Otherwise
   it is the task with the current ID. The "to" pointer is the
   task with the newly selected ID.
6. Call `switch_context(from, to)`.

`switch_context` is a small assembly routine in
`sched/context.S`. It saves the callee-saved registers of the
current task on its stack, stores the resulting ESP into
`from->esp`, loads `to->esp` into ESP, restores the
callee-saved registers of the new task, and returns into it.

The routine does not return to its caller. It returns into
whichever task `to` refers to, from that task's saved
instruction pointer.

## The first switch

Before the first switch, the kernel runs directly on the
bootstrap stack, in `kernel_main`. There is no `Task` for this
execution context.

To make the first switch work, the scheduler declares a
`BOOTSTRAP_TASK` static. Its `esp` field starts at zero. When
`schedule_and_switch` is called for the first time, the
current task ID is zero, so the "from" pointer is set to the
bootstrap task.

`switch_context` writes the kernel's current ESP into
`bootstrap->esp` before switching to the selected task. This
means that if the bootstrap context were ever restored,
`kernel_main` would resume from the switch call. It never is.
The bootstrap context is used exactly once and then abandoned.

## The task structure

A `Task` has these fields, in order:

- `esp` — the saved stack pointer. Must be at offset 0,
  because `switch_context` reads it directly through a raw
  pointer.
- `id` — a stable identifier.
- `name` — a human-readable string for diagnostics.
- `state` — one of the four states above.
- `priority` — 0 through 7.
- `all_next`, `all_prev` — intrusive links for the all-tasks
  list.
- `run_next`, `run_prev` — intrusive links for the current run
  queue.
- `kernel_stack` — the task's kernel stack, owned by the task
  and freed when the task is dropped. `None` for the bootstrap
  task, which runs on the initial stack.
- `kernel_stack_base` — cached for computing stack addresses.
- `kernel_stack_size` — cached for the same reason.

The two pairs of links are independent. A task can be in the
all-tasks list and a run queue simultaneously, which is
required: the scheduler must be able to look up a task by ID
while the task is queued for execution.

## Intrusive lists

The scheduler uses intrusive doubly-linked lists, not `Vec`
or `LinkedList`. Each task carries its own links. The list is
just a pair of head and tail pointers.

The list implementation in `sched/list.rs` is parameterized by
the byte offsets of the two link fields. `offset_of!` computes
these at compile time. This lets one list implementation serve
both the all-tasks list and the run queues, with different
link pairs.

The list operations are `unsafe fn` and take a `*mut Task`
rather than a `&mut Task`. This is necessary: the scheduler
holds task pointers in three places at once (the all-tasks
list, a run queue, and the `current` field), and the borrow
checker cannot prove they do not alias.

The caller contract for the list is:

- `push_back` requires that the task is not already in a list
  using the same link pair.
- `remove` requires that the task is a member of the list.
- The list does not check either condition, because a task
  that is the sole element of a list has both links null, so
  "are the links set?" is not a reliable membership test.

The scheduler enforces these conditions via state invariants:
a task is in a run queue if and only if its state is `Ready`,
and every live task is in the all-tasks list.

## The scheduler's statics

The scheduler lives in a single `static mut SCHEDULER:
Scheduler`. All access goes through `scheduler_mut()`, which
returns a `&'static mut Scheduler`.

This is the same pattern the frame allocator and the
capability core use. It is not thread-safe and does not
pretend to be. The kernel is single-CPU, and the scheduler is
only called from contexts where it has exclusive access:
`kernel_main` during boot, `yield_task` from a running task,
and `schedule_and_switch` from the timer handler. All three
contexts are serialized by the fact that only one task runs at
a time.

When SMP arrives, the scheduler will need per-CPU structures
or a lock around the global one. Neither is currently present.

## What the scheduler does not do

- It does not create tasks. `create` is a method on the
  scheduler, but the caller supplies the parameters. The
  scheduler does not decide when a new task should exist.
- It does not decide when a task should block. Blocking is
  initiated by the task itself, via a future `block_current`
  call.
- It does not decide when a task should wake. Waking is
  initiated by whatever event the task was waiting for.
- It does not enforce CPU quotas. Every ready task at the
  highest priority runs on every tick. Quotas would require a
  scheduling policy more complex than strict priority.
- It does not distinguish between kernel tasks and user tasks.
  Both are `Task`s, and both are scheduled the same way. User
  tasks will be added when user mode is implemented.

## What will be added

Three things are on the roadmap for the scheduler.

**Exit.** A task that finishes must be able to remove itself
from the scheduler and free its kernel stack. This is
delicate, because the task is running on the stack it wants to
free. The standard pattern is to switch to another task's
stack first, then free.

**Block and wake.** A task that waits for an event must be
able to block without spinning. `block_current` marks the
current task `Blocked` and switches to another. `wake(id)`
marks a `Blocked` task `Ready` and enqueues it. This is the
substrate for all synchronization primitives.

**Priority inheritance.** When a high-priority task blocks on
a lock held by a low-priority task, the holder must
temporarily inherit the higher priority. This prevents
priority inversion. It will be added along with the first
blocking lock.

After those three, the scheduler is complete for kernel
tasks. User tasks will reuse the same primitives.