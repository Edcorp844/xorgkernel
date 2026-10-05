//! Kernel heap.
//!
//! A general-purpose allocator for the kernel's own bookkeeping:
//! task structures, run queues, strings, and the various other
//! containers that Rust's `alloc` types need.
//!
//! # Design
//!
//! The heap is a **bump allocator over a dedicated virtual range**.
//! When the heap runs out of space, it asks the capability fabric
//! for one or more memory objects and installs them into the next
//! available page-aligned addresses in the heap's range. The heap
//! then continues from where it left off.
//!
//! ```text
//! HEAP_START                                              HEAP_END
//!     |                                                       |
//!     v                                                       v
//!     +----------+----------+----------+----------+-----------+
//!     | region 0 | region 1 | region 2 | unused   |           |
//!     +----------+----------+----------+----------+-----------+
//!                ^
//!                |
//!          current bump pointer
//! ```
//!
//! A **region** is one or more contiguous memory objects installed
//! consecutively in virtual address space. If a single request
//! needs more space than one memory object can hold (256 KiB with
//! the current `MAX_FRAMES = 64`), the heap installs multiple
//! objects back-to-back and treats them as one region.
//!
//! # Why a virtual range
//!
//! The heap does *not* use the direct map, and does *not* assume
//! that the frames backing a region are physically contiguous. It
//! sees only virtual addresses, and every virtual address it hands
//! out lies within a mapping that was installed through the
//! fabric's `map_memory`, which checked the kernel's `MAP` right on
//! both the address space and the memory object.
//!
//! This is the whole point. The kernel's heap is capability-mediated
//! like everything else. It does not reach through an ambient
//! mapping to grab arbitrary physical memory.
//!
//! # Free and deallocation
//!
//! The current implementation is a pure bump allocator: `dealloc`
//! is a no-op. This is correct only if allocations are never freed,
//! or if the freed memory is not reclaimed. For the kernel's
//! current usage — long-lived data structures that outlive the
//! kernel — this is acceptable.
//!
//! A free-list allocator (with per-size-class lists for O(1) alloc
//! and dealloc) will replace this once the kernel has enough
//! dynamic churn to justify it.
//!
//! # Reentrancy
//!
//! The heap is a single global. It is not thread-safe, not
//! interrupt-safe, and not SMP-safe. All accesses must occur with
//! interrupts disabled and with a guarantee that no other CPU is
//! touching the heap. This matches the current single-threaded
//! kernel.
//!
//! When the scheduler and preemption arrive, the heap will need
//! either a lock with priority inheritance or a per-CPU design.

use core::alloc::{GlobalAlloc, Layout};

use crate::capability::capability::{CapabilityId, CapabilityRights};
use crate::memory::object::MAX_FRAMES;

/// First virtual address of the heap range.
///
/// Chosen to be above the direct map (which ends at `0xc8000000`)
/// and below the kernel stack (which is in the low 4 MiB). The
/// range is currently unmapped; the heap installs mappings as it
/// grows.
pub const HEAP_START: u32 = 0xd000_0000;

/// One past the last virtual address of the heap range.
///
/// 256 MiB of virtual space, enough for the kernel's bookkeeping
/// for a long time.
pub const HEAP_END: u32 = 0xe000_0000;

/// Size of a single page, in bytes.
const PAGE_SIZE: u32 = 4096;

/// Minimum number of frames to install when growing.
///
/// Even for a small request, the heap installs at least this many
/// frames. This amortizes the cost of the fabric calls that back a
/// region: allocation, registration, and mapping. Without a
/// minimum, a `Box<u32>` would trigger a fabric round-trip for a
/// single frame.
///
/// 16 frames = 64 KiB, which is enough to cover many small
/// allocations without dominating memory on a system that might
/// only have a few megabytes of RAM.
const MIN_GROW_FRAMES: usize = 16;

/// Kernel heap.
///
/// See the module documentation for the design.
pub struct KernelHeap {
    /// Capability to the kernel's address space.
    ///
    /// Held for the lifetime of the heap. `map_memory` is called
    /// with this capability whenever the heap grows.
    address_space: CapabilityId,

