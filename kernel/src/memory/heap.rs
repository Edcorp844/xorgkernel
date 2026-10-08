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
//! for a memory object and installs it into the next available
//! page-aligned address in the heap's range. The heap then
//! continues from where it left off.
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
//! # Region size
//!
//! Regions are a fixed size, [`REGION_FRAMES`] frames. The size is
//! a policy choice, not a consequence of the pending allocation:
//!
//! - Small enough that installing a region on a memory-constrained
//!   system does not waste much.
//! - Large enough that a region serves many small allocations
//!   before the heap needs to grow again.
//!
//! This decoupling matters. If the region size tracked the
//! requested allocation size, a single large allocation would
//! trigger a fabric round-trip to install exactly that much memory,
//! and the resulting region would serve few subsequent allocations.
//! A fixed region size amortizes the fabric cost across many
//! allocations, regardless of the sizes involved.
//!
//! # Allocation size limit
//!
//! Because regions are a fixed size, the heap cannot serve an
//! allocation larger than one region. [`MAX_ALLOCATION_FRAMES`]
//! records this limit; `alloc` refuses a request that exceeds it
//! by returning null.
//!
//! The limit is 1 MiB per allocation with the current constants.
//! This covers every allocation the kernel is expected to make for
//! the foreseeable future.
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
//! # Bootstrap cycle
//!
//! The heap is backed by memory objects, and memory objects are
//! backed by the heap only if their frame count exceeds
//! [`crate::memory::object::INLINE_FRAMES`].
//!
//! The heap avoids the cycle by construction:
//!
//! - `REGION_FRAMES` (the number of frames in a heap region) is
//!   equal to `INLINE_FRAMES` (the number of frames a memory
//!   object can store without a heap allocation).
//! - Every memory object that backs a heap region therefore uses
//!   inline storage. Constructing it does not touch the heap, and
//!   `grow` never recurses into `alloc`.
//!
//! If `REGION_FRAMES` were ever raised above `INLINE_FRAMES`, the
//! recursion would reappear: `grow` would create a heap-allocated
//! `MemoryObject`, whose `Vec<Frame>` would call back into the
//! heap, whose empty state would call `grow` again, and so on.
//!
//! Keeping the two constants equal is the invariant that makes
//! the heap bootstrappable.
//!
//! # Reentrancy
//!
//! The heap is a single global. It is not thread-safe, not
//! interrupt-safe, and not SMP-safe. All accesses must occur with
//! interrupts disabled and with a guarantee that no other CPU is
//! touching the heap. This matches the current single-threaded
//! kernel.

use core::alloc::{GlobalAlloc, Layout};

use crate::capability::capability::{CapabilityId, CapabilityRights};

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

/// Number of frames in a heap region.
///
/// Every region the heap installs is this size, regardless of the
/// allocation that triggered the growth.
///
/// The value must equal [`crate::memory::object::INLINE_FRAMES`],
/// so that the memory object backing a region uses inline storage
/// and does not recurse into the heap. See the module
/// documentation on the bootstrap cycle.
pub const REGION_FRAMES: usize = 256;

/// Maximum size of a single heap allocation, in frames.
///
/// The heap cannot serve an allocation larger than one region,
/// because `alloc` bumps a pointer through a single region and
/// does not span regions. This constant records that limit.
pub const MAX_ALLOCATION_FRAMES: usize = REGION_FRAMES;

/// Kernel heap.
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
    committed: u32,
}

impl KernelHeap {
    /// Creates an uninitialized heap.
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
    /// Returns a null pointer on failure.
    ///
    /// # Safety
    ///
    /// The caller must ensure that no other thread is concurrently
    /// allocating and that interrupts are disabled.
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

        // The request does not fit in the current region. Check
        // whether it can fit in a fresh region.
        //
        // A fresh region starts at a page-aligned (and therefore
        // maximally aligned) address, so no alignment padding is
        // needed for the first allocation from it.
        let frames_needed = ((size as u32 + PAGE_SIZE - 1) / PAGE_SIZE) as usize;

