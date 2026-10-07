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
static TICKS: AtomicU32 = AtomicU32::new(0);

/// Number of ticks between "still alive" prints.
const TICKS_PER_REPORT: u32 = 100;

// ---------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------

/// Zero every linker-defined region between `__bootstrap_start` and
/// `__kernel_end`.
///
/// These regions are declared `NOLOAD` in the linker script: they
/// occupy address space but are not part of the disk image loaded by
/// the bootloader. They must therefore be zeroed explicitly before
/// first use.
unsafe fn clear_bootstrap_regions() {
    let start = arch::__bootstrap_start() as *mut u8;
    let end = arch::__kernel_end() as *mut u8;
    let size = (end as usize) - (start as usize);
    unsafe {
        core::ptr::write_bytes(start, 0, size);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn kernel_main(multiboot_info: *const u8) -> ! {
    // Zero the kernel's own regions, then copy the bootloader's
    // information structure into a buffer in `.bss`.
    unsafe {
        clear_bootstrap_regions();
        boot::multiboot2::init(multiboot_info);
    }

    // Set up the early console sinks (serial and VGA text).
    console::init();

    // Load the GDT and IDT, then enable paging and set up the
    // direct map and frame allocator.
    cpu::gdt::init();
    cpu::idt::init();
    memory::paging::init();
    memory::direct_map::init();

    // If the bootloader provided a linear framebuffer, register a
    // framebuffer sink with the console. This must happen after
    // paging and the direct map are in place.
    setup_framebuffer();

    // Initialize the frame allocator.
    memory::frame::init();

    // Initialize the capability fabric.
    capability::init();

    // ---- Kernel heap. ----
    //
    // The heap needs a capability to the kernel's own address
    // space, so it can install mappings into it as it grows.
    let kernel_as_rights =
        CapabilityRights::MAP | CapabilityRights::UNMAP | CapabilityRights::SHARE;

    let (_kernel_as_id, kernel_as_cap) = capability::core_mut()
        .register_kernel_address_space(kernel_as_rights)
        .expect("kernel address space registration failed");

    memory::heap::init(kernel_as_cap);

    println!();
    println!("Kernel heap initialized.");

    // ---- Substrate tests. ----
    tests::run_all();

    // ---- Scheduler bootstrap. ----
    //
    // Create the initial tasks and hand control to the scheduler.
    // From this point on, the kernel runs as a task, not as
    // `kernel_main`.
    sched::init();

    let idle_id = sched::scheduler_mut()
        .create("idle", idle_task, PRIORITY_IDLE, 4096)
        .expect("idle task creation failed");

    let a_id = sched::scheduler_mut()
        .create("task-a", task_a, PRIORITY_NORMAL, 4096)
        .expect("task-a creation failed");

    let b_id = sched::scheduler_mut()
        .create("task-b", task_b, PRIORITY_NORMAL, 4096)
        .expect("task-b creation failed");

    println!();
    println!("Handing control to the scheduler...");
    println!("  idle   = {}", idle_id);
    println!("  task-a = {}", a_id);
    println!("  task-b = {}", b_id);

    // ---- Interrupt subsystem. ----
    //
    // The timer drives preemption once it is enabled. We set it up
    // before yielding so that the first tick arrives while the
    // initial task is running.
    setup_interrupts();

    // Yield to the scheduler. This never returns.
    unsafe {
        sched::schedule_and_switch();
    }

    // Unreachable.
    loop {
        core::hint::spin_loop();
    }
}

/// Detects a framebuffer from the Multiboot2 information and
/// registers it with the console.
fn setup_framebuffer() {
    let Some(info) = boot::multiboot2::framebuffer_info() else {
        return;
    };

    let bytes_per_pixel = (info.bpp as usize + 7) / 8;
    let size_bytes = info.pitch * info.height;

    // Map the framebuffer at a dedicated virtual address, using
    // uncached 4 MiB pages.
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
/// control to the scheduler. If a higher-priority task is ready, or
/// if the current task's time slice has expired, the scheduler
/// switches to it.
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
/// the next interrupt.
fn idle_task() -> ! {
    loop {
        unsafe {
            core::arch::asm!("hlt", options(nomem, nostack));
        }
    }
}

/// Test task A.
fn task_a() -> ! {
    let mut count = 0u32;
    loop {
        println!("A{}", count);
        count = count.wrapping_add(1);
        sched::yield_task();
    }
}

/// Test task B.
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
