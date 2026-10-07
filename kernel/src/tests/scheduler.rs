//! Scheduler primitive tests.
//!
//! These tests exercise the scheduler's data structures without
//! invoking context switching. They create tasks, move them
//! through state transitions, and verify the priority-based
//! selection policy.
//!
//! # Why no context switching
//!
//! `schedule_and_switch` saves the outgoing task's registers and
//! loads the incoming task's, which means it never returns to its
//! caller. A test that called it would lose control of the CPU and
//! never reach its assertions. The tests therefore exercise the
//! scheduler *up to* the switch: task creation, state transitions,
//! and selection. The switch itself is exercised by the boot
//! sequence, which hands control to the idle task and observes the
//! result on the serial console.
//!
//! # Why a real cell and address space
//!
//! `Task::create` requires a valid `CellId` and a non-invalid
//! `CapabilityId` for the address space. The tests therefore ask
//! the fabric for a fresh cell per task and use the kernel
//! address space capability, which was registered during boot and
//! is reachable through [`crate::capability::kernel_as_cap`].
//!
//! This makes the tests depend on the fabric, which is a change
//! from the earlier version of this file: the tests used to run
//! without any fabric interaction. The dependency is correct,
//! because the scheduler's task-creation path now requires the
//! fabric. Tests that want to check the scheduler in isolation
//! should use the scheduler's data structures directly rather than
//! going through `Scheduler::create`.

use crate::capability::capability::CapabilityRights;
use crate::capability::object::ObjectKind;
use crate::println;
use crate::sched::scheduler_mut;
use crate::sched::task::{PRIORITY_IDLE, PRIORITY_NORMAL, PRIORITY_REALTIME, TaskState};

/// Tests the scheduler's data structures and policy without
/// context switching.
///
/// Creates three tasks at three different priorities, verifies
/// that they are initially `Ready`, that `schedule` returns them
/// in priority order, that blocking removes a task from selection,
/// that waking restores it, and that exiting reaps it.
pub fn test_scheduler() {
    println!();
    println!("Testing scheduler primitives...");

    let core = crate::capability::core_mut();
    let kernel_as = crate::capability::kernel_as_cap();

    // Each task gets its own cell. The cells are empty; the
    // scheduler does not populate them, and these tests do not
    // need them populated.
    let idle_cell = core.create_cell().expect("idle cell creation failed");
    let kern_cell = core.create_cell().expect("kern cell creation failed");
    let rt_cell = core.create_cell().expect("rt cell creation failed");

    let sched = scheduler_mut();

    let idle = sched
        .create(
            "idle",
            task_stub_idle,
            PRIORITY_IDLE,
            4096,
            idle_cell,
            kernel_as,
        )
        .expect("idle task creation failed");
    let kern = sched
        .create(
            "kern",
            task_stub_kernel,
            PRIORITY_NORMAL,
            4096,
            kern_cell,
            kernel_as,
        )
        .expect("kernel task creation failed");
    let rt = sched
        .create(
            "rt",
            task_stub_rt,
            PRIORITY_REALTIME,
            4096,
            rt_cell,
            kernel_as,
        )
        .expect("realtime task creation failed");

    println!("  Created: idle={} kern={} rt={}", idle, kern, rt);

    // ---- Initial state: every task is Ready. ----

    assert_eq!(sched.task(idle).unwrap().state(), TaskState::Ready);
    assert_eq!(sched.task(kern).unwrap().state(), TaskState::Ready);
    assert_eq!(sched.task(rt).unwrap().state(), TaskState::Ready);

    // ---- Fabric linkage: each task carries the cell it was
    //      created with, and the kernel address space. ----

    assert_eq!(sched.task(idle).unwrap().cell(), idle_cell);
    assert_eq!(sched.task(kern).unwrap().cell(), kern_cell);
    assert_eq!(sched.task(rt).unwrap().cell(), rt_cell);

    assert_eq!(sched.task(idle).unwrap().address_space(), kernel_as);
    assert_eq!(sched.task(kern).unwrap().address_space(), kernel_as);
    assert_eq!(sched.task(rt).unwrap().address_space(), kernel_as);

    println!("  Initial states: all Ready, cells and AS linked");

    // ---- Priority order: highest priority first. ----

    assert_eq!(sched.schedule(), Some(rt));
    assert_eq!(sched.task(rt).unwrap().state(), TaskState::Running);

    assert_eq!(sched.schedule(), Some(kern));
    assert_eq!(sched.task(kern).unwrap().state(), TaskState::Running);

    assert_eq!(sched.schedule(), Some(idle));
    assert_eq!(sched.schedule(), None);

    println!("  Priority order: SUCCESS");

    // ---- Re-enqueue: a running task can be made Ready again. ----

    sched.make_ready(rt);
    assert_eq!(sched.task(rt).unwrap().state(), TaskState::Ready);
    assert_eq!(sched.schedule(), Some(rt));

    println!("  Re-enqueue: SUCCESS");

    // ---- Block: a blocked task is not selected. ----

    sched.make_ready(kern);
    sched.make_blocked(kern);
    assert_eq!(sched.task(kern).unwrap().state(), TaskState::Blocked);

    // At this point: `rt` is running, `kern` is blocked, and
    // `idle` is running. Nothing is ready, so scheduling returns
    // `None`.
    assert_eq!(sched.schedule(), None);

    // Put `idle` back on its queue and verify that the scheduler
    // picks it, skipping the higher-priority `kern` (blocked) and
    // `rt` (running). This is the property the block test is
    // about: a non-ready task does not block selection of a
    // ready task below it.
    sched.make_ready(idle);
    assert_eq!(sched.schedule(), Some(idle));

    println!("  Blocked: SUCCESS");

    // ---- Wake: a blocked task becomes selectable again. ----

    sched.make_ready(kern);
    assert_eq!(sched.task(kern).unwrap().state(), TaskState::Ready);
    assert_eq!(sched.schedule(), Some(kern));

    println!("  Wake: SUCCESS");

    // ---- Exit: a task is reaped and no longer resolvable. ----

    sched.exit(kern);
    assert!(sched.task(kern).is_none());

    println!("  Exit and reap: SUCCESS");

    println!("  Scheduler primitives: SUCCESS");
}

