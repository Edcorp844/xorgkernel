//! Physical frame allocator.
//!
//! The frame allocator owns every 4 KiB physical page in the system
//! and hands them out on demand to the rest of the kernel. It is the
//! lowest layer of the memory fabric: above it sit memory objects,
//! address spaces, and eventually capabilities.
//!
//! # Design
//!
//! The allocator is a simple bitmap allocator:
//!
//! - One bit per 4 KiB frame.
//! - 4 GiB address space / 4 KiB = 1 MiB frames.
//! - 1 MiB frames / 8 bits per byte = 128 KiB bitmap.
//!
//! The bitmap lives in a linker-placed region (see `linker.ld`,
//! symbol `__frame_bitmap_start`). It is a bootstrap structure:
//! the allocator cannot allocate its own bookkeeping before it
//! exists, so the bitmap must be statically placed.
//!
//! # Initialization
//!
//! Initialization proceeds in three phases:
//!
//! 1. **Reserve everything.** Every bit is set to 1 (used). This
//!    gives a known state from which the other phases only clear
//!    bits.
//!
//! 2. **Mark usable regions.** The BIOS E820 memory map tells us
//!    which physical ranges are usable RAM. Frames in those ranges
//!    are cleared (made free).
//!
//! 3. **Reserve boot memory.** Structures that the bootloader and
//!    linker placed (kernel image, page directory, page table, IDT,
//!    GDT, frame bitmap, bootstrap stack, low BIOS memory) are
//!    marked used again. Without this step the allocator would hand
//!    out frames that overlap kernel code or bootstrap data.
//!
//! After initialization the allocator hands out frames on demand.
//! When a frame is no longer needed, `free` returns it to the pool.
//!
//! # Concurrency
//!
//! The current implementation is single-threaded: it assumes only
//! one CPU is running and only one caller touches the allocator at
//! a time. Once SMP and preemption exist, this will need a lock or
//! a per-CPU design.

use crate::arch;
use crate::memory::boot::{self, MemoryRegion};

/// Size of a single physical frame, in bytes.
const PAGE_SIZE: u64 = 4096;

/// Number of frames in the physical address space.
///
/// 4 GiB / 4 KiB = 1 MiB frames.
const FRAME_COUNT: usize = 1 << 20;

/// Size of the frame bitmap, in bytes.
///
/// One bit per frame, so `FRAME_COUNT / 8`.
const BITMAP_SIZE: usize = FRAME_COUNT / 8;

/// Upper bound of the physical address space covered by the bitmap.
const FOUR_GIB: u64 = 0x1_0000_0000;

/// A single physical frame.
///
/// The frame is identified by its physical base address. The frame
/// allocator guarantees that this address is page-aligned and lies
/// inside the physical address space covered by the bitmap.
#[derive(Clone, Copy)]
pub struct Frame {
    address: u32,
}

impl Frame {
    /// Returns the physical base address of the frame.
    pub fn address(&self) -> u32 {
        self.address
    }
}

/// Physical frame allocator.
///
/// The allocator tracks which frames are in use and which are free,
/// and hands out free frames on demand.
///
/// See the module documentation for the initialization sequence.
pub struct FrameAllocator {
    /// Index of the next frame to consider when allocating.
    ///
    /// This is a hint, not a guarantee: the allocator scans forward
    /// from this index and skips frames that are already used. It is
    /// reset to a lower value when a frame below it is freed, so
    /// that freed low frames are reused first.
    next_frame: usize,

    /// Total number of frames tracked by the bitmap.
    ///
    /// Always equal to `FRAME_COUNT` after initialization. Exposed
    /// for diagnostics.
    total_frames: usize,

    /// Number of frames that were marked usable by the E820 map.
    ///
    /// This is the total amount of RAM the firmware reported,
    /// before any reservations are applied. Exposed for diagnostics.
    usable_frames: usize,

    /// Number of frames currently free (available for allocation).
    free_frames: usize,
}

impl FrameAllocator {
    /// Creates an uninitialized frame allocator.
    ///
    /// All counters are zero. The allocator must be initialized with
    /// [`FrameAllocator::init`] before any frame can be allocated.
    pub const fn new() -> Self {
        Self {
            next_frame: 0,
            total_frames: 0,
            usable_frames: 0,
            free_frames: 0,
        }
    }

    /// Initializes the allocator.
    ///
    /// This runs the three-phase initialization described in the
    /// module documentation: reserve everything, mark usable RAM,
    /// then reserve boot memory.
    ///
    /// Must be called exactly once, before any allocation.
    pub fn init(&mut self) {
        println!();
        println!("Initializing physical frame allocator...");
        println!(
            "  Bitmap: 0x{:08x} - 0x{:08x}",
            arch::__frame_bitmap_start(),
            arch::__frame_bitmap_end()
        );

        self.reserve_all();
        self.mark_usable_regions();
        self.reserve_boot_memory();

        self.total_frames = FRAME_COUNT;

        println!();
        println!("Frame allocator statistics:");
        println!("  Physical frames: {}", self.total_frames);
        println!("  Usable frames:   {}", self.usable_frames);
        println!("  Free frames:     {}", self.free_frames);
        println!("  Free memory:     {} KiB", self.free_frames * 4);
    }

