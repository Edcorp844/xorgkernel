//! The Indirection Table.
//!
//! The ITable is the fabric's capability store. Every capability
//! that exists is a slot in the ITable, and every lookup of a
//! `CapabilityId` goes through it.
//!
//! # Structure
//!
//! The table is a fixed-size array of slots. Each slot records:
//!
//! - whether it is occupied
//! - a generation counter
//! - a free-list link, used only when the slot is unoccupied
//! - the capability stored in it, if any
//!
//! # Generations
//!
//! Each slot has a generation counter that increments whenever the
//! slot is freed. A `CapabilityId` carries both the slot index and
//! the generation at which the capability was created. When the
//! slot is reused, the generation changes, and any old ID naming
//! the slot is rejected. This makes revocation sound: a revoked
//! capability cannot be resurrected by allocating a new one into
//! the same slot.
//!
//! # Free list
//!
//! Allocation is O(1): the table maintains a free list of
//! unoccupied slot indices. `allocate` pops the head; `revoke` and
//! `revoke_object` push the freed slot onto the head.
//!
//! The free list is threaded through the `next_free` field of each
//! slot. It is deliberately a separate field from `generation`, so
//! the two do not alias: a slot's generation is stable across the
//! slot being on the free list, and the free-list link is stable
//! across the slot being allocated.
//!
//! # Bounds
//!
//! The table size is fixed. Capabilities beyond its capacity cannot
//! be created. This is a bootstrap constraint; later, the table
//! will grow on demand and the bound will be removed.
//!
//! # Slot 0
//!
//! Slot 0 is reserved. `CapabilityId::INVALID` is `CapabilityId::new(0, 0)`,
//! and by never handing out slot 0 the table ensures that an ID
//! constructed by `new(0, generation)` cannot accidentally resolve
//! to a real capability. The generation on slot 0 is not
//! meaningful.

use crate::capability::capability::{Capability, CapabilityId, CapabilityRights};
use crate::capability::object::ObjectId;

/// Number of slots in the ITable.
const ITABLE_SIZE: usize = 1024;

/// Sentinel marking the end of the free list.
///
/// Chosen as `u16::MAX` because it is larger than any valid slot
/// index (`ITABLE_SIZE - 1 = 1023`), so it cannot collide with a
/// real index.
const FREE_LIST_END: u16 = u16::MAX;

/// One ITable slot.
#[repr(C)]
pub struct ITableEntry {
    /// Generation of this slot.
    ///
    /// Incremented on every free. Never zero: a generation of zero
    /// would make the ID look like `CapabilityId::INVALID`.
    generation: u16,

    /// Next free slot index, used only when `occupied` is false.
    ///
    /// Set to `FREE_LIST_END` when this slot is the last on the
    /// free list. Not meaningful when the slot is occupied.
    next_free: u16,

    /// Whether this slot currently holds a capability.
    occupied: bool,

    /// The capability in this slot, if occupied.
    capability: Option<Capability>,
}

impl ITableEntry {
    /// An empty entry with generation 1.
    const fn empty() -> Self {
        Self {
            generation: 1,
            next_free: FREE_LIST_END,
            occupied: false,
            capability: None,
        }
    }
}

/// The fabric's capability store.
pub struct ITable {
    /// Fixed-size array of slots.
    entries: [ITableEntry; ITABLE_SIZE],

    /// Head of the free list: index of the first free slot.
    ///
    /// `FREE_LIST_END` means the table is full.
    free_head: u16,
}

impl ITable {
    /// Creates an empty ITable with every slot except slot 0 on
    /// the free list.
    ///
    /// Slot 0 is reserved. Because `CapabilityId::INVALID` is
    /// `CapabilityId::new(0, 0)`, reserving slot 0 means a zeroed
    /// ID cannot resolve to a real capability.
    ///
    /// The free list is constructed at compile time. Iteration is
    /// in reverse so the lowest-numbered free slot ends up at the
    /// head, which makes the allocation order predictable for
    /// debugging.
    pub const fn new() -> Self {
        let mut table = Self {
            entries: [const { ITableEntry::empty() }; ITABLE_SIZE],
            free_head: FREE_LIST_END,
        };

        let mut i = ITABLE_SIZE;

        while i > 1 {
            i -= 1;

            table.entries[i].next_free = table.free_head;
            table.free_head = i as u16;
        }

        table
    }

