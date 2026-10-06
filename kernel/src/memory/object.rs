//! Memory objects.
//!
//! A memory object is a bounded, fabric-managed region of physical
//! frames. It is the unit of authority for memory in the capability
//! model: allocating memory means creating a memory object, and
//! sharing memory means transferring a capability to one.
//!
//! # Relationship to capabilities
//!
//! A memory object is created through
//! [`crate::capability::core::CapabilityCore::allocate_memory`],
//! which:
//!
//! 1. Allocates N frames from the frame allocator.
//! 2. Creates a `MemoryObject` holding those frames.
//! 3. Registers the object in the object registry.
//! 4. Returns a capability with the requested rights.
//!
//! The caller never sees the object directly. It sees a capability.
//! To map, read, write, or transfer the object, the caller
//! presents the capability, and the fabric checks it.
//!
//! # Storage
//!
//! A memory object's frames are stored in one of two forms,
//! depending on the object's size:
//!
//! - **Inline**, for objects of at most [`INLINE_FRAMES`] frames.
//!   The frame list is a fixed-size array inside the object. No
//!   heap allocation is needed.
//!
//! - **Heap**, for larger objects. The frame list is a
//!   heap-allocated `Vec<Frame>`.
//!
//! The two-tier design exists to break a bootstrap cycle. The
//! kernel heap is itself backed by memory objects: when the heap
//! needs to grow, it asks the fabric for a memory object and
//! installs it into the heap's virtual range. If allocating a
//! memory object required a heap allocation — as it would if every
//! memory object used `Vec<Frame>` — then the very first heap
//! growth would deadlock: the fabric would ask the heap for
//! memory, the heap would ask the fabric for a memory object, and
//! so on.
//!
//! By keeping small objects inline, the heap's own growth (which
//! always requests small objects — see `heap::MIN_GROW_FRAMES`)
//! never needs the heap. The cycle is broken.
//!
//! # Lifetime
//!
//! A `MemoryObject` owns its frames. When it is dropped, every
//! frame is returned to the frame allocator. The fabric drops the
//! object when the last capability to it is revoked and the
//! object's own `destroy_object` is called.
//!
//! Because capabilities are revoked before the object is dropped,
//! no cell can be holding a stale reference when the frames are
//! freed. The order matters:
//!
//! 1. `CapabilityCore::destroy_object(id)` revokes every
//!    capability referring to the object.
//! 2. The registry marks the slot free.
//! 3. The `MemoryObject` is dropped, and its `Drop` returns its
//!    frames.

use alloc::vec::Vec;

use crate::capability::object::ObjectId;
use crate::memory::frame::{self, Frame};

/// Number of frames stored inline before falling back to a heap
/// allocation.
///
/// Objects with at most this many frames use an inline array and
/// do not touch the heap. Larger objects use a `Vec<Frame>`.
///
/// The value must be large enough to hold the heap's own growth
/// requests (see `heap::MIN_GROW_FRAMES`), otherwise the bootstrap
/// cycle described in the module documentation reappears.
///
/// 16 frames = 64 KiB, which is comfortably above the minimum
/// region the heap installs, and small enough that the inline
/// array adds only 64 bytes to each `MemoryObject`.
pub const INLINE_FRAMES: usize = 256;

/// Storage for a memory object's frames.
///
/// The variant used depends on the object's size. See the module
/// documentation for the rationale.
enum FrameStorage {
    /// Frames stored inline. Used for objects of at most
    /// [`INLINE_FRAMES`] frames.
    ///
    /// Only the first `count` entries of `frames` are meaningful;
    /// the rest are `None`.
    Inline {
        frames: [Option<Frame>; INLINE_FRAMES],
        count: usize,
    },

    /// Frames stored in a heap-allocated vector. Used for objects
    /// larger than [`INLINE_FRAMES`].
    Heap(Vec<Frame>),
}

/// A bounded region of physical frames.
///
/// See the module documentation for the object's role in the
/// capability fabric.
pub struct MemoryObject {
    /// The fabric's ID for this object.
    ///
    /// Assigned by the registry when the object is created through
    /// the fabric. `ObjectId::INVALID` before registration.
    id: ObjectId,

    /// The frames backing this object.
    storage: FrameStorage,
}