    /// Allocates a single physical frame.
    ///
    /// Returns `None` if no free frame remains.
    ///
    /// The returned frame is marked used in the bitmap; the caller
    /// owns it until it is returned via [`FrameAllocator::free`].
    pub fn allocate(&mut self) -> Option<Frame> {
        for frame in self.next_frame..FRAME_COUNT {
            if !self.is_used(frame) {
                self.set_used(frame);

                self.next_frame = frame + 1;
                self.free_frames -= 1;

                return Some(Frame {
                    address: (frame as u32) * PAGE_SIZE as u32,
                });
            }
        }

        None
    }

    /// Returns a frame to the free pool.
    ///
    /// # Panics
    ///
    /// Panics if the frame is not page-aligned, if it lies outside
    /// the physical address space tracked by the bitmap, or if it is
    /// already free (a double free).
    pub fn free(&mut self, frame: Frame) {
        let address = frame.address();

        if address % PAGE_SIZE as u32 != 0 {
            panic!("frame_allocator: invalid frame address");
        }

        let frame_number = (address / PAGE_SIZE as u32) as usize;

        if frame_number >= FRAME_COUNT {
            panic!("frame_allocator: frame outside physical address space");
        }

        if !self.is_used(frame_number) {
            panic!("frame_allocator: double free");
        }

        self.set_free(frame_number);

        self.free_frames += 1;

        // Reuse freed low frames before moving on. If a frame below
        // the current scan position is freed, restart the scan there.
        if frame_number < self.next_frame {
            self.next_frame = frame_number;
        }
    }

    /// Returns the total number of frames tracked by the bitmap.
    pub fn total_frames(&self) -> usize {
        self.total_frames
    }

    /// Returns the number of frames the firmware reported as usable.
    pub fn usable_frames(&self) -> usize {
        self.usable_frames
    }

    /// Returns the number of frames currently available for allocation.
    pub fn free_frames(&self) -> usize {
        self.free_frames
    }

    // -----------------------------------------------------------------
    // Initialization phases
    // -----------------------------------------------------------------

    /// Phase 1: mark every frame as used.
    ///
    /// This establishes a known starting state. The next two phases
    /// only clear bits, so the invariant "everything is used unless
    /// explicitly freed" is trivially preserved.
    fn reserve_all(&mut self) {
        unsafe {
            core::ptr::write_bytes(arch::__frame_bitmap_start() as *mut u8, 0xff, BITMAP_SIZE);
        }

        self.free_frames = 0;
        self.usable_frames = 0;
        self.next_frame = 0;
    }

    /// Phase 2: mark every frame in a usable E820 region as free.
    fn mark_usable_regions(&mut self) {
        let map = boot::memory_map();

        for region in map.usable_regions() {
            self.mark_region_usable(region);
        }
    }

    /// Marks every frame in a single E820 region as free.
    ///
    /// Regions that extend past 4 GiB are truncated: the bitmap only
    /// covers the low 4 GiB of physical address space. Regions that
    /// begin past 4 GiB are ignored.
    fn mark_region_usable(&mut self, region: &MemoryRegion) {
        if region.base >= FOUR_GIB {
            return;
        }

        let region_end = region.base.saturating_add(region.length).min(FOUR_GIB);

        if region.base >= region_end {
            return;
        }

        let start = align_up(region.base);
        let end = align_down(region_end);

        if start >= end {
            return;
        }

        let start_frame = (start / PAGE_SIZE) as usize;
        let end_frame = (end / PAGE_SIZE) as usize;

        for frame in start_frame..end_frame {
            if frame >= FRAME_COUNT {
                break;
            }

            if self.is_used(frame) {
                self.set_free(frame);

                self.usable_frames += 1;
                self.free_frames += 1;
            }
        }
    }

    /// Phase 3: reserve every region the bootloader and linker placed.
    ///
    /// Without this step the allocator would happily hand out frames
    /// that overlap kernel code, kernel data, page tables, the IDT,
    /// the GDT, the frame bitmap, or the bootstrap stack.
    fn reserve_boot_memory(&mut self) {
        // Low 64 KiB: BIOS IVT, BDA, boot sector, BootInfo, E820
        // buffer. None of this is ordinary RAM from the kernel's
        // point of view.
        self.reserve_range(0x0000_0000, 0x0001_0000);

        // Kernel image loaded by the bootloader. Covers .text,
        // .rodata, and .data; the linker also reports this range
        // via BootInfo.
        self.reserve_range(boot::boot_info().kernel_start, boot::boot_info().kernel_end);

        // Everything the linker placed between __bootstrap_start
        // and __kernel_end:
        //
        //   - page directory
        //   - bootstrap page table
        //   - IDT
        //   - GDT
        //   - frame bitmap
        //   - .bss
        //
        // These regions are contiguous, so a single range covers
        // them all.
        self.reserve_range(arch::__bootstrap_start(), arch::__kernel_end());

        // Bootstrap stack. Placed ABOVE __kernel_end so that
        // clearing the bootstrap range at boot does not zero the
        // stack the kernel is running on.
        self.reserve_range(
            arch::__bootstrap_stack_bottom(),
            arch::__bootstrap_stack_top(),
        );
    }