    /// First virtual address not yet mapped.
    ///
    /// The next region will be installed starting at this address.
    /// Initially `HEAP_START`.
    next_region_va: u32,

    /// Bump pointer within the currently-active region.
    ///
    /// The next `alloc` will return memory starting here. When
    /// this reaches `current_region_end`, the heap grows.
    current: u32,

    /// End of the currently-active region.
    current_region_end: u32,

    /// Total bytes committed to the heap so far.
    ///
    /// Incremented by the size of each region as it is installed.
    /// Exposed for diagnostics.
    committed: u32,
}

impl KernelHeap {
    /// Creates an uninitialized heap.
    ///
    /// `address_space` must be a capability to the kernel's own
    /// address space, with `MAP` right. The heap will call
    /// `map_memory` with it whenever it grows.
    pub const fn new(address_space: CapabilityId) -> Self {
        Self {
            address_space,
            next_region_va: HEAP_START,
            current: 0,
            current_region_end: 0,
            committed: 0,
        }
    }

    /// Allocates a block of memory.
    ///
    /// Returns a null pointer on failure. Callers that use
    /// `GlobalAlloc` should handle null as allocation failure.
    ///
    /// # Safety
    ///
    /// The caller must ensure that:
    ///
    /// - no other thread is concurrently allocating
    /// - interrupts are disabled
    pub unsafe fn alloc(&mut self, layout: Layout) -> *mut u8 {
        let align = layout.align().max(core::mem::align_of::<usize>());
        let size = layout.size();

        // Align the bump pointer up to the requested alignment.
        let aligned = align_up(self.current, align as u32);

        // If the aligned pointer plus the requested size fits in
        // the current region, use it.
        if let Some(next) = aligned.checked_add(size as u32) {
            if next <= self.current_region_end {
                self.current = next;
                return aligned as *mut u8;
            }
        }

        // Otherwise, install a new region large enough for this
        // request, then retry.
        //
        // The new region needs to hold the alignment padding plus
        // the requested size. Worst-case padding is `align - 1`
        // bytes.
        let padding = (align - 1) as u32;
        let needed = (size as u32).saturating_add(padding);

        if !unsafe { self.grow(needed) } {
            return core::ptr::null_mut();
        }

        unsafe { self.alloc(layout) }
    }

    /// Frees a block of memory.
    ///
    /// The current implementation is a no-op: the heap is a bump
    /// allocator, and reclaimed bytes are not returned to the free
    /// pool. See the module documentation for the migration plan.
    ///
    /// # Safety
    ///
    /// The pointer and layout must be one previously returned by
    /// `alloc`. This is required by the `GlobalAlloc` contract, but
    /// the current implementation does not actually use them.
    pub unsafe fn dealloc(&mut self, _ptr: *mut u8, _layout: Layout) {
        // Intentionally empty. See module documentation.
    }

