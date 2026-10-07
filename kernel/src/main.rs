//! Kernel entry point.
//!
//! `kernel_main` is the function GRUB calls after loading the
//! kernel image. Its job is to bring the system from the state the
//! bootloader left it in to the state the scheduler expects:
//!
//! 1. **Bootloader handoff.** Copy the Multiboot2 information
//!    structure into the kernel's own memory, so it survives the
//!    switch to paging.
//!
//! 2. **Early console.** Bring up serial and VGA text so that the
//!    rest of boot has somewhere to log.
//!
//! 3. **Descriptor tables.** Load the GDT and IDT. Without these,
//!    an early exception has nowhere to go and the CPU triple-
//!    faults with no diagnostic.
//!
//! 4. **Memory.** Enable paging, install the kernel's physical-
//!    memory window, and initialize the frame allocator. After
//!    this step, the kernel can allocate and free 4 KiB physical
//!    frames.
//!
//! 5. **Capability fabric.** Initialize the fabric, register the
//!    kernel's own address space as a fabric-managed object, and
//!    remember the resulting capability so later subsystems can
//!    install mappings into the kernel's virtual memory.
//!
//! 6. **Heap.** Bring up the kernel heap. The heap is capability-
//!    mediated: its backing memory comes from memory objects
//!    allocated through the fabric, mapped into the kernel's
//!    address space through the fabric. The bootstrap cycle this
//!    creates is broken by the fabric's and heap's design; see
//!    `memory/heap.rs` for the analysis.
//!
//! 7. **Tests.** Run the substrate test suite. Tests run before
//!    the scheduler takes over, with interrupts still disabled,
//!    so they cannot be interleaved with timer ticks.
//!
//! 8. **Scheduler.** Initialize the scheduler, create one cell
//!    per initial task, and create the tasks themselves. Each
//!    task is linked to its cell and to the kernel address space
//!    capability.
//!
//! 9. **Interrupts.** Configure the PIC and PIT, register the
//!    timer handler, unmask IRQ0, and enable interrupts. The
//!    first timer tick arrives while the idle task is running.
//!
//! 10. **Scheduler handoff.** Call `schedule_and_switch`. This
//!     never returns: execution continues in the idle task, and
//!     `kernel_main`'s frame is left on the bootstrap stack,
//!     saved into `BOOTSTRAP_TASK` and never restored.
//!
//! The order is not arbitrary. Each step depends on the ones
//! before it:
//!
//! - The heap needs a capability to the kernel address space, so
//!   the fabric must be initialized and the kernel address space
//!   registered first.
//! - The scheduler's task creation allocates kernel stacks from
//!   the heap, so the heap must be up first.
//! - The test suite exercises the fabric and the heap, so both
//!   must be up first.
//! - Interrupts are enabled last, so that no handler can run
//!   before the structures it depends on exist.

#![no_std]
#![no_main]

extern crate alloc;

#[macro_use]
mod macros;

mod arch;
mod boot;
mod capability;
mod console;
mod memory;
mod sched;
mod sync;
mod tests;

mod cpu;

use core::panic::PanicInfo;
use core::sync::atomic::{AtomicU32, Ordering};

use crate::capability::capability::CapabilityRights;
use crate::sched::task::{PRIORITY_IDLE, PRIORITY_NORMAL};

// ---------------------------------------------------------------------
// Timer tick state
// ---------------------------------------------------------------------

/// Number of timer interrupts received since boot.
///
/// Used only for the periodic "still alive" log line. Once the
/// scheduler has a real time source, this will be replaced by a
/// proper tick counter.
static TICKS: AtomicU32 = AtomicU32::new(0);

/// Number of ticks between "still alive" prints.
///
/// At the PIT's configured 100 Hz, this is one log line per
/// second. Frequent enough to confirm the timer is running,
/// infrequent enough not to dominate the serial output.
const TICKS_PER_REPORT: u32 = 100;

// ---------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------

/// Zero every linker-defined region between `__bootstrap_start` and
/// `__kernel_end`.
///
/// These regions are declared `NOLOAD` in the linker script: they
/// occupy address space but are not part of the disk image loaded
/// by the bootloader. They must therefore be zeroed explicitly
/// before first use.
///
/// The regions covered are the page directory, the bootstrap page
/// table, the IDT, the GDT, the frame bitmap, and `.bss`. All of
/// them are written by the kernel before being read, so zeroing is
/// not strictly required for correctness, but it makes debugging
/// easier: an uninitialized read shows up as a zero rather than as
/// whatever the bootloader happened to leave in those frames.
///
/// # Safety
///
/// Must be called before any of the regions are used. In
/// `kernel_main` it is the first thing that runs, before even the
/// Multiboot2 structure is copied.
unsafe fn clear_bootstrap_regions() {
    let start = arch::__bootstrap_start() as *mut u8;
    let end = arch::__kernel_end() as *mut u8;
    let size = (end as usize) - (start as usize);
    unsafe {
        core::ptr::write_bytes(start, 0, size);
    }
}