        if frames_needed > MAX_ALLOCATION_FRAMES {
            // The request is too large for the heap to serve, no
            // matter how many regions are installed.
            return core::ptr::null_mut();
        }

        if !unsafe { self.grow() } {
            return core::ptr::null_mut();
        }

        unsafe { self.alloc(layout) }
    }

    /// Frees a block of memory.
    ///
    /// The current implementation is a no-op: the heap is a bump
    /// allocator, and reclaimed bytes are not returned to the free
    /// pool. See the module documentation.
    ///
    /// # Safety
    ///
    /// The pointer and layout must be one previously returned by
    /// `alloc`.
    pub unsafe fn dealloc(&mut self, _ptr: *mut u8, _layout: Layout) {
        // Intentionally empty.
    }

    /// Installs a new region at the end of the heap's virtual
    /// range.
    ///
    /// The region is always [`REGION_FRAMES`] frames.
    ///
    /// # Safety
    ///
    /// Must be called with the fabric's global state accessible.
    unsafe fn grow(&mut self) -> bool {
        unsafe {
            marker(b'G');
        }

        let total_bytes = (REGION_FRAMES as u32) * PAGE_SIZE;

        // The region must fit in the heap's virtual range.
        if self.next_region_va + total_bytes > HEAP_END {
            unsafe {
                marker(b'!');
            }
            return false;
        }

        let region_start = self.next_region_va;

        // Ask the fabric for a memory object large enough for the
        // region.
        let rights = CapabilityRights::MAP | CapabilityRights::READ | CapabilityRights::WRITE;

        let core = crate::capability::core_mut();

        let (object_id, object_cap) = match core.allocate_memory(REGION_FRAMES, rights) {
            Some(result) => {
                unsafe {
                    marker(b'1');
                }
                result
            }
            None => {
                unsafe {
                    marker(b'!');
                }
                return false;
            }
        };

        // Install the object into the kernel's address space.
        if core
            .map_memory(self.address_space, object_cap, region_start, true, false)
            .is_err()
        {
            unsafe {
                marker(b'X');
            }
            core.destroy_object(object_id);
            return false;
        }

        unsafe {
            marker(b'2');
        }

        // Success. Update the heap's state.
        self.current = region_start;
        self.current_region_end = region_start + total_bytes;
        self.next_region_va = region_start + total_bytes;
        self.committed += total_bytes;

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
/// Initialized by [`init`], after the fabric is ready.
static mut HEAP: Option<KernelHeap> = None;

/// Initializes the global heap.
pub fn init(address_space: CapabilityId) {
    unsafe {
        marker(b'I');
        HEAP = Some(KernelHeap::new(address_space));
        marker(b'J');

        // Read back and confirm.
        let heap = &*core::ptr::addr_of!(HEAP);
        match heap {
            Some(_) => marker(b'1'),
            None => marker(b'0'),
        }
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
struct KernelAllocator;

unsafe impl GlobalAlloc for KernelAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        unsafe {
            marker(b'A');
        }

        let ptr = core::ptr::addr_of!(HEAP) as *const u8;
        for i in 0..16 {
            let b = unsafe { core::ptr::read_volatile(ptr.add(i)) };
            let hi = b >> 4;
            let lo = b & 0x0F;
            let hi_ch = if hi < 10 { b'0' + hi } else { b'A' + (hi - 10) };
            let lo_ch = if lo < 10 { b'0' + lo } else { b'A' + (lo - 10) };
            unsafe {
                marker(hi_ch);
            }
            unsafe {
                marker(lo_ch);
            }
            unsafe {
                marker(b' ');
            }
        }
        unsafe {
            marker(b'|');
        }

        unsafe {
            let heap = &mut *core::ptr::addr_of_mut!(HEAP);
            match heap {
                Some(h) => {
                    marker(b'K');

                    h.alloc(layout)
                }
                None => {
                    marker(b'N');

                    core::ptr::null_mut()
                }
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

/// Writes a byte to the QEMU debug port.
unsafe fn marker(byte: u8) {
    unsafe {
        core::arch::asm!(
            "out 0xE9, al",
            in("al") byte,
            options(nostack, preserves_flags),
        );
    }
}
