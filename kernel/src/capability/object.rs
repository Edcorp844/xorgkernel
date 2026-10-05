//! Object identifiers and kinds.
//!
//! Every object managed by the fabric has an `ObjectId` assigned by
//! the registry. The ID is opaque: callers pass it around but do
//! not interpret it. The registry keeps the mapping from ID to
//! kind, so that when a capability is resolved, the fabric knows
//! which set of rights is meaningful for the underlying object.
//!
//! The ID is deliberately not a pointer. A pointer would leak the
//! object's physical location and allow an attacker who guesses
//! the address to bypass the fabric. An ID is an indirection: to
//! reach the object, you must go through the registry, and to
//! reach the registry, you must hold a capability.

/// Identifies an object managed by the fabric.
///
/// Assigned by `ObjectRegistry::create`. The value 0 is reserved
/// and never assigned; it is used as a sentinel for "no object."
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ObjectId(u32);

impl ObjectId {
    /// The reserved invalid ID.
    pub const INVALID: Self = Self(0);

    /// Creates an object ID with the given raw value.
    ///
    /// The caller is responsible for ensuring the value is one that
    /// the registry has actually issued. In practice, this
    /// constructor is only used by the registry itself.
    pub const fn new(id: u32) -> Self {
        Self(id)
    }

    /// Returns the raw numeric value of the ID.
    ///
    /// Used for logging and for the registry's internal bookkeeping.
    pub const fn raw(self) -> u32 {
        self.0
    }

    /// Returns whether this ID is anything other than `INVALID`.
    pub const fn is_valid(self) -> bool {
        self.0 != 0
    }
}

/// The kind of object that an `ObjectId` refers to.
///
/// The fabric uses the kind to decide which rights are meaningful
/// for a capability and which operations the capability permits.
/// For example, a `READ` right on a memory object means "read its
/// contents," while a `READ` right on a cell would have no defined
/// meaning.
///
/// New kinds are added as the fabric grows. The `#[non_exhaustive]`
/// attribute is not used because the kernel is built as a single
/// crate and all matches on `ObjectKind` must be updated whenever
/// a variant is added. This is intentional: it forces the
/// maintainer to consider each match site when introducing a new
/// object kind.
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ObjectKind {
    /// A region of physical memory, backed by one or more frames.
    ///
    /// Rights that are meaningful: `MAP`, `READ`, `WRITE`,
    /// `EXECUTE`, `SHARE`, `DESTROY`.
    MemoryObject,

    /// A virtual address space.
    ///
    /// Rights that are meaningful: `MAP`, `UNMAP`, `ACTIVATE`,
    /// `SHARE`, `DESTROY`.
    AddressSpace,

    /// An execution context.
    ///
    /// Rights that are meaningful: `ENTER`, `GRANT`, `REVOKE`,
    /// `SHARE`, `DESTROY`.
    Cell,
}