/// The kernel's entry point.
///
/// Called from `_start` in `boot/grub_header.S` with the physical
/// address of the Multiboot2 information structure. Never returns;
/// control eventually passes to the scheduler, and `kernel_main`'s
/// frame is saved into the bootstrap task and abandoned.
#[unsafe(no_mangle)]
pub extern "C" fn kernel_main(multiboot_info: *const u8) -> ! {
    // Zero the kernel's own regions, then copy the bootloader's
    // information structure into a buffer in `.bss`. The copy is
    // necessary because the structure is placed above 4 MiB by
    // GRUB, and the kernel's identity map covers only the first
    // 4 MiB; once paging is enabled, the original pointer becomes
    // unreachable.
    unsafe {
        clear_bootstrap_regions();
        boot::multiboot2::init(multiboot_info);
    }

    // Set up the early console sinks (serial and VGA text). These
    // are unconditional: serial is used by every development
    // setup, and VGA text is present on every PC the kernel is
    // expected to run on.
    console::init();

    // Load the GDT and IDT, then enable paging and set up the
    // direct map and frame allocator.
    //
    // The GDT and IDT must come first: any exception between here
    // and the end of `kernel_main` is delivered through them, and
    // without the IDT an early fault would triple-fault with no
    // diagnostic.
    cpu::gdt::init();
    cpu::idt::init();
    memory::paging::init();
    memory::direct_map::init();

    // If the bootloader provided a linear framebuffer, register a
    // framebuffer sink with the console. This must happen after
    // paging and the direct map are in place, because the
    // framebuffer is mapped through the direct-map machinery.
    setup_framebuffer();

    // Initialize the frame allocator. After this call, physical
    // frames can be allocated and freed.
    memory::frame::init();

    // Initialize the capability fabric. The fabric's state is
    // constructed at load time; this call prints the
    // configuration and is the place where future runtime
    // initialization will live.
    capability::init();

    // ---- Kernel address space. ----
    //
    // Register the kernel's own address space with the fabric so
    // that it can be used as a target for `map_memory`. The
    // kernel address space already exists: it was established by
    // `paging::init`, and it holds the identity map, the direct
    // map, and every kernel code and data mapping. Registering it
    // does not create anything; it wraps the existing page
    // directory so the fabric can hand out a capability to it.
    //
    // The capability is stored in a fabric-global static because
    // many later subsystems need it and none of them has a
    // convenient path back to this function's locals:
    //
    // - `memory::heap::grow` uses it to install new heap regions.
    // - The scheduler's task-creation path stores it in every
    //   kernel task's `address_space` field.
    // - The test suite uses it to exercise the mapping path.
    //
    // The capability is never revoked. Revoking it would leave
    // the kernel unable to install mappings, which is not a
    // recoverable state.
    let kernel_as_rights =
        CapabilityRights::MAP | CapabilityRights::UNMAP | CapabilityRights::SHARE;

    let (_kernel_as_id, kernel_as_cap) = capability::core_mut()
        .register_kernel_address_space(kernel_as_rights)
        .expect("kernel address space registration failed");

    capability::set_kernel_as_cap(kernel_as_cap);

    // ---- Kernel heap. ----
    //
    // The heap needs a capability to the kernel's address space,
    // so it can install mappings into it as it grows. From this
    // point on, `Box`, `Vec`, and every other `alloc` type can be
    // used.
    memory::heap::init(kernel_as_cap);

    println!();
    println!("Kernel heap initialized.");

    // ---- Substrate tests. ----
    //
    // Run the full test suite. Tests run with interrupts disabled
    // and before the scheduler takes over, so they cannot be
    // interleaved with timer ticks. They must finish before
    // `sched::init` is called.
    tests::run_all();

    // ---- Scheduler bootstrap. ----
    //
    // From this point on, the kernel runs as a task, not as
    // `kernel_main`. The scheduler is initialized, then one cell
    // and one task are created for each initial task. The cells
    // are empty for now; the tasks hold no capabilities. When IPC
    // and syscalls arrive, cells will be populated with the
    // capabilities their tasks should hold.
    sched::init();

    let core = capability::core_mut();

    // ---- Idle task. ----
    //
    // The idle task runs when nothing else is ready. It executes
    // `hlt` in a loop, suspending the CPU until the next
    // interrupt. It is created first so that if the scheduler is
    // given control before the other tasks are ready (which
    // cannot happen in the current boot sequence, but is a
    // reasonable property to maintain), there is always at least
    // one task to run.
    let idle_cell = core.create_cell().expect("idle cell creation failed");
    let idle_id = sched::scheduler_mut()
        .create(
            "idle",
            idle_task,
            PRIORITY_IDLE,
            4096,
            idle_cell,
            kernel_as_cap,
        )
        .expect("idle task creation failed");

    // ---- Test tasks. ----
    //
    // Two normal-priority tasks that print and yield. Their
    // purpose is to demonstrate preemption and round-robin
    // scheduling: with two tasks at the same priority, the timer
    // tick alternates between them, and the serial output shows
    // interleaved progress.
    //
    // These will be removed once there is a real way to create
    // tasks (a `spawn` syscall, a userspace init process, or
    // similar). Until then they are the only thing exercising
    // the scheduler's preemption path.
    let a_cell = core.create_cell().expect("task-a cell creation failed");
    let a_id = sched::scheduler_mut()
        .create(
            "task-a",
            task_a,
            PRIORITY_NORMAL,
            4096,
            a_cell,
            kernel_as_cap,
        )
        .expect("task-a creation failed");

    let b_cell = core.create_cell().expect("task-b cell creation failed");
    let b_id = sched::scheduler_mut()
        .create(
            "task-b",
            task_b,
            PRIORITY_NORMAL,
            4096,
            b_cell,
            kernel_as_cap,
        )
        .expect("task-b creation failed");

    println!();
    println!("Handing control to the scheduler...");
    println!("  kernel AS  = 0x{:08x}", kernel_as_cap.raw());
    println!("  idle   id={} cell={}", idle_id, idle_cell.raw());
    println!("  task-a id={} cell={}", a_id, a_cell.raw());
    println!("  task-b id={} cell={}", b_id, b_cell.raw());

    // ---- Interrupt subsystem. ----
    //
    // The timer drives preemption once it is enabled. We set it
    // up before yielding so that the first tick arrives while the
    // initial task is running.
    //
    // The PIC remap must happen before any IRQ is unmasked. Before
    // remapping, IRQ0 is delivered at vector 0x08, which
    // collides with the CPU's double-fault exception. Remapping
    // moves IRQs to vectors 0x20-0x2F, which are free.
    setup_interrupts();

    // Yield to the scheduler. This never returns: the first
    // context switch saves `kernel_main`'s registers into
    // `BOOTSTRAP_TASK` and loads the first task's registers from
    // its kernel stack. From here on, execution is in a task.
    unsafe {
        sched::schedule_and_switch();
    }

    // Unreachable.
    //
    // If the scheduler ever selected the bootstrap task again,
    // `switch_context` would restore the registers saved above
    // and execution would resume here. The bootstrap task has no
    // ID and is not in any list, so this cannot happen; the loop
    // is here to satisfy the type checker and to halt cleanly if
    // it ever does.
    loop {
        core::hint::spin_loop();
    }
}

