//! Physical frame allocator.
//!
//! The frame allocator owns every 4 KiB physical page in the system
//! and hands them out on demand. It is the lowest layer of the
//! memory fabric: above it sit memory objects, address spaces, and
//! eventually capabilities.
//!
//! # Design
//!
//! Two data structures are maintained side by side:
//!
//! - A **bitmap**, one bit per 4 KiB frame, which records which
//!   frames are reserved or in use. The bitmap is the source of
//!   truth for reservations and for `is_used` queries.
//!
//! - A **free list**, a singly-linked list threaded through the
//!   free frames themselves. The first 4 bytes of each free frame
//!   hold the frame number of the next free frame. The allocator
//!   keeps only a head pointer.
//!
//! Allocation pops the head of the free list. Freeing pushes onto
//! the head. Both operations are O(1) and touch only one frame's
//! worth of memory.
//!
//! The bitmap is retained because it is compact, easy to audit, and
//! gives O(1) `is_used` queries. It is *not* on the allocation fast
//! path.
//!
//! # Why two structures
//!
//! A bitmap-only allocator would need to scan for a free bit, which
//! is O(n) in the worst case. For a general-purpose kernel this is
//! tolerable; for a real-time workload it is not. The free list
//! gives a hard O(1) upper bound on both allocation and freeing.
//!
//! The free list is built once, at the end of initialization, after
//! every reservation has been applied. It is never rebuilt. The
//! bitmap continues to be updated on every allocation and free, so
//! `is_used` remains accurate.
//!
//! # Initialization
//!
//! Initialization proceeds in four phases:
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
//! 4. **Build the free list.** Every frame that is still free after
//!    the first three phases is pushed onto the free list. From this
//!    point on, allocation and free are O(1).
//!
//! # Concurrency
//!
//! The current implementation is single-threaded: it assumes only
//! one CPU is running and only one caller touches the allocator at
//! a time. Once SMP and preemption exist, this will need a lock or
//! a per-CPU design. For a real-time configuration, the lock must
//! support priority inheritance to avoid priority inversion.

use crate::arch;
use crate::memory::boot::{self, MemoryRegion};
use crate::memory::direct_map;

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

/// Sentinel value marking the end of the free list.
///
/// Chosen as `u32::MAX` because it is not a valid frame number: the
/// highest frame number is `FRAME_COUNT - 1`, which is much smaller.
const FREE_LIST_END: u32 = u32::MAX;

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
/// See the module documentation for the design and initialization
/// sequence.
pub struct FrameAllocator {
    /// Head of the free list, as a frame number.
    ///
    /// `FREE_LIST_END` means the list is empty and no frame is
    /// available for allocation. Every other value is the frame
    /// number of the first free frame; that frame's first 4 bytes
    /// (accessed through the direct map) hold the next frame number
    /// or `FREE_LIST_END`.
    free_head: u32,

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
    ///
    /// Invariant: after initialization, this is equal to the number
    /// of frames in the free list.
    free_frames: usize,
}

impl FrameAllocator {
    /// Creates an uninitialized frame allocator.
    ///
    /// All counters are zero and the free list is empty. The
    /// allocator must be initialized with [`FrameAllocator::init`]
    /// before any frame can be allocated.
    pub const fn new() -> Self {
        Self {
            free_head: FREE_LIST_END,
            total_frames: 0,
            usable_frames: 0,
            free_frames: 0,
        }
    }

    /// Initializes the allocator.
    ///
    /// Runs the four-phase initialization described in the module
    /// documentation: reserve everything, mark usable RAM, reserve
    /// boot memory, then build the free list.
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
        self.build_free_list();

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
    /// O(1): pops the head of the free list, reads the next pointer
    /// from the frame's first 4 bytes, and updates the head.
    ///
    /// Returns `None` if the free list is empty.
    ///
    /// The returned frame is marked used in the bitmap; the caller
    /// owns it until it is returned via [`FrameAllocator::free`].
    pub fn allocate(&mut self) -> Option<Frame> {
        if self.free_head == FREE_LIST_END {
            return None;
        }

        let frame_number = self.free_head;
        let frame_address = frame_number * PAGE_SIZE as u32;

        // Read the next pointer from the frame's first 4 bytes.
        // The frame is currently free, so the caller has not
        // touched it since it was placed on the list.
        let next = unsafe {
            let virt = direct_map::phys_to_virt(frame_address) as *const u32;
            core::ptr::read_volatile(virt)
        };

        self.free_head = next;
        self.set_used(frame_number as usize);
        self.free_frames -= 1;

        Some(Frame {
            address: frame_address,
        })
    }

    /// Returns a frame to the free pool.
    ///
    /// O(1): writes the current head into the frame's first 4 bytes
    /// and updates the head to point at the returned frame.
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

        // Push onto the free list: write the old head into the
        // frame's first 4 bytes, then point the head at the frame.
        unsafe {
            let virt = direct_map::phys_to_virt(address) as *mut u32;
            core::ptr::write_volatile(virt, self.free_head);
        }

        self.free_head = frame_number as u32;
        self.set_free(frame_number);
        self.free_frames += 1;
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
    ///
    /// The free list is not touched here: it is built in phase 4,
    /// after all reservations are complete.
    fn reserve_all(&mut self) {
        unsafe {
            core::ptr::write_bytes(arch::__frame_bitmap_start() as *mut u8, 0xff, BITMAP_SIZE);
        }

        self.free_head = FREE_LIST_END;
        self.free_frames = 0;
        self.usable_frames = 0;
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

    /// Phase 4: build the free list from the bitmap.
    ///
    /// Walks every frame once. Frames that are not used are pushed
    /// onto the free list in decreasing address order, so the list
    /// ends up with the lowest frame at its tail. The order does
    /// not affect correctness; it only affects which frames are
    /// handed out first.
    ///
    /// This is the only O(n) operation in the allocator. It runs
    /// once at boot and is not on any hot path.
    fn build_free_list(&mut self) {
        self.free_head = FREE_LIST_END;

        // Walk frames from highest to lowest. Each free frame's
        // first 4 bytes are set to the current head, and the frame
        // becomes the new head. The result is a list whose head is
        // the lowest-numbered free frame.
        for frame in (0..FRAME_COUNT).rev() {
            if self.is_used(frame) {
                continue;
            }

            let frame_number = frame as u32;
            let frame_address = frame_number * PAGE_SIZE as u32;

            unsafe {
                let virt = direct_map::phys_to_virt(frame_address) as *mut u32;
                core::ptr::write_volatile(virt, self.free_head);
            }

            self.free_head = frame_number;
        }
    }

    // -----------------------------------------------------------------
    // Bitmap primitives
    // -----------------------------------------------------------------

    /// Marks every frame in `[start, end)` as used.
    ///
    /// Both endpoints are rounded outward to frame boundaries, so
    /// the reserved range is at least as large as the input.
    ///
    /// Used only during initialization. After `build_free_list` has
    /// run, calling this would leave the free list inconsistent
    /// with the bitmap, so it must not be called again.
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
    ///
    /// O(1): a single byte read and bit test.
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

    /// Marks the given frame as used in the bitmap.
    ///
    /// Frames outside the bitmap are silently ignored: the caller
    /// cannot reserve what does not exist.
    ///
    /// This does not touch the free list. It is the caller's
    /// responsibility to ensure the two remain consistent.
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

    /// Marks the given frame as free in the bitmap.
    ///
    /// Frames outside the bitmap are silently ignored.
    ///
    /// This does not touch the free list. It is the caller's
    /// responsibility to ensure the two remain consistent.
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