    /// Allocates a new capability slot.
    ///
    /// O(1): pops the head of the free list.
    ///
    /// Returns `None` if the table is full.
    ///
    /// The capability's generation is taken from the slot, which
    /// is incremented on every revoke. A capability allocated into
    /// a previously-revoked slot therefore has a fresh generation,
    /// and the old ID for that slot no longer resolves.
    pub fn allocate(&mut self, object: ObjectId, rights: CapabilityRights) -> Option<CapabilityId> {
        if self.free_head == FREE_LIST_END {
            return None;
        }

        let index = self.free_head;
        let entry = &mut self.entries[index as usize];

        self.free_head = entry.next_free;

        let id = CapabilityId::new(index, entry.generation);

        entry.next_free = FREE_LIST_END;
        entry.capability = Some(Capability::new(id, object, rights));
        entry.occupied = true;

        Some(id)
    }

    /// Looks up a capability by ID.
    ///
    /// Returns `None` if:
    ///
    /// - the slot index is out of range
    /// - the slot is unoccupied
    /// - the generation does not match
    /// - the slot is marked occupied but holds no capability (a
    ///   state that should not occur and indicates corruption)
    pub fn lookup(&self, id: CapabilityId) -> Option<&Capability> {
        let index = id.index();

        if index >= ITABLE_SIZE {
            return None;
        }

        let entry = &self.entries[index];

        if !entry.occupied {
            return None;
        }

        if entry.generation != id.generation() {
            return None;
        }

        entry.capability.as_ref()
    }

    /// Revokes the capability with the given ID.
    ///
    /// Returns `true` if the capability existed and was revoked.
    /// The slot is pushed onto the free list and its generation is
    /// incremented, so the revoked ID can no longer be resolved.
    pub fn revoke(&mut self, id: CapabilityId) -> bool {
        let index = id.index();

        if index >= ITABLE_SIZE {
            return false;
        }

        let entry = &mut self.entries[index];

        if !entry.occupied {
            return false;
        }

        if entry.generation != id.generation() {
            return false;
        }

        entry.occupied = false;
        entry.capability = None;

        // Bump generation. Wrap to 1 if it would become 0, since
        // generation 0 collides with `CapabilityId::INVALID`.
        entry.generation = entry.generation.wrapping_add(1);

        if entry.generation == 0 {
            entry.generation = 1;
        }

        // Push onto the free list.
        entry.next_free = self.free_head;
        self.free_head = index as u16;

        true
    }

    /// Revokes every capability referring to `object`.
    ///
    /// Returns the number of capabilities revoked.
    ///
    /// This is the fabric's revocation primitive: destroying an
    /// object must revoke every capability to it, so that no cell
    /// can hold a stale reference after the object is gone.
    ///
    /// The scan is O(n) in the table size. This is acceptable for
    /// the current bootstrap sequence, where destruction is rare.
    /// Once the fabric is on a hot path, the object registry will
    /// maintain a list of capabilities per object, making this
    /// O(k) in the number of capabilities to the object.
    pub fn revoke_object(&mut self, object: ObjectId) -> usize {
        let mut revoked = 0;

        for index in 0..ITABLE_SIZE {
            let matches = {
                let entry = &self.entries[index];

                if !entry.occupied {
                    false
                } else {
                    match entry.capability.as_ref() {
                        Some(capability) => capability.object() == object,
                        None => false,
                    }
                }
            };

            if matches {
                let entry = &mut self.entries[index];

                entry.occupied = false;
                entry.capability = None;

                entry.generation = entry.generation.wrapping_add(1);

                if entry.generation == 0 {
                    entry.generation = 1;
                }

                entry.next_free = self.free_head;
                self.free_head = index as u16;

                revoked += 1;
            }
        }

        revoked
    }

    /// Returns `(object, rights)` for a live capability, or `None`
    /// if the ID does not resolve.
    ///
    /// This is a convenience wrapper over `lookup` that copies the
    /// fields out, so the caller does not hold a borrow on the table
    /// when it subsequently calls `allocate` or `revoke`.
    ///
    /// The affine-move path needs this: it must read the source's
    /// object and rights, then mutate the table twice. Borrowing
    /// `&Capability` across those mutations is not possible.
    pub fn lookup_parts(&self, id: CapabilityId) -> Option<(ObjectId, CapabilityRights)> {
        self.lookup(id).map(|cap| (cap.object(), cap.rights()))
    }

    /// Returns the number of live capabilities referring to `object`.
    ///
    /// O(n) in the table size. Provided for tests and for the
    /// affine-invariant assertion; not on the hot path.
    ///
    /// Once the per-object capability list exists, this becomes O(k).
    pub fn count_capabilities_to(&self, object: ObjectId) -> usize {
        self.entries
            .iter()
            .filter(|entry| {
                entry.occupied
                    && entry
                        .capability
                        .as_ref()
                        .map(|cap| cap.object() == object)
                        .unwrap_or(false)
            })
            .count()
    }
}