/// Detects a framebuffer from the Multiboot2 information and
/// registers it with the console.
///
/// Returns without doing anything if the bootloader did not
/// provide a linear framebuffer, or if the framebuffer is of a
/// type the console sink does not support. Both cases are
/// non-fatal: the serial and VGA text sinks are already running.
fn setup_framebuffer() {
    let Some(info) = boot::multiboot2::framebuffer_info() else {
        return;
    };

    let bytes_per_pixel = (info.bpp as usize + 7) / 8;
    let size_bytes = info.pitch * info.height;

    // Map the framebuffer at a dedicated virtual address, using
    // uncached 4 MiB pages. Uncached is required: the framebuffer
    // is MMIO, and CPU caching would delay writes to the device.
    let virt_base = memory::direct_map::map_mmio(info.address as u32, size_bytes) as *mut u8;

    if virt_base.is_null() {
        println!("Framebuffer: cannot map (address space exhausted)");
        return;
    }

    let fb = unsafe {
        console::framebuffer::Framebuffer::new(
            virt_base,
            info.pitch as usize,
            info.width as usize,
            info.height as usize,
            bytes_per_pixel,
            info.red_shift,
            info.red_size,
            info.green_shift,
            info.green_size,
            info.blue_shift,
            info.blue_size,
        )
    };

    match fb {
        Some(fb) => {
            console::init_framebuffer(fb);
            println!(
                "Framebuffer console: {}x{} @ {} bpp (virt 0x{:08x})",
                info.width, info.height, info.bpp, virt_base as u32
            );
        }
        None => println!("Framebuffer present but not usable"),
    }
}