/// Tests that a task's fabric linkage survives creation and is
/// visible from the cell side as well as the task side.
///
/// The linkage test is deliberately stronger than the one embedded
/// in `test_scheduler`: it grants a real capability to the cell,
/// verifies the cell holds it, and verifies that the cell reached
/// through the task is the same one the fabric knows about. This
/// catches a class of bug where the task stores a `CellId` that
/// does not correspond to any live cell, which would otherwise go
/// unnoticed until the first syscall tried to use it.
pub fn test_task_fabric_linkage() {
    println!();
    println!("Testing task–fabric linkage...");

    let core = crate::capability::core_mut();
    let sched = scheduler_mut();

    let kernel_as = crate::capability::kernel_as_cap();

    // Create a cell and give it a real capability. The object is
    // a `Cell`-kind object purely because the kind enum has no
    // more specific choice; the object is a placeholder for the
    // linkage test.
    let cell = core.create_cell().expect("cell creation failed");

    let obj = core
        .create_object(ObjectKind::Cell)
        .expect("object creation failed");
    let cap = core
        .allocate(obj, CapabilityRights::READ)
        .expect("capability allocation failed");

    assert!(
        core.grant_capability(cell, cap),
        "grant_capability to a live cell must succeed"
    );

    assert!(
        core.cell_has_capability(cell, cap),
        "cell must hold the capability after grant"
    );

    // Create the task, using the populated cell and the kernel
    // address space.
    let id = sched
        .create(
            "linkage-test",
            task_stub_idle,
            PRIORITY_NORMAL,
            4096,
            cell,
            kernel_as,
        )
        .expect("task creation failed");

    // The task must carry exactly the cell and address space we
    // passed in.
    let task = sched.task(id).expect("task lookup failed");

    assert_eq!(
        task.cell(),
        cell,
        "task's cell must match creation argument"
    );
    assert_eq!(
        task.address_space(),
        kernel_as,
        "task's address space must match creation argument"
    );

    // The cell reached through the task must be the same cell the
    // fabric knows about, and it must still hold the capability.
    assert!(
        core.cell_has_capability(task.cell(), cap),
        "cell reached through the task must hold the capability granted earlier"
    );

    // Clean up.
    sched.exit(id);
    core.destroy_object(obj);
    assert!(
        core.lookup(cap).is_none(),
        "capability must be revoked after object destruction"
    );

    println!("  Task–fabric linkage: SUCCESS");
}

/// Stub entry points for the scheduler tests.
///
/// These functions are never actually called during the tests: the
/// tests only exercise task creation and scheduler bookkeeping,
/// not context switching. They exist to provide a valid entry
/// pointer for the initial frame.
///
/// Each stub is a distinct function rather than a shared one so
/// that the compiler cannot coalesce them and so that a future
/// debugging session that traces entry pointers can tell which
/// task was created from which stub.
fn task_stub_idle() -> ! {
    loop {
        core::hint::spin_loop();
    }
}

fn task_stub_kernel() -> ! {
    loop {
        core::hint::spin_loop();
    }
}

fn task_stub_rt() -> ! {
    loop {
        core::hint::spin_loop();
    }
}
