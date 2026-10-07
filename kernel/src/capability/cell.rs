//! Execution cell capability namespaces.
//!
//! A cell is the unit of authority in the fabric: it holds a set
//! of capability IDs and represents the authority of one
//! execution context. The fabric reaches a cell's capabilities
//! through this module's [`Cell`] type.
//!
//! # What a cell is not
//!
//! A cell does **not** create capabilities. It is a namespace: a
//! cell records which global capability IDs the execution context
//! is permitted to possess. Creating a capability is
//! [`CapabilityCore::allocate`]'s job; placing one in a cell is
//! [`CapabilityCore::grant_capability`]'s. A cell's only role is
//! to answer the question "does this execution context hold this
//! capability?"
//!
//! # The borrow state array
//!
//! Each capability slot in a cell has a [`BorrowState`]. The state
//! tracks whether the capability is currently owned by the cell
//! or lent out to another cell for the duration of an invocation.
//!
//! Only two states exist:
//!
//! - **`Owned`** — the cell holds the capability normally. It may
//!   be moved, copied (as a leaf), or used.
//!
//! - **`BorrowedOut`** — the cell has lent the capability to
//!   another cell. It cannot be moved, copied, or used until the
//!   borrower returns it. On return, the state reverts to
//!   `Owned`.
//!
//! There is deliberately **no** `BorrowedIn` state. The
//! architecture places borrowed capabilities in the IPC channel
//! as part of the borrow message, not in the borrower's cell. The
//! borrower holds the capability ID for the duration of the
//! invocation, but the ID's residence is the message, not a slot
//! in the borrower's namespace. See `capability/channel.rs` for
//! the rationale.
//!
//! The consequence is that `BorrowState` has exactly two
//! variants and the state machine per slot is a toggle:
//!
//! ```text
//!   Owned ───── borrow_out ─────► BorrowedOut
//!     ▲                                 │
//!     └──────── revert ────────────────┘
//! ```
//!
//! A slot that is `BorrowedOut` cannot transition to `BorrowedOut`
//! again. A slot that is `Owned` cannot transition to `Owned` by
//! a borrow operation. The two operations that move the state,
//! `borrow_out` and `revert_to_owned`, are the only ones that
//! touch it.

use crate::capability::capability::CapabilityId;

/// Maximum number of capability slots in a cell.
///
/// A cell cannot hold more than this many capabilities. When the
/// limit is reached, further grants fail; the caller must remove
/// something first. The limit is a fixed array bound, not a policy
/// choice: it exists because a cell is a statically-sized
/// structure, and the fabric's design does not use a heap for its
/// capability bookkeeping.
pub const MAX_CELL_CAPABILITIES: usize = 256;

/// Identifies a cell.
///
/// Assigned by [`CapabilityCore::create_cell`]. The value 0 is
/// reserved and never assigned; it is used as a sentinel for "no
/// cell."
///
/// [`CapabilityCore::create_cell`]:
///     crate::capability::core::CapabilityCore::create_cell
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CellId(u32);

impl CellId {
    /// The reserved invalid ID.
    pub const INVALID: Self = Self(0);

    /// Creates a cell ID with the given raw value.
    ///
    /// The caller is responsible for ensuring the value is one
    /// that the fabric has issued. In practice, this constructor
    /// is only used by the fabric itself.
    pub const fn new(id: u32) -> Self {
        Self(id)
    }

    /// Returns the raw numeric value of the ID.
    pub const fn raw(self) -> u32 {
        self.0
    }

    /// Returns whether this ID is anything other than `INVALID`.
    pub const fn is_valid(self) -> bool {
        self.0 != 0
    }
}

/// The borrow state of a capability slot.
///
/// See the module documentation for the state machine and why
/// there are only two variants.
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BorrowState {
    /// The cell holds the capability normally. It may be moved,
    /// copied (as a leaf), or used as the argument to any fabric
    /// operation that accepts it.
    Owned,

    /// The cell has lent the capability to another cell. The slot
    /// is frozen: it cannot be the source of a move or copy, and
    /// it cannot be used as the argument to any fabric operation
    /// that requires ownership. It reverts to `Owned` when the
    /// borrower returns it, or when the borrower's task exits
    /// without returning (in which case the borrow is broken and
    /// the slot is released).
    BorrowedOut,
}

/// An execution cell's capability namespace.
///
/// See the module documentation for the cell's role and the
/// borrow state array.
pub struct Cell {
    /// The cell's identifier.
    id: CellId,

    /// The cell's local capability namespace.
    ///
    /// This does NOT create new capabilities. It records which
    /// global capability IDs this cell is permitted to possess.
    ///
    /// Slots `0..capability_count` are valid. Slots at or above
    /// `capability_count` hold [`CapabilityId::INVALID`] and must
    /// not be read as if they were live.
    capabilities: [CapabilityId; MAX_CELL_CAPABILITIES],

    /// The borrow state of each capability slot.
    ///
    /// `borrow_state[i]` is the state of `capabilities[i]`. The
    /// two arrays are kept in lockstep: every method that adds or
    /// removes a capability maintains both.
    borrow_state: [BorrowState; MAX_CELL_CAPABILITIES],

    /// Number of valid entries in `capabilities` and
    /// `borrow_state`.
    ///
    /// Slots `0..capability_count` are valid; the rest are
    /// uninitialized and must not be read.
    capability_count: usize,
}