/// Configures the PIC, PIT, and IRQ dispatch path, then enables
/// interrupts.
///
/// The order matters:
///
/// 1. Remap the PIC. Before remapping, IRQ0 is delivered at
///    vector 0x08, which collides with the double-fault
///    exception.
/// 2. Register the timer handler. The handler is stored in a
///    table indexed by IRQ line; registering before unmasking
///    ensures the first tick finds it.
/// 3. Program the PIT. The PIT starts generating ticks as soon
///    as it is programmed, but the PIC will not deliver them
///    until IRQ0 is unmasked.
/// 4. Unmask IRQ0. From this point, ticks accumulate in the PIC's
///    pending register.
/// 5. Enable interrupts with `sti`. The first tick is delivered
///    immediately after `sti` returns.
fn setup_interrupts() {
    println!();
    println!("Initializing interrupt subsystem...");

    cpu::pic::remap();
    println!("  PIC remapped: IRQ0-15 -> vectors 0x20-0x2F");

    cpu::interrupts::register(0, on_timer_tick);
    println!("  IRQ0 handler registered");

    cpu::pit::init(100);

    cpu::pic::unmask(0);
    println!("  IRQ0 unmasked");

    unsafe {
        core::arch::asm!("sti", options(nomem, nostack));
    }
    println!("  Interrupts enabled");
    println!();
}

/// IRQ0 handler.
///
/// Called by the IRQ dispatch path each time the PIT fires. After
/// updating the tick counter and printing periodically, hands
/// control to the scheduler. If a higher-priority task is ready,
/// or if the current task's time slice has expired, the scheduler
/// switches to it.
///
/// The handler runs with interrupts disabled (interrupt gates
/// clear IF on entry), so it cannot be preempted by another timer
/// tick. It must complete quickly: any work done here delays the
/// interrupted task's resumption by the same amount.
fn on_timer_tick() {
    let n = TICKS.fetch_add(1, Ordering::Relaxed) + 1;

    if n % TICKS_PER_REPORT == 0 {
        println!("tick: {}", n);
    }

    unsafe {
        sched::schedule_and_switch();
    }
}

// ---------------------------------------------------------------------
// Task entry points
// ---------------------------------------------------------------------

/// The idle task.
///
/// Runs when nothing else is ready. `hlt` suspends the CPU until
/// the next interrupt, which is either the next timer tick (which
/// will schedule something else if anything is ready) or a device
/// interrupt (which a driver handler will service).
///
/// The task never returns; it loops forever. If the idle task ever
/// exited, the scheduler would have nothing to run when all other
/// tasks were blocked, and the kernel would spin.
fn idle_task() -> ! {
    loop {
        unsafe {
            core::arch::asm!("hlt", options(nomem, nostack));
        }
    }
}

/// Test task A.
///
/// Prints a counter, increments it, and yields. With task B at the
/// same priority, the timer tick and the yields interleave the two
/// tasks' output on the serial console.
///
/// This task and `task_b` exist only to demonstrate the scheduler
/// and the preemption path. They will be removed once there is a
/// real way to create tasks.
fn task_a() -> ! {
    let mut count = 0u32;
    loop {
        println!("A{}", count);
        count = count.wrapping_add(1);
        sched::yield_task();
    }
}

/// Test task B.
///
/// See [`task_a`].
fn task_b() -> ! {
    let mut count = 0u32;
    loop {
        println!("B{}", count);
        count = count.wrapping_add(1);
        sched::yield_task();
    }
}

// ---------------------------------------------------------------------
// Panic handler
// ---------------------------------------------------------------------

/// The kernel's panic handler.
///
/// Prints the panic message and halts. There is no unwinding: the
/// kernel is built with `panic = "abort"`, and there is nothing
/// useful to unwind to anyway.
///
/// The handler does not attempt to recover. A panic in kernel code
/// is a bug, and continuing after one would leave the system in an
/// unknown state. Halting is the only correct response.
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    println!();
    println!("========================================");
    println!("           KERNEL PANIC");
    println!("========================================");
    println!("{}", info);

    loop {
        core::hint::spin_loop();
    }
}