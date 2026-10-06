//! Scheduler primitive tests.
//!
//! These tests exercise the scheduler's data structures without
//! invoking context switching. They create tasks, move them through
//! state transitions, and verify the priority-based selection
//! policy.

use crate::println;
use crate::sched::scheduler_mut;
use crate::sched::task::{PRIORITY_IDLE, PRIORITY_NORMAL, PRIORITY_REALTIME, TaskState};

/// Tests the scheduler's data structures and policy without
/// context switching.
pub fn test_scheduler() {
    println!();
    println!("Testing scheduler primitives...");

    let sched = scheduler_mut();

    let idle = sched
        .create("idle", task_stub_idle, PRIORITY_IDLE, 4096)
        .expect("idle task creation failed");
    let kern = sched
        .create("kern", task_stub_kernel, PRIORITY_NORMAL, 4096)
        .expect("kernel task creation failed");
    let rt = sched
        .create("rt", task_stub_rt, PRIORITY_REALTIME, 4096)
        .expect("realtime task creation failed");

    println!("  Created: idle={} kern={} rt={}", idle, kern, rt);

    assert_eq!(sched.task(idle).unwrap().state(), TaskState::Ready);
    assert_eq!(sched.task(kern).unwrap().state(), TaskState::Ready);
    assert_eq!(sched.task(rt).unwrap().state(), TaskState::Ready);

    println!("  Initial states: all Ready");

    assert_eq!(sched.schedule(), Some(rt));
    assert_eq!(sched.task(rt).unwrap().state(), TaskState::Running);

    assert_eq!(sched.schedule(), Some(kern));
    assert_eq!(sched.task(kern).unwrap().state(), TaskState::Running);

    assert_eq!(sched.schedule(), Some(idle));
    assert_eq!(sched.schedule(), None);

    println!("  Priority order: SUCCESS");

    sched.make_ready(rt);
    assert_eq!(sched.task(rt).unwrap().state(), TaskState::Ready);
    assert_eq!(sched.schedule(), Some(rt));

    println!("  Re-enqueue: SUCCESS");

    sched.make_ready(kern);
    sched.make_blocked(kern);
    assert_eq!(sched.task(kern).unwrap().state(), TaskState::Blocked);
    assert_eq!(sched.schedule(), None);

    println!("  Blocked: SUCCESS");

    sched.make_ready(kern);
    assert_eq!(sched.task(kern).unwrap().state(), TaskState::Ready);
    assert_eq!(sched.schedule(), Some(kern));

    println!("  Wake: SUCCESS");

    sched.exit(kern);
    assert!(sched.task(kern).is_none());

    println!("  Exit and reap: SUCCESS");

    println!("  Scheduler primitives: SUCCESS");
}

/// Stub entry points for the scheduler tests.
///
/// These functions are never actually called during the test: the
/// test only exercises task creation and scheduler bookkeeping, not
/// context switching. They exist to provide a valid entry pointer
/// for the initial frame.
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
