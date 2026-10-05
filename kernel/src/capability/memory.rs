use crate::capability::object::ObjectId;

pub const PAGE_SIZE: usize = 4096;

#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct MemoryFlags(u32);

impl MemoryFlags {
    pub const READ: Self = Self(1 << 0);
    pub const WRITE: Self = Self(1 << 1);
    pub const EXECUTE: Self = Self(1 << 2);
    pub const SHARED: Self = Self(1 << 3);

    pub const fn empty() -> Self {
        Self(0)
    }

    pub const fn bits(self) -> u32 {
        self.0
    }

    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }
}

impl core::ops::BitOr for MemoryFlags {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

#[repr(C)]
pub struct MemoryObject {
    object_id: ObjectId,
    base_frame: usize,
    frame_count: usize,
    flags: MemoryFlags,
}

impl MemoryObject {
    pub const fn new(
        object_id: ObjectId,
        base_frame: usize,
        frame_count: usize,
        flags: MemoryFlags,
    ) -> Self {
        Self {
            object_id,
            base_frame,
            frame_count,
            flags,
        }
    }

    pub const fn id(&self) -> ObjectId {
        self.object_id
    }

    pub const fn base_frame(&self) -> usize {
        self.base_frame
    }

    pub const fn frame_count(&self) -> usize {
        self.frame_count
    }

    pub const fn flags(&self) -> MemoryFlags {
        self.flags
    }

    pub const fn size(&self) -> usize {
        self.frame_count * PAGE_SIZE
    }
}