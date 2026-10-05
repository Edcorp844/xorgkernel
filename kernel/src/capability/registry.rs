//! Object registry.
//!
//! The registry is the fabric's source of truth for what objects
//! exist. Every object created by the fabric is recorded here, and
//! every lookup of an object ID goes through the registry.
//!
//! # Structure
//!
//! The registry is a fixed-size table of entries, one per object
//! slot. Each entry records:
//!
//! - whether the slot is occupied
//! - the object's kind
//! - the object's ID
//! - a generation counter
//!
//! The generation counter guards against stale IDs. When a slot is
//! freed and later reused, the generation is incremented, so an ID
//! from before the free is rejected even though it names the same
//! slot.
//!
//! # Complexity
//!
//! `create` is currently O(n): it scans for the first free slot.
//! `lookup` is O(n): it scans for the ID. Neither is on the hot
//! path in the current boot sequence; once the fabric is on the
//! hot path, both will need to become O(1) via free lists and a
//! direct slot-indexed representation.
//!
//! # Bounds
//!
//! The registry size is fixed at compile time. Objects beyond the
//! table's capacity cannot be created. This is a bootstrap
//! constraint; later, the registry will grow on demand and the
//! bound will be removed.

use crate::capability::object::{ObjectId, ObjectKind};

/// Number of slots in the registry.
///
/// Each slot records one object. Objects beyond this many cannot
/// be created until the registry is made dynamic.
const OBJECT_REGISTRY_SIZE: usize = 1024;

/// One registry entry.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ObjectEntry {
    /// Whether this slot currently holds an object.
    occupied: bool,

    /// Kind of the object in this slot.
    ///
    /// Undefined when `occupied` is false.
    kind: ObjectKind,

    /// Generation counter for the slot.
    ///
    /// Incremented every time the slot is freed. Used to reject
    /// stale IDs whose slot has been reused.
    generation: u16,

    /// The object's ID, or `ObjectId::INVALID` if unoccupied.
    object: ObjectId,
}

impl ObjectEntry {
    /// An empty entry with generation 1.
    const fn empty() -> Self {
        Self {
            occupied: false,
            kind: ObjectKind::MemoryObject,
            generation: 1,
            object: ObjectId::INVALID,
        }
    }
}

/// Registry of all objects managed by the fabric.
pub struct ObjectRegistry {
    /// Fixed-size table of entries.
    entries: [ObjectEntry; OBJECT_REGISTRY_SIZE],

    /// Value to assign to the next created object's ID.
    ///
    /// Starts at 1 (0 is `INVALID`) and increments monotonically.
    /// Wraps to 1 if it overflows; this is safe because the
    /// generation counter also guards against stale IDs.
    next_id: u32,
}

impl ObjectRegistry {
    /// Creates an empty registry.
    pub const fn new() -> Self {
        Self {
            entries: [const { ObjectEntry::empty() }; OBJECT_REGISTRY_SIZE],
            next_id: 1,
        }
    }

    /// Creates a new object of the given kind.
    ///
    /// Returns `None` if the registry is full.
    ///
    /// The object's ID is chosen from `next_id`. The ID is opaque;
    /// callers use it only to look the object up again.
    pub fn create(&mut self, kind: ObjectKind) -> Option<ObjectId> {
        for entry in self.entries.iter_mut() {
            if !entry.occupied {
                let id = ObjectId::new(self.next_id);

                self.next_id = self.next_id.wrapping_add(1);

                if self.next_id == 0 {
                    self.next_id = 1;
                }

                entry.kind = kind;
                entry.object = id;
                entry.occupied = true;

                return Some(id);
            }
        }

        None
    }

    /// Returns the object ID if it names a live object.
    pub fn lookup(&self, id: ObjectId) -> Option<ObjectId> {
        if !id.is_valid() {
            return None;
        }

        for entry in &self.entries {
            if entry.occupied && entry.object == id {
                return Some(entry.object);
            }
        }

        None
    }

    /// Returns the kind of the object named by `id`, if it exists.
    ///
    /// Used by the fabric to decide which rights are meaningful for
    /// a capability and which operations the capability permits.
    pub fn kind(&self, id: ObjectId) -> Option<ObjectKind> {
        if !id.is_valid() {
            return None;
        }

        for entry in &self.entries {
            if entry.occupied && entry.object == id {
                return Some(entry.kind);
            }
        }

        None
    }

    /// Destroys the object named by `id`.
    ///
    /// Returns `true` if the object existed and was destroyed.
    ///
    /// This does not free the object's resources; that is the
    /// caller's responsibility. The registry only tracks identity.
    /// In the current design, the fabric's `destroy_object` is the
    /// entry point that revokes capabilities and then calls this.
    pub fn destroy(&mut self, id: ObjectId) -> bool {
        if !id.is_valid() {
            return false;
        }

        for entry in self.entries.iter_mut() {
            if entry.occupied && entry.object == id {
                entry.occupied = false;
                entry.object = ObjectId::INVALID;
                entry.generation = entry.generation.wrapping_add(1);

                if entry.generation == 0 {
                    entry.generation = 1;
                }

                return true;
            }
        }

        false
    }
}