impl MemoryObject {
    /// Creates a memory object holding `pages` freshly-allocated
    /// frames.
    ///
    /// The frames are allocated one at a time from the frame
    /// allocator. They are not required to be physically
    /// contiguous: a memory object's frames are visible to code
    /// only through virtual mappings, which the fabric installs
    /// one page at a time.
    ///
    /// Returns `None` if:
    ///
    /// - `pages` is 0
    /// - the frame allocator cannot provide enough frames
    ///
    /// On failure, any frames already allocated are returned to
    /// the frame allocator before returning. No partial state is
    /// left behind.
    ///
    /// The object is not yet registered. Registration is the
    /// fabric's responsibility and happens in
    /// [`crate::capability::core::CapabilityCore::allocate_memory`].
    pub fn new(pages: usize) -> Option<Self> {
        if pages == 0 {
            return None;
        }

        if pages <= INLINE_FRAMES {
            Self::new_inline(pages)
        } else {
            Self::new_heap(pages)
        }
    }

    /// Creates a small memory object using inline frame storage.
    ///
    /// On allocation failure, rolls back any frames already
    /// allocated.
    fn new_inline(pages: usize) -> Option<Self> {
        let mut frames = [None; INLINE_FRAMES];

        for i in 0..pages {
            match frame::allocate() {
                Some(f) => frames[i] = Some(f),
                None => {
                    // Roll back the frames we already took.
                    for slot in frames.iter_mut().take(i) {
                        if let Some(f) = slot.take() {
                            frame::free(f);
                        }
                    }
                    return None;
                }
            }
        }

        Some(Self {
            id: ObjectId::INVALID,
            storage: FrameStorage::Inline {
                frames,
                count: pages,
            },
        })
    }

    /// Creates a large memory object using a heap-allocated frame
    /// vector.
    ///
    /// On allocation failure, rolls back any frames already
    /// allocated.
    ///
    /// # Bootstrap note
    ///
    /// This variant allocates from the kernel heap. If the heap is
    /// empty when a large object is requested, the heap will grow,
    /// which calls back into the fabric to allocate a *small*
    /// memory object (which uses inline storage and does not
    /// recurse). Once the heap has space, the `Vec` allocation
    /// here succeeds and the outer object is completed.
    ///
    /// See the module documentation for the full cycle analysis.
    fn new_heap(pages: usize) -> Option<Self> {
        let mut frames: Vec<Frame> = Vec::with_capacity(pages);

        for _ in 0..pages {
            match frame::allocate() {
                Some(f) => frames.push(f),
                None => {
                    // Roll back: free every frame we already took.
                    // The `Vec` itself is dropped by Rust; we only
                    // need to return the frames to the frame
                    // allocator.
                    for f in frames {
                        frame::free(f);
                    }
                    return None;
                }
            }
        }

        Some(Self {
            id: ObjectId::INVALID,
            storage: FrameStorage::Heap(frames),
        })
    }

    /// Returns the number of frames held by this object.
    pub fn page_count(&self) -> usize {
        match &self.storage {
            FrameStorage::Inline { count, .. } => *count,
            FrameStorage::Heap(frames) => frames.len(),
        }
    }

    /// Returns the frame at index `i`, if it exists.
    ///
    /// Used by the fabric to install mappings and by the frame
    /// allocator to release frames on object destruction.
    pub fn frame(&self, i: usize) -> Option<Frame> {
        match &self.storage {
            FrameStorage::Inline { frames, count } => {
                if i >= *count {
                    None
                } else {
                    frames[i]
                }
            }
            FrameStorage::Heap(frames) => frames.get(i).copied(),
        }
    }

    /// Returns the fabric ID assigned to this object.
    ///
    /// Valid only after the fabric has registered the object.
    /// Before registration this returns `ObjectId::INVALID`.
    pub fn id(&self) -> ObjectId {
        self.id
    }

    /// Sets the fabric ID.
    ///
    /// Called by the fabric during registration. Not public outside
    /// the crate: the caller must go through
    /// [`crate::capability::core::CapabilityCore::allocate_memory`].
    pub(crate) fn set_id(&mut self, id: ObjectId) {
        self.id = id;
    }
}

impl Drop for MemoryObject {
    /// Returns every frame to the frame allocator when the object
    /// is destroyed.
    ///
    /// See the module documentation for the ordering guarantee
    /// that makes this safe: capabilities are revoked before the
    /// object is dropped, so no cell holds a stale reference.
    fn drop(&mut self) {
        match &mut self.storage {
            FrameStorage::Inline { frames, count } => {
                for slot in frames.iter_mut().take(*count) {
                    if let Some(f) = slot.take() {
                        frame::free(f);
                    }
                }
            }
            FrameStorage::Heap(frames) => {
                for f in frames.drain(..) {
                    frame::free(f);
                }
            }
        }
    }
}