    /// Installs a new region at the end of the heap's virtual
    /// range, large enough for a request of `needed` bytes.
    ///
    /// A region is one or more memory objects installed
    /// consecutively. Each object holds at most `MAX_FRAMES`
    /// frames. If `needed` requires more than one object, the heap
    /// installs them back-to-back.
    ///
    /// # Parameters
    ///
    /// - `needed`: total bytes the region must provide, including
    ///   worst-case alignment padding for the pending allocation.
    ///
    /// # Returns
    ///
    /// `true` on success. `false` if:
    ///
    /// - the request would exceed the heap's virtual range
    /// - the fabric cannot allocate the memory objects
    /// - the fabric cannot install the mappings
    ///
    /// # Safety
    ///
    /// Must be called with the fabric's global state accessible
    /// (see `crate::capability::core_mut`). The fabric must not be
    /// in the middle of another operation.
    unsafe fn grow(&mut self, needed: u32) -> bool {
        // Compute how many frames the region must hold. Round up
        // to whole frames, then ensure the result is at least
        // `MIN_GROW_FRAMES` so that small requests do not trigger
        // a fabric round-trip per allocation.
        let frames_needed = ((needed + PAGE_SIZE - 1) / PAGE_SIZE) as usize;

        let total_frames = frames_needed.max(MIN_GROW_FRAMES);

        // The region must fit in the heap's virtual range.
        let total_bytes = (total_frames as u32) * PAGE_SIZE;

        if self.next_region_va + total_bytes > HEAP_END {
            return false;
        }

        // Install the region, one memory object at a time. Each
        // object holds at most `MAX_FRAMES` frames.
        let region_start = self.next_region_va;
        let mut cursor = region_start;
        let mut remaining = total_frames;

        while remaining > 0 {
            let chunk = remaining.min(MAX_FRAMES);
            let chunk_bytes = (chunk as u32) * PAGE_SIZE;

            // Ask the fabric for a memory object.
            //
            // The object carries MAP|READ|WRITE. SHARE is not
            // needed: the heap holds the object directly and never
            // delegates it.
            let rights = CapabilityRights::MAP | CapabilityRights::READ | CapabilityRights::WRITE;

            let core = crate::capability::core_mut();

            let (object_id, object_cap) = match core.allocate_memory(chunk, rights) {
                Some(result) => result,
                None => return false,
            };

            // Install the object into the kernel's address space.
            if core
                .map_memory(self.address_space, object_cap, cursor, true, false)
                .is_err()
            {
                // Roll back: destroy the object we just created.
                //
                // The mappings already installed are left in place.
                // They are backed by memory objects whose
                // capabilities we no longer hold, but the mappings
                // themselves remain valid until the address space
                // is destroyed. The heap does not track them, so
                // they leak. This is a known limitation of the
                // current design; see the module docs on
                // deallocation. Once we have a proper region list,
                // this rollback can be made complete.
                core.destroy_object(object_id);
                return false;
            }

            cursor += chunk_bytes;
            remaining -= chunk;
        }

        // Success. Update the heap's state.
        self.current = region_start;
        self.current_region_end = cursor;
        self.next_region_va = cursor;
        self.committed += cursor - region_start;

        true
    }
}

/// Rounds `value` up to the next multiple of `align`.
///
/// `align` must be a power of two.
const fn align_up(value: u32, align: u32) -> u32 {
    (value + align - 1) & !(align - 1)
}

// ---------------------------------------------------------------------
// Global allocator
// ---------------------------------------------------------------------

/// The kernel's global heap.
///
/// Initialized by `init`, after the fabric is ready. Before
/// initialization, `alloc` returns null and `dealloc` is a no-op.
///
/// # Safety
///
/// This is a `static mut` for the same reason the frame allocator
/// and the capability core are: the `GlobalAlloc` trait is a
/// global, not a capability-parameterized interface, so the heap's
/// state has to live in a global. The capability that governs the
/// heap is held inside the `KernelHeap`, not passed on each call.
static mut HEAP: Option<KernelHeap> = None;

/// Initializes the global heap.
///
/// `address_space` must be a capability to the kernel's own
/// address space, carrying `MAP`.
///
/// Must be called exactly once, after the fabric has registered
/// the kernel address space.
pub fn init(address_space: CapabilityId) {
    unsafe {
        HEAP = Some(KernelHeap::new(address_space));
    }
}

/// Returns the number of bytes committed to the heap.
pub fn committed() -> u32 {
    unsafe {
        let heap = &*core::ptr::addr_of!(HEAP);

        match heap {
            Some(heap) => heap.committed,
            None => 0,
        }
    }
}

/// The kernel's global allocator.
///
/// Delegates every allocation to the global `HEAP`. This is what
/// makes `Box`, `Vec`, `String`, and the rest of Rust's `alloc`
/// types work in the kernel.
struct KernelAllocator;

unsafe impl GlobalAlloc for KernelAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        unsafe {
            let heap = &mut *core::ptr::addr_of_mut!(HEAP);

            match heap {
                Some(heap) => heap.alloc(layout),
                None => core::ptr::null_mut(),
            }
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe {
            let heap = &mut *core::ptr::addr_of_mut!(HEAP);

            if let Some(heap) = heap {
                heap.dealloc(ptr, layout);
            }
        }
    }
}

#[global_allocator]
static ALLOCATOR: KernelAllocator = KernelAllocator;
