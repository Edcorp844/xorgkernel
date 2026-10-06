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
mod serial;
mod sync;
mod vga;

mod cpu;

use core::panic::PanicInfo;
use core::sync::atomic::{AtomicU32, Ordering};

use crate::capability::capability::CapabilityRights;
use crate::capability::core::MapError;
use crate::capability::object::ObjectKind;

// ---------------------------------------------------------------------
// Timer tick state
// ---------------------------------------------------------------------

/// Number of timer interrupts received since boot.
///
/// Incremented by `on_timer_tick` from IRQ0.
static TICKS: AtomicU32 = AtomicU32::new(0);

/// Number of ticks between "still alive" prints.
///
/// At the PIT frequency used below (100 Hz), this prints once per
/// second. Lower it to see more output, raise it to see less.
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
///
/// This includes:
///
/// - the page directory
/// - the bootstrap page table
/// - the IDT
/// - the GDT
/// - the frame bitmap
/// - `.bss`
///
/// The bootstrap stack is *not* zeroed here: it is placed above
/// `__kernel_end` in the linker script and this function runs on it.
unsafe fn clear_bootstrap_regions() {
    unsafe {
        marker(b'Z');
    }
    let start = arch::__bootstrap_start() as *mut u8;
    let end = arch::__kernel_end() as *mut u8;
    let size = (end as usize) - (start as usize);
    unsafe {
        core::ptr::write_bytes(start, 0, size);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn kernel_main(multiboot_info: *const u8) -> ! {
    unsafe {
        clear_bootstrap_regions();
        boot::multiboot2::init(multiboot_info);
    }

    console::init();
    println!("multiboot_info = {:p}", multiboot_info);
    println!("magic = 0x{:08x}", unsafe {
        *(multiboot_info as *const u32)
    });
    println!("total_size = {}", unsafe {
        *((multiboot_info as *const u32).add(1))
    });

    cpu::gdt::init();
    cpu::idt::init();
    memory::paging::init();
    memory::direct_map::init();
    memory::frame::init();
    capability::init();

    // ---- Kernel heap. ----
    //
    // The heap needs a capability to the kernel's own address
    // space, so it can install mappings into it as it grows. The
    // fabric registers that address space once, at boot, and the
    // heap holds the capability for its lifetime.
    //
    // The `MAP` right is required because the heap calls
    // `map_memory` when it grows.
    let kernel_as_rights =
        CapabilityRights::MAP | CapabilityRights::UNMAP | CapabilityRights::SHARE;

    let (_kernel_as_id, kernel_as_cap) = capability::core_mut()
        .register_kernel_address_space(kernel_as_rights)
        .expect("kernel address space registration failed");

    memory::heap::init(kernel_as_cap);

    println!();
    println!("Kernel heap initialized.");

    // ---- Existing substrate tests. ----
    //
    // These run with interrupts disabled. They exercise the
    // capability fabric and the address-space machinery and would
    // be unsafe to interleave with a timer tick, so they must
    // finish before interrupts are enabled.
    test_capability_transfer();
    test_cells();
    test_memory_object_allocation();
    test_map_memory();
    test_heap(); // <-- add this line
    test_address_space();
    test_address_space_activation();
    test_kernel_mapping_sharing();
    test_frame_allocator();
    test_address_space_isolation();

    // ---- Interrupt subsystem. ----
    //
    // Order matters here:
    //
    //   1. Remap the PIC so IRQ0-15 arrive at vectors 0x20-0x2F.
    //      This must happen before any IRQ is unmasked. Until the
    //      remap, IRQ0 would arrive at vector 0x08 (double fault).
    //
    //   2. Register the timer handler.
    //
    //   3. Program the PIT to a known frequency.
    //
    //   4. Unmask IRQ0.
    //
    //   5. Enable interrupts globally with `sti`.
    //
    // Only after step 5 will the first tick be delivered.
    setup_interrupts();

    // ---- Idle. ----
    //
    // The timer now ticks at the configured rate. The handler
    // increments TICKS and prints periodically. `hlt` suspends the
    // CPU until the next interrupt, which is the correct way to
    // idle a kernel with nothing to do.
    idle_loop()
}

/// Configures the PIC, PIT, and IRQ dispatch path, then enables
/// interrupts.
///
/// See the comment at the call site in `kernel_main` for the
/// ordering constraints.
fn setup_interrupts() {
    println!();
    println!("Initializing interrupt subsystem...");

    // 1. Remap the PIC.
    cpu::pic::remap();
    println!("  PIC remapped: IRQ0-15 -> vectors 0x20-0x2F");

    // 2. Register the timer handler.
    cpu::interrupts::register(0, on_timer_tick);
    println!("  IRQ0 handler registered");

    // 3. Program the PIT.
    cpu::pit::init(100);

    // 4. Unmask IRQ0.
    cpu::pic::unmask(0);
    println!("  IRQ0 unmasked");

    // 5. Enable interrupts.
    unsafe {
        core::arch::asm!("sti", options(nomem, nostack));
    }
    println!("  Interrupts enabled");
    println!();
}

/// Idles the CPU until the next interrupt.
///
/// Each iteration of this loop halts the processor; the next timer
/// tick or other interrupt wakes it, the handler runs, and the loop
/// re-enters `hlt`.
fn idle_loop() -> ! {
    loop {
        unsafe {
            core::arch::asm!("hlt", options(nomem, nostack));
        }
    }
}

/// IRQ0 handler.
///
/// Called by the IRQ dispatch path each time the PIT fires. The
/// handler runs with interrupts disabled (interrupt gates clear IF),
/// so it must complete quickly; anything long should be deferred to
/// a bottom half, which the kernel does not yet have.
///
/// Prints a running count once every `TICKS_PER_REPORT` ticks.
fn on_timer_tick() {
    let n = TICKS.fetch_add(1, Ordering::Relaxed) + 1;

    if n % TICKS_PER_REPORT == 0 {
        println!("tick: {}", n);
    }
}

// ---------------------------------------------------------------------
// Panic handler
// ---------------------------------------------------------------------

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // Print the panic message and location if the console is
    // initialized. During early boot the console may not be
    // ready, in which case the writes go nowhere, but by the
    // time tests run it's fully functional.
    println!();
    println!("========================================");
    println!("           KERNEL PANIC");
    println!("========================================");
    println!("{}", info);

    loop {
        core::hint::spin_loop();
    }
}

// ---------------------------------------------------------------------
// Substrate tests
//
// These run before interrupts are enabled. See `kernel_main`.
// ---------------------------------------------------------------------

fn test_address_space() {
    use crate::memory::address_space::AddressSpace;

    println!();
    println!("Testing address spaces...");

    let mut address_space = AddressSpace::new().expect("address-space creation failed");

    println!("  Page directory: 0x{:08x}", address_space.page_directory());

    let virtual_address = 0x0080_0000;
    let physical_address = 0x0010_0000;

    println!("  Checking initial mapping...");
    assert!(!address_space.is_mapped(virtual_address));
    println!("  Initial mapping check: SUCCESS");

    println!("  Creating mapping...");
    assert!(address_space.map(virtual_address, physical_address, true, false,));
    println!("  Mapping creation: SUCCESS");

    println!(
        "  Mapped VA 0x{:08x} -> PA 0x{:08x}",
        virtual_address, physical_address
    );

    println!("  Checking mapping...");
    let mapped = address_space.is_mapped(virtual_address);
    println!("  Mapping check returned: {}", mapped);

    assert!(mapped);
    println!("  Virtual mapping: SUCCESS");

    println!("  Translating...");
    let translated = address_space.translate(virtual_address);

    println!("  Translation result: 0x{:08x}", translated.unwrap_or(0));

    assert_eq!(translated, Some(physical_address));

    println!("  Virtual-to-physical translation: SUCCESS");

    println!("  Unmapping...");
    let unmapped = address_space.unmap(virtual_address);

    println!("  Unmap result: 0x{:08x}", unmapped.unwrap_or(0));

    assert_eq!(unmapped, Some(physical_address));

    println!("  Page unmapping: SUCCESS");

    println!("  Checking isolation...");
    assert!(!address_space.is_mapped(virtual_address));

    println!("  Address-space isolation: SUCCESS");
}

fn test_capability_transfer() {
    println!();
    println!("Testing capability transfer...");

    let core = capability::core_mut();

    let object = core
        .create_object(ObjectKind::Cell)
        .expect("object creation failed");

    println!("  Object created: {}", object.raw());

    // `SHARE` replaces the old `GRANT` right: a capability with
    // `SHARE` can be delegated to produce a derived capability.
    let parent_rights = CapabilityRights::READ | CapabilityRights::WRITE | CapabilityRights::SHARE;

    let parent = core
        .allocate(object, parent_rights)
        .expect("parent capability allocation failed");

    println!("  Parent capability: 0x{:08x}", parent.raw());

    let child = core
        .transfer(parent, CapabilityRights::READ)
        .expect("READ transfer failed");

    println!("  Child capability:  0x{:08x}", child.raw());

    assert!(core.has_rights(child, CapabilityRights::READ));
    assert!(!core.has_rights(child, CapabilityRights::WRITE));
    assert!(!core.has_rights(child, CapabilityRights::EXECUTE));

    println!("  Rights attenuation: SUCCESS");

    assert_eq!(core.object(parent), Some(object));
    assert_eq!(core.object(child), Some(object));

    println!("  Object sharing: SUCCESS");

    let child_rw = core
        .transfer(parent, CapabilityRights::READ | CapabilityRights::WRITE)
        .expect("READ|WRITE transfer failed");

    assert!(core.has_rights(child_rw, CapabilityRights::READ));
    assert!(core.has_rights(child_rw, CapabilityRights::WRITE));

    println!("  Multi-right transfer: SUCCESS");

    let denied = core.transfer(parent, CapabilityRights::EXECUTE);
    assert!(denied.is_none());

    println!("  Excess-rights rejection: SUCCESS");

    // A capability without SHARE cannot be delegated.
    let restricted = core
        .allocate(object, CapabilityRights::READ)
        .expect("restricted capability allocation failed");

    let denied = core.transfer(restricted, CapabilityRights::READ);
    assert!(denied.is_none());

    println!("  SHARE enforcement: SUCCESS");

    assert!(core.destroy_object(object));

    assert!(core.lookup(parent).is_none());
    assert!(core.lookup(child).is_none());
    assert!(core.lookup(child_rw).is_none());
    assert!(core.lookup(restricted).is_none());

    println!("  Object-wide revocation: SUCCESS");
}

fn test_cells() {
    println!();
    println!("Testing execution cells...");

    println!(
        "  CapabilityCore size: {} bytes",
        core::mem::size_of::<capability::core::CapabilityCore>()
    );
    let core = capability::core_mut();

    println!("  CapabilityCore address: 0x{:08x}", core as *mut _ as u32);

    let object = core
        .create_object(ObjectKind::Cell)
        .expect("object creation failed");

    println!("  Object created: {}", object.raw());

    println!("  Creating Cell A...");
    let cell_a = core.create_cell().expect("cell A creation failed");
    println!("  Cell A: {}", cell_a.raw());

    println!("  Creating Cell B...");
    let cell_b = core.create_cell().expect("cell B creation failed");
    println!("  Cell B: {}", cell_b.raw());
}

/// Tests the fabric's memory-object allocation path.
///
/// This exercises:
///
/// - `CapabilityCore::allocate_memory`, which allocates frames,
///   creates a memory object, registers it, and returns a
///   capability
/// - `CapabilityCore::memory_object`, which resolves a capability
///   back to the object
/// - `CapabilityCore::lookup_object_kind`, which reports the
///   object's kind
/// - `CapabilityCore::transfer`, for deriving an attenuated
///   capability
/// - `CapabilityCore::destroy_object`, which revokes every
///   capability and returns the object's frames
fn test_memory_object_allocation() {
    println!();
    println!("Testing memory object allocation...");

    let core = capability::core_mut();

    // Allocate a 4-page memory object with MAP | READ | WRITE.
    let rights = CapabilityRights::MAP
        | CapabilityRights::READ
        | CapabilityRights::WRITE
        | CapabilityRights::SHARE;

    let (object, cap) = core
        .allocate_memory(4, rights)
        .expect("memory object allocation failed");

    println!("  Object created: {}", object.raw());
    println!("  Capability:     0x{:08x}", cap.raw());

    // The registry must report the object's kind.
    assert_eq!(
        core.lookup_object_kind(object),
        Some(ObjectKind::MemoryObject)
    );

    println!("  Object kind: MemoryObject: SUCCESS");

    // The capability must carry the rights that were requested.
    assert!(core.has_rights(cap, CapabilityRights::MAP));
    assert!(core.has_rights(cap, CapabilityRights::READ));
    assert!(core.has_rights(cap, CapabilityRights::WRITE));
    assert!(!core.has_rights(cap, CapabilityRights::EXECUTE));
    assert!(core.has_rights(cap, CapabilityRights::SHARE));

    println!("  Rights: MAP|READ|WRITE|SHARE: SUCCESS");

    // The memory object must be resolvable through the capability.
    let mem = core
        .memory_object(cap)
        .expect("memory object lookup failed");

    assert_eq!(mem.page_count(), 4);

    println!("  Frames: {}", mem.page_count());

    for i in 0..4 {
        let frame = mem.frame(i).expect("frame missing");
        println!("    Frame {}: 0x{:08x}", i, frame.address());
    }

    // Derive an attenuated capability: READ only.
    let read_only = core
        .transfer(cap, CapabilityRights::READ)
        .expect("transfer failed");

    assert!(core.has_rights(read_only, CapabilityRights::READ));
    assert!(!core.has_rights(read_only, CapabilityRights::WRITE));

    println!("  Derived READ-only capability: 0x{:08x}", read_only.raw());
    println!("  Attenuation: SUCCESS");

    // Destroy the object. Both capabilities must become invalid,
    // and the frames must be returned to the frame allocator.
    assert!(core.destroy_object(object));

    assert!(core.lookup(cap).is_none());
    assert!(core.lookup(read_only).is_none());

    println!("  Object-wide revocation: SUCCESS");
}

fn test_kernel_mapping_sharing() {
    use crate::memory::address_space::AddressSpace;
    use crate::memory::direct_map;

    println!();
    println!("Testing kernel mapping sharing...");

    let address_space = AddressSpace::new().expect("address-space creation failed");

    let kernel_virtual = direct_map::PHYSICAL_MEMORY_BASE;

    println!("  Kernel virtual address: 0x{:08x}", kernel_virtual);

    println!("  Checking kernel mapping...");

    assert!(address_space.is_mapped(kernel_virtual));

    println!("  Kernel mapping visibility: SUCCESS");

    let translated = address_space.translate(kernel_virtual);

    println!("  Translation: 0x{:08x}", translated.unwrap_or(0));

    assert_eq!(translated, Some(0));

    println!("  Kernel mapping translation: SUCCESS");
}

fn test_frame_allocator() {
    println!();
    println!("Testing frame allocator isolation...");

    for _ in 0..16 {
        let frame = memory::frame::allocate().expect("frame allocation failed");
        let address = frame.address();

        assert!(
            address < arch::__bootstrap_start() || address >= arch::__bootstrap_stack_top(),
            "frame allocator returned a bootstrap frame: 0x{:08x}",
            address
        );

        println!("  Allocated frame: 0x{:08x}", address);
    }

    println!("  Frame allocator isolation: SUCCESS");
}

fn test_address_space_activation() {
    use crate::cpu::control;
    use crate::memory::address_space::AddressSpace;

    println!();
    println!("Testing address-space activation...");

    let mut space_a = AddressSpace::new().expect("address-space A creation failed");

    let mut space_b = AddressSpace::new().expect("address-space B creation failed");

    println!("  Address space A: 0x{:08x}", space_a.page_directory());

    println!("  Address space B: 0x{:08x}", space_b.page_directory());

    assert_ne!(space_a.page_directory(), space_b.page_directory());

    println!("  Private page directories: SUCCESS");

    let virtual_address = 0x0080_0000;

    let physical_a = 0x0010_0000;
    let physical_b = 0x0020_0000;

    println!("  Mapping A:");
    println!(
        "    VA 0x{:08x} -> PA 0x{:08x}",
        virtual_address, physical_a
    );

    assert!(space_a.map(virtual_address, physical_a, true, false,));

    println!("  Mapping B:");
    println!(
        "    VA 0x{:08x} -> PA 0x{:08x}",
        virtual_address, physical_b
    );

    assert!(space_b.map(virtual_address, physical_b, true, false,));

    assert_eq!(space_a.translate(virtual_address), Some(physical_a));
    assert_eq!(space_b.translate(virtual_address), Some(physical_b));

    println!("  Independent mappings: SUCCESS");

    println!("  Activating address space A...");
    unsafe {
        space_a.activate();
    }

    let cr3 = control::read_cr3();

    println!("  CR3 = 0x{:08x}", cr3);

    assert_eq!(cr3, space_a.page_directory());

    println!("  Address-space A activation: SUCCESS");

    println!("  Activating address space B...");
    unsafe {
        space_b.activate();
    }

    let cr3 = control::read_cr3();

    println!("  CR3 = 0x{:08x}", cr3);

    assert_eq!(cr3, space_b.page_directory());

    println!("  Address-space B activation: SUCCESS");

    println!("  Switching back to A...");
    unsafe {
        space_a.activate();
    }

    let cr3 = control::read_cr3();

    println!("  CR3 = 0x{:08x}", cr3);

    assert_eq!(cr3, space_a.page_directory());

    println!("  Address-space switching: SUCCESS");
}

fn test_address_space_isolation() {
    use crate::memory::address_space::AddressSpace;

    println!();
    println!("Testing address-space isolation...");

    let mut space_a = AddressSpace::new().expect("space A creation failed");
    let space_b = AddressSpace::new().expect("space B creation failed");

    let isolated_va = 0x0080_0000;
    let backing_pa = 0x0010_0000;

    println!(
        "  Space A maps VA 0x{:08x} -> PA 0x{:08x}",
        isolated_va, backing_pa
    );
    assert!(space_a.map(isolated_va, backing_pa, true, false));

    println!("  Space B does NOT map VA 0x{:08x}", isolated_va);
    assert!(!space_b.is_mapped(isolated_va));

    println!("  Activating A...");
    unsafe {
        space_a.activate();
    }

    println!("  Reading from A's mapping...");
    let value = unsafe { core::ptr::read_volatile(isolated_va as *const u32) };
    println!("  Read returned 0x{:08x}", value);
    println!("  Space A access: SUCCESS");
}

fn test_map_memory() {
    println!();
    println!("Testing map_memory...");

    let core = capability::core_mut();

    // Allocate a memory object.
    let mem_rights = CapabilityRights::MAP
        | CapabilityRights::READ
        | CapabilityRights::WRITE
        | CapabilityRights::SHARE;

    let (mo_id, mo_cap) = core
        .allocate_memory(4, mem_rights)
        .expect("memory object allocation failed");

    println!("  Memory object: {}", mo_id.raw());
    println!("  MO capability: 0x{:08x}", mo_cap.raw());

    // Allocate an address space.
    let as_rights = CapabilityRights::MAP
        | CapabilityRights::UNMAP
        | CapabilityRights::ACTIVATE
        | CapabilityRights::SHARE;

    let (as_id, as_cap) = core
        .allocate_address_space(as_rights)
        .expect("address space allocation failed");

    println!("  Address space: {}", as_id.raw());
    println!("  AS capability: 0x{:08x}", as_cap.raw());

    // Map the object into the address space.
    let va = 0x0080_0000;

    println!(
        "  Mapping VA 0x{:08x} <- memory object {}...",
        va,
        mo_id.raw()
    );

    core.map_memory(as_cap, mo_cap, va, true, false)
        .expect("map_memory failed");

    println!("  Mapping: SUCCESS");

    // Verify the mapping through the address space.
    let aspace = core
        .address_space(as_cap)
        .expect("address space lookup failed");

    for i in 0..4 {
        let page_va = va + i * 4096;
        let translated = aspace.translate(page_va);

        assert!(
            translated.is_some(),
            "page {} not mapped after map_memory",
            i
        );

        println!(
            "    VA 0x{:08x} -> PA 0x{:08x}",
            page_va,
            translated.unwrap()
        );
    }

    println!("  Translation: SUCCESS");

    // Mapping must fail if the caller lacks MAP on the address
    // space. Derive a read-only capability and try again.
    let ro_as = core
        .transfer(as_cap, CapabilityRights::SHARE)
        .expect("AS capability derivation failed");

    // `ro_as` still carries SHARE but has dropped MAP, UNMAP,
    // ACTIVATE. Mapping with it must fail.
    let result = core.map_memory(ro_as, mo_cap, va + 0x10_0000, true, false);

    assert_eq!(result, Err(MapError::MissingAddressSpaceMapRight));

    println!("  Rights enforcement on AS: SUCCESS");

    core.revoke(ro_as);

    // Unmap the mapping.
    println!("  Unmapping...");

    for i in 0..4 {
        let page_va = va + i * 4096;
        let unmapped = core
            .unmap_memory(as_cap, page_va)
            .expect("unmap_memory failed");

        println!(
            "    VA 0x{:08x} unmapped (was PA 0x{:08x})",
            page_va, unmapped
        );
    }

    let aspace = core
        .address_space(as_cap)
        .expect("address space lookup failed");

    assert!(!aspace.is_mapped(va));

    println!("  Unmapping: SUCCESS");

    // Destroy both objects. Their resources must be returned.
    assert!(core.destroy_object(as_id));
    assert!(core.destroy_object(mo_id));

    assert!(core.lookup(as_cap).is_none());
    assert!(core.lookup(mo_cap).is_none());

    println!("  Object-wide revocation: SUCCESS");
}

/// Tests the kernel heap by allocating through Rust's `alloc`
/// types.
///
/// This exercises:
///
/// - the global allocator, which delegates to the heap
/// - the bump allocator within a slab
/// - slab growth, when the first slab is exhausted
/// - the `map_memory` path, which the heap calls when it grows
fn test_heap() {
    use alloc::boxed::Box;
    use alloc::vec::Vec;

    println!();
    println!("Testing kernel heap...");

    // A single boxed value.
    let boxed = Box::new(42u32);

    assert_eq!(*boxed, 42);

    println!("  Box<u32> = {}: SUCCESS", *boxed);

    // A vector that grows past one allocation.
    let mut vec: Vec<u64> = Vec::new();

    for i in 0..100 {
        vec.push(i);
    }

    assert_eq!(vec.len(), 100);

    for i in 0..100 {
        assert_eq!(vec[i], i as u64);
    }

    println!("  Vec<u64> with 100 elements: SUCCESS");

    // A vector large enough to force a slab growth.
    //
    // Each u64 is 8 bytes; 100 000 elements is 800 KiB, well past
    // the 256 KiB slab size. This forces the heap to call
    // `grow` and install a second slab through the fabric.
    let mut big: Vec<u64> = Vec::new();

    for i in 0..100_000 {
        big.push(i as u64);
    }

    assert_eq!(big.len(), 100_000);
    assert_eq!(big[50_000], 50_000);
    assert_eq!(big[99_999], 99_999);

    println!("  Vec<u64> with 100 000 elements: SUCCESS");

    // Report committed bytes.
    let committed = memory::heap::committed();

    println!(
        "  Committed: {} bytes ({} KiB)",
        committed,
        committed / 1024
    );
    println!("  Kernel heap: SUCCESS");

    // Prevent the compiler from eliding the allocations.
    core::hint::black_box(&boxed);
    core::hint::black_box(&vec);
    core::hint::black_box(&big);
}

unsafe fn marker(byte: u8) {
    unsafe {
        core::arch::asm!(
            "out 0xE9, al",
            in("al") byte,
            options(nostack, preserves_flags),
        );
    }
}