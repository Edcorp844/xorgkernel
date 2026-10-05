use crate::capability::object::ObjectId;

#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CapabilityId(u32);

impl CapabilityId {
    pub const INVALID: Self = Self(0);

    pub const fn new(index: u16, generation: u16) -> Self {
        Self(((generation as u32) << 16) | index as u32)
    }

    pub const fn index(self) -> usize {
        (self.0 & 0xffff) as usize
    }

    pub const fn generation(self) -> u16 {
        (self.0 >> 16) as u16
    }

    pub const fn raw(self) -> u32 {
        self.0
    }
}

#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct CapabilityRights(u32);

impl CapabilityRights {
    pub const READ: Self = Self(1 << 0);
    pub const WRITE: Self = Self(1 << 1);
    pub const EXECUTE: Self = Self(1 << 2);
    pub const GRANT: Self = Self(1 << 3);

    pub const fn empty() -> Self {
        Self(0)
    }

    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    pub const fn intersect(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    pub const fn bits(self) -> u32 {
        self.0
    }
}

impl core::ops::BitOr for CapabilityRights {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl core::ops::BitAnd for CapabilityRights {
    type Output = Self;

    fn bitand(self, rhs: Self) -> Self {
        Self(self.0 & rhs.0)
    }
}

#[repr(C)]
pub struct Capability {
    id: CapabilityId,
    object: ObjectId,
    rights: CapabilityRights,
}

impl Capability {
    pub const fn new(id: CapabilityId, object: ObjectId, rights: CapabilityRights) -> Self {
        Self { id, object, rights }
    }

    pub const fn id(&self) -> CapabilityId {
        self.id
    }

    pub const fn object(&self) -> ObjectId {
        self.object
    }

    pub const fn rights(&self) -> CapabilityRights {
        self.rights
    }
}