impl Cell {
    /// Creates an empty cell with the given ID.
    ///
    /// The cell has no capabilities and no borrowed slots.
    pub const fn new(id: CellId) -> Self {
        Self {
            id,
            capabilities: [CapabilityId::INVALID; MAX_CELL_CAPABILITIES],
            borrow_state: [BorrowState::Owned; MAX_CELL_CAPABILITIES],
            capability_count: 0,
        }
    }

    /// Returns the cell's ID.
    pub const fn id(&self) -> CellId {
        self.id
    }

    /// Adds a capability to the cell's namespace.
    ///
    /// The new slot's borrow state is `Owned`.
    ///
    /// # Return value
    ///
    /// - `true` if the capability was added, or if the cell
    ///   already held it (in which case nothing changed).
    /// - `false` if the capability is [`CapabilityId::INVALID`],
    ///   or if the cell is at capacity.
    pub fn add_capability(&mut self, capability: CapabilityId) -> bool {
        if capability == CapabilityId::INVALID {
            return false;
        }

        // Don't insert duplicates.
        if self.has_capability(capability) {
            return true;
        }

        if self.capability_count >= MAX_CELL_CAPABILITIES {
            return false;
        }

        self.capabilities[self.capability_count] = capability;
        self.borrow_state[self.capability_count] = BorrowState::Owned;
        self.capability_count += 1;

        true
    }

    /// Returns whether the cell holds the given capability.
    ///
    /// This reports the presence of the capability ID in the
    /// cell's namespace. It does not consult the fabric to check
    /// whether the ID still resolves; a cell may hold an ID whose
    /// ITable slot has been revoked, in which case the ID is
    /// stale. Callers that need a live capability should resolve
    /// the ID through [`CapabilityCore::lookup`].
    ///
    /// [`CapabilityCore::lookup`]:
    ///     crate::capability::core::CapabilityCore::lookup
    pub fn has_capability(&self, capability: CapabilityId) -> bool {
        for index in 0..self.capability_count {
            if self.capabilities[index] == capability {
                return true;
            }
        }

        false
    }

    /// Returns the borrow state of the given capability, if the
    /// cell holds it.
    ///
    /// Returns `None` if the capability is not in the cell.
    pub fn borrow_state(&self, capability: CapabilityId) -> Option<BorrowState> {
        for index in 0..self.capability_count {
            if self.capabilities[index] == capability {
                return Some(self.borrow_state[index]);
            }
        }

        None
    }

    /// Marks a capability as `BorrowedOut`.
    ///
    /// The capability must be present in the cell and currently
    /// `Owned`. The transition to `BorrowedOut` is the first step
    /// of a borrow; the second is placing the borrow message in
    /// the channel.
    ///
    /// # Return value
    ///
    /// - `true` if the transition succeeded.
    /// - `false` if the capability is not in the cell, or if its
    ///   state is already `BorrowedOut` (nested borrows are not
    ///   allowed).
    pub fn borrow_out(&mut self, capability: CapabilityId) -> bool {
        for index in 0..self.capability_count {
            if self.capabilities[index] == capability {
                if self.borrow_state[index] != BorrowState::Owned {
                    return false;
                }

                self.borrow_state[index] = BorrowState::BorrowedOut;
                return true;
            }
        }

        false
    }

    /// Reverts a `BorrowedOut` capability to `Owned`.
    ///
    /// Called when a borrower returns the capability, or when a
    /// borrower's task exits without returning and the borrow is
    /// broken.
    ///
    /// # Return value
    ///
    /// - `true` if the transition succeeded.
    /// - `false` if the capability is not in the cell, or if its
    ///   state is not `BorrowedOut`.
    pub fn revert_to_owned(&mut self, capability: CapabilityId) -> bool {
        for index in 0..self.capability_count {
            if self.capabilities[index] == capability {
                if self.borrow_state[index] != BorrowState::BorrowedOut {
                    return false;
                }

                self.borrow_state[index] = BorrowState::Owned;
                return true;
            }
        }

        false
    }

    /// Removes a capability from the cell's namespace.
    ///
    /// The slot is removed and the arrays are compacted. Removing
    /// a capability that is `BorrowedOut` is allowed: the fabric
    /// uses this path when a borrowed capability is revoked by a
    /// third party, which breaks the borrow and removes the slot
    /// from both the lender and the borrower.
    ///
    /// # Return value
    ///
    /// - `true` if the capability was removed.
    /// - `false` if the capability is not in the cell.
    pub fn remove_capability(&mut self, capability: CapabilityId) -> bool {
        for index in 0..self.capability_count {
            if self.capabilities[index] == capability {
                // Compact the namespace and the borrow-state
                // array in lockstep. Shifting both by one from
                // `index` keeps them aligned.
                for next in index..(self.capability_count - 1) {
                    self.capabilities[next] = self.capabilities[next + 1];
                    self.borrow_state[next] = self.borrow_state[next + 1];
                }

                self.capabilities[self.capability_count - 1] = CapabilityId::INVALID;
                self.borrow_state[self.capability_count - 1] = BorrowState::Owned;
                self.capability_count -= 1;

                return true;
            }
        }

        false
    }

    /// Returns the number of capabilities in the cell.
    pub const fn capability_count(&self) -> usize {
        self.capability_count
    }

    /// Returns the capability at the given slot index, if the
    /// slot is valid.
    ///
    /// Used by tests and diagnostics that need to iterate a
    /// cell's contents. The order of the slots is the order in
    /// which capabilities were added, minus any that have been
    /// removed (removals compact the array).
    pub fn capability_at(&self, index: usize) -> Option<CapabilityId> {
        if index >= self.capability_count {
            return None;
        }

        Some(self.capabilities[index])
    }
}