    // -----------------------------------------------------------------
    // Bitmap primitives
    // -----------------------------------------------------------------

    /// Marks every frame in `[start, end)` as used.
    ///
    /// Both endpoints are rounded outward to frame boundaries, so
    /// the reserved range is at least as large as the input.
    ///
    /// Unlike [`mark_region_usable`], this does not check whether a
    /// frame was already used: it only sets bits that are currently
    /// clear, and adjusts `free_frames` accordingly.
    fn reserve_range(&mut self, start: u32, end: u32) {
        let start = align_down(start as u64);
        let end = align_up(end as u64);

        if start >= end {
            return;
        }

        let start_frame = (start / PAGE_SIZE) as usize;
        let end_frame = (end / PAGE_SIZE) as usize;

        for frame in start_frame..end_frame {
            if frame >= FRAME_COUNT {
                break;
            }

            if !self.is_used(frame) {
                self.set_used(frame);
                self.free_frames -= 1;
            }
        }
    }

    /// Returns `true` if the given frame is currently marked used.
    ///
    /// Frames outside the bitmap are treated as used, so the
    /// allocator never hands out a frame it cannot track.
    fn is_used(&self, frame: usize) -> bool {
        if frame >= FRAME_COUNT {
            return true;
        }

        unsafe {
            let bitmap = arch::__frame_bitmap_start() as *const u8;

            let byte = core::ptr::read_volatile(bitmap.add(frame / 8));

            let bit = frame % 8;

            (byte & (1 << bit)) != 0
        }
    }

    /// Marks the given frame as used.
    ///
    /// Frames outside the bitmap are silently ignored: the caller
    /// cannot reserve what does not exist.
    fn set_used(&mut self, frame: usize) {
        if frame >= FRAME_COUNT {
            return;
        }

        unsafe {
            let bitmap = arch::__frame_bitmap_start() as *mut u8;

            let ptr = bitmap.add(frame / 8);

            let value = core::ptr::read_volatile(ptr);

            core::ptr::write_volatile(ptr, value | (1 << (frame % 8)));
        }
    }

    /// Marks the given frame as free.
    ///
    /// Frames outside the bitmap are silently ignored.
    fn set_free(&mut self, frame: usize) {
        if frame >= FRAME_COUNT {
            return;
        }

        unsafe {
            let bitmap = arch::__frame_bitmap_start() as *mut u8;

            let ptr = bitmap.add(frame / 8);

            let value = core::ptr::read_volatile(ptr);

            core::ptr::write_volatile(ptr, value & !(1 << (frame % 8)));
        }
    }
}

// ---------------------------------------------------------------------
// Alignment helpers
// ---------------------------------------------------------------------

/// Rounds `address` up to the next frame boundary.
fn align_up(address: u64) -> u64 {
    (address + PAGE_SIZE - 1) & !(PAGE_SIZE - 1)
}

/// Rounds `address` down to the previous frame boundary.
fn align_down(address: u64) -> u64 {
    address & !(PAGE_SIZE - 1)
}

// ---------------------------------------------------------------------
// Global allocator
// ---------------------------------------------------------------------

/// The kernel's single frame allocator.
///
/// This is a bootstrap global: it is placed statically because the
/// system has no heap yet, and the allocator's own bookkeeping (the
/// bitmap) is also linker-placed for the same reason.
///
/// Once the kernel has a working heap and a capability fabric, the
/// allocator can be moved into a capability-managed object. Until
/// then this global is the authoritative allocator.
static mut FRAME_ALLOCATOR: FrameAllocator = FrameAllocator::new();

/// Initializes the global frame allocator.
///
/// Must be called exactly once, early in boot, after the memory map
/// is available and after the bootstrap regions are known.
pub fn init() {
    unsafe {
        (*core::ptr::addr_of_mut!(FRAME_ALLOCATOR)).init();
    }
}

/// Allocates one physical frame.
///
/// See [`FrameAllocator::allocate`].
pub fn allocate() -> Option<Frame> {
    unsafe { (*core::ptr::addr_of_mut!(FRAME_ALLOCATOR)).allocate() }
}

/// Returns a frame to the free pool.
///
/// See [`FrameAllocator::free`].
pub fn free(frame: Frame) {
    unsafe {
        (*core::ptr::addr_of_mut!(FRAME_ALLOCATOR)).free(frame);
    }
}

/// Returns the total number of frames tracked by the allocator.
pub fn total_frames() -> usize {
    unsafe { (*core::ptr::addr_of!(FRAME_ALLOCATOR)).total_frames() }
}

/// Returns the number of frames the firmware reported as usable.
pub fn usable_frames() -> usize {
    unsafe { (*core::ptr::addr_of!(FRAME_ALLOCATOR)).usable_frames() }
}

/// Returns the number of frames currently available for allocation.
pub fn free_frames() -> usize {
    unsafe { (*core::ptr::addr_of!(FRAME_ALLOCATOR)).free_frames() }
}
