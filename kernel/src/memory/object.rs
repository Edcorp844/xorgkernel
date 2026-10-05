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
//! `CapabilityCore::allocate_memory`, which:
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
//! # Bounds
//!
//! A memory object currently holds at most `MAX_FRAMES` frames.
//! This is a bootstrap constraint; the eventual design stores the
//! frames in a slab or a linked list and has no fixed upper bound.
//! The bound exists now because the object's frame storage is
//! inline and the kernel has no heap yet.

use crate::capability::object::ObjectId;
use crate::memory::frame::{self, Frame};

/// Maximum number of frames a single memory object can hold.
///
/// 64 frames = 256 KiB. The bound exists because the object stores
/// its frames inline; later, the storage will be dynamic.
pub const MAX_FRAMES: usize = 64;

/// A bounded region of physical frames.
///
/// See the module documentation for the object's role in the
/// capability fabric.
pub struct MemoryObject {
    /// The fabric's ID for this object.
    ///
    /// Assigned by the registry when the object is created.
    id: ObjectId,

    /// The frames backing this object.
    ///
    /// Only the first `frame_count` entries are meaningful; the
    /// rest are `None`.
    frames: [Option<Frame>; MAX_FRAMES],

    /// Number of frames actually held.
    frame_count: usize,
}

impl MemoryObject {
    /// Creates a memory object holding `pages` freshly-allocated
    /// frames.
    ///
    /// Returns `None` if:
    ///
    /// - `pages` is 0
    /// - `pages` exceeds `MAX_FRAMES`
    /// - the frame allocator cannot provide enough frames
    ///
    /// On failure, any frames already allocated are returned to the
    /// frame allocator before returning, so the caller does not
    /// leak.
    ///
    /// The object is not yet registered. Registration is the
    /// fabric's responsibility and happens in
    /// `CapabilityCore::allocate_memory`.
    pub fn new(pages: usize) -> Option<Self> {
        if pages == 0 || pages > MAX_FRAMES {
            return None;
        }

        let mut frames = [None; MAX_FRAMES];

        for i in 0..pages {
            match frame::allocate() {
                Some(f) => frames[i] = Some(f),
                None => {
                    // Roll back anything already allocated.
                    for frame in frames.iter_mut().take(i) {
                        if let Some(f) = frame.take() {
                            frame::free(f);
                        }
                    }

                    return None;
                }
            }
        }

        Some(Self {
            id: ObjectId::INVALID,
            frames,
            frame_count: pages,
        })
    }

    /// Returns the number of frames held by this object.
    pub fn page_count(&self) -> usize {
        self.frame_count
    }

    /// Returns the frame at index `i`, if it exists.
    ///
    /// Used by the fabric to install mappings and by the frame
    /// allocator to release frames on object destruction.
    pub fn frame(&self, i: usize) -> Option<Frame> {
        if i >= self.frame_count {
            return None;
        }

        self.frames[i]
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
    /// Called by the fabric during registration. Not public: the
    /// caller must go through `CapabilityCore::allocate_memory`.
    pub(crate) fn set_id(&mut self, id: ObjectId) {
        self.id = id;
    }
}

impl Drop for MemoryObject {
    /// Returns every frame to the frame allocator when the object
    /// is destroyed.
    ///
    /// The fabric destroys a memory object when its last capability
    /// is revoked and the object itself is destroyed. The order is:
    ///
    /// 1. `CapabilityCore::destroy_object(id)` revokes every
    ///    capability referring to the object.
    /// 2. The registry marks the slot free.
    /// 3. The `MemoryObject` is dropped, and this `Drop` returns
    ///    its frames.
    ///
    /// Because capabilities are revoked before the object is
    /// dropped, no cell can be holding a stale reference when the
    /// frames are freed.
    fn drop(&mut self) {
        for frame in self.frames.iter_mut().take(self.frame_count) {
            if let Some(f) = frame.take() {
                frame::free(f);
            }
        }
    }
}
