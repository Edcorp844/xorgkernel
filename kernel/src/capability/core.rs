//! The capability core.
//!
//! `CapabilityCore` is the fabric's central authority. It owns the
//! ITable, the object registry, the memory-object storage, the
//! address-space storage, and the execution cells, and it exposes
//! the operations that manipulate them.
//!
//! # The three capability operations
//!
//! The fabric provides exactly three ways for a capability to move
//! or be duplicated. The choice of three — and the fact that each
//! has a distinct name — is the architecture's central decision.
//!
//! ## `move_capability` — affine ownership transfer
//!
//! Transfers a capability from wherever it currently lives to a
//! target cell. The source is **consumed**: after a successful
//! move, the source `CapabilityId` no longer resolves. The target
//! receives a fresh `CapabilityId` naming the same object with the
//! same rights.
//!
//! Move is the affine operation. It is the fabric's default for
//! authority transfer, and it is what the architecture's safety
//! argument rests on. Two properties follow from it:
//!
//! 1. **No authority amplification.** At most one `Owned` token
//!    exists for any object at any time. There is no operation
//!    that produces a second `Owned` token; the source of a move
//!    is consumed before the target's token is observable.
//!
//! 2. **O(1) revocation.** Because each token has at most one
//!    owner, revoking a token is a single ITable write. There is
//!    no derivation tree to walk, because there are no
//!    derivations: nothing ever branches.
//!
//! Both properties are *structural*: they hold because the
//! operation set contains no amplifying operation, not because
//! some policy layer correctly enforces a rule. Structural
//! properties are what make the architecture verifiable and what
//! make a security audit tractable.
//!
//! ## `copy_capability` — leaf-only duplication
//!
//! Produces a second capability to the same object with a subset
//! of the source's rights, **stripped of `SHARE`**. The source is
//! retained. The new capability is a *leaf*: because it lacks
//! `SHARE`, it cannot be delegated further, so it cannot be the
//! root of a derivation tree.
//!
//! Copy is what makes read-only sharing possible. A compositor
//! needs to read every surface; a monitor needs to read every
//! process's memory. Copy handles both without breaking the
//! affine invariant for *ownership*, because a leaf is never
//! `Owned` in the sense the paper uses: it is a read-only
//! reference, not a token that can be delegated or amplified.
//!
//! ## `try_move_capability` — consuming move
//!
//! The affine-purist variant of `move_capability`. It consumes
//! the source *even if the target side fails*, so a caller that
//! has already committed to spending the token can use it without
//! worrying about a partial failure returning a token it thought
//! it had given away.
//!
//! The difference is entirely about failure semantics. Both
//! operations preserve the affine invariant; they differ in what
//! the caller sees when the move cannot complete.
//!
//! # Why not one operation with a flag
//!
//! A single `transfer(source, target, mode: Move | Copy)` would
//! have the same behavior and fewer names. It is rejected because
//! the mode is the *reason* the caller is calling, not a
//! configuration detail. Making it a parameter means the compiler
//! cannot distinguish "this call is a move" from "this call is a
//! copy," which means a caller who meant to give away ownership
//! and wrote `Copy` by mistake gets a silent authorization to
//! retain the token. Two names, two functions, two distinct
//! obligations.
//!
//! # Speculation barriers
//!
//! The fabric's public capability-check methods —
//! [`CapabilityCore::lookup`], [`CapabilityCore::has_rights`],
//! and [`CapabilityCore::object`] — call
//! [`crate::cpu::speculate::capability_barrier`] before returning.
//! The barrier is an `lfence` on x86 (a sequentially-consistent
//! fence elsewhere) that prevents the processor from speculating
//! past the capability check.
//!
//! The rationale is in the paper's §4.2 and in the documentation
//! of the `cpu::speculate` module. In short: an out-of-order
//! processor can speculatively execute a load *after* a
//! capability check has failed, and while the load's result is
//! squashed, the cache state it leaves behind can be measured by
//! an attacker to recover information about the load's target.
//! The barrier forces the check to commit before any subsequent
//! instruction is dispatched, closing that window.
//!
//! # The `lookup_checked` / `lookup` split
//!
//! The public checks are wrappers around private `_checked`
//! variants:
//!
//! - [`CapabilityCore::lookup_checked`] performs the actual
//!   ITable lookup and returns its result without a barrier. It
//!   is the fabric's internal primitive.
//! - [`CapabilityCore::lookup`] calls `lookup_checked` and
//!   places a barrier before returning. It is the public
//!   boundary.
//!
//! The split exists because the barrier's purpose is to protect
//! *callers of the fabric* from speculatively accessing an object
//! whose capability check failed. The fabric's own methods
//! (`move_capability`, `map_memory`, and so on) are inside the
//! trust boundary: they call `lookup_checked` and do not pay the
//! barrier, because their own callers are the fabric's callers
//! and the barrier belongs at that outer boundary.
//!
//! Without the split, every internal call to `lookup` would pay a
//! barrier whose purpose did not apply, and `has_rights` and
//! `object` would end up paying two barriers each (their own, plus
//! the one inside `lookup`). The split makes the boundary explicit
//! in the code and ensures exactly one barrier per public check.
//!
//! # Invariants
//!
//! 1. Every live object is registered in the object registry.
//! 2. Every live capability refers to a registered object.
//! 3. Destroying an object revokes every capability to it.
//! 4. Move consumes its source; the target's token is the only
//!    live `Owned` token to the object.
//! 5. Copy produces a leaf: the derived capability has no
//!    `SHARE` right, regardless of what was requested.
//! 6. A memory object's frames are returned to the frame
//!    allocator when the object is destroyed.
//! 7. Mapping memory into an address space requires `MAP` on both
//!    capabilities.
//!
//! The fabric is responsible for maintaining these invariants.
//! Callers must not violate them by reaching into the tables
//! directly; all operations go through the methods below.

use crate::capability::capability::{Capability, CapabilityId, CapabilityRights};
use crate::capability::cell::{Cell, CellId};
use crate::capability::channel::{Channel, Message, MessageKind};
use crate::capability::itable::ITable;
use crate::capability::object::{ObjectId, ObjectKind};
use crate::capability::registry::ObjectRegistry;
use crate::memory::address_space::AddressSpace;
use crate::memory::object::MemoryObject;
use crate::memory::paging;

/// Maximum number of execution cells managed by one capability
/// domain.
///
/// A cell holds up to [`crate::capability::cell::MAX_CELL_CAPABILITIES`]
/// capability IDs. The core's cell table is a fixed array of
/// `Option<Cell>`; cells beyond this count cannot be created until
/// the table is made dynamic.
///
/// 64 cells is enough for the current boot sequence (one cell per
/// task, plus headroom for tests). It is deliberately smaller than
/// `ITABLE_SIZE` because a cell is a much larger structure than a
/// capability slot; 64 cells at
/// `MAX_CELL_CAPABILITIES * 4` bytes each is already 64 KiB of
/// kernel memory.
const MAX_CELLS: usize = 64;

/// Maximum number of memory objects managed by one capability
/// domain.
///
/// 256 is chosen to match `MAX_MAPPABLE_FRAMES` and the heap's
/// `REGION_FRAMES`: the heap's largest memory object is 256
/// frames, so 256 memory-object slots are enough to hold the
/// heap's growth plus the fabric's other users (address spaces,
/// test objects, and the objects the boot sequence creates).
const MAX_MEMORY_OBJECTS: usize = 256;

/// Maximum number of address spaces managed by one capability
/// domain.
///
/// Address spaces are relatively expensive: each owns a page
/// directory and its page tables. 64 is enough for the kernel's
/// own address space plus every user cell the current kernel is
/// expected to support. It will grow when user mode and process
/// creation arrive, and the bound will be lifted when the tables
/// become dynamic.
const MAX_ADDRESS_SPACES: usize = 64;

/// Maximum number of IPC channels managed by one capability
/// domain.
///
/// A channel holds a fixed ring of [`crate::capability::channel::MAX_CHANNEL_MESSAGES`]
/// messages at 28 bytes each, so one channel occupies about 900
/// bytes. 64 channels is roughly 56 KiB, which is small enough for
/// the bootstrap kernel and large enough that the tests and the
/// boot sequence have headroom.
///
/// The bound is a fixed array size, not a design choice. When the
/// fabric's tables become dynamic, the bound will be lifted.
const MAX_CHANNELS: usize = 64;

/// Maximum number of outstanding dequeued borrows the fabric
/// tracks at once.
///
/// A "dequeued borrow" is a `BorrowIn` message that a borrower
/// has received but not yet returned. The fabric tracks these so
/// that a borrower's task exit can release the lender's slot,
/// which would otherwise stay frozen forever.
///
/// 64 is chosen to match `MAX_CELLS`: in the current kernel each
/// cell can have at most a small number of outstanding borrows,
/// and 64 entries cover every cell having one borrow in flight
/// simultaneously. The table is an array of `Option<OutstandingBorrow>`;
/// each entry is 16 bytes, so the table is 1 KiB.
const MAX_OUTSTANDING_BORROWS: usize = 64;

/// Maximum number of frames `map_memory` can install in one call.
///
/// `map_memory` copies a memory object's frame addresses into a
/// stack-allocated scratch buffer before iterating them. The
/// buffer's size is fixed, so objects larger than this cannot be
/// mapped.
///
/// 256 frames = 1 MiB. This matches `heap::REGION_FRAMES`: the
/// heap installs regions of this size, and the regions must be
/// mappable through `map_memory`. A smaller value would prevent
/// the heap from installing its own regions; a larger value would
/// waste stack space for a capability the heap does not exercise.
///
/// The scratch buffer is 256 * 4 = 1 KiB on the stack, which is
/// negligible. `map_memory` is called only when the heap grows, a
/// rare operation.
///
/// Lifting this bound requires the fabric to obtain the scratch
/// buffer without going through the heap, which would deadlock
/// during heap growth. See the module documentation on the
/// bootstrap cycle.
const MAX_MAPPABLE_FRAMES: usize = 256;

/// Errors returned by the fabric's mapping operations.
///
/// Every variant names a specific failure mode. Callers can
/// distinguish "the caller lacks authority" from "the caller
/// provided a bad address" from "the system is out of resources."
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapError {
    /// The address-space capability does not resolve, or its object
    /// is not an address space.
    InvalidAddressSpace,

    /// The memory-object capability does not resolve, or its object
    /// is not a memory object.
    InvalidMemoryObject,

    /// The caller does not hold `MAP` on the address space.
    MissingAddressSpaceMapRight,

    /// The caller does not hold `MAP` on the memory object.
    MissingMemoryObjectMapRight,

    /// The caller does not hold `UNMAP` on the address space.
    MissingAddressSpaceUnmapRight,

    /// The virtual address is not page-aligned.
    UnalignedVirtualAddress,

    /// The virtual address falls in a region the address space
    /// reserves for the kernel.
    ForbiddenVirtualAddress,

    /// The address space is out of page-table frames.
    OutOfMemory,

    /// The memory object has more frames than `map_memory` can map
    /// in one call.
    ///
    /// `map_memory` installs a memory object's frames into an
    /// address space all at once, copying the frame addresses into
    /// a fixed-size stack buffer first. The buffer is sized for
    /// [`MAX_MAPPABLE_FRAMES`] frames; a larger object cannot be
    /// mapped.
    ///
    /// This is a bootstrap limitation, not a design choice. Once
    /// the fabric has a way to obtain scratch storage that does
    /// not go through the heap, the bound will be lifted.
    ///
    /// A caller that encounters this error and genuinely needs to
    /// map a larger object must split it into multiple objects,
    /// each at or below the limit, and map each one separately.
    ObjectTooLarge,
}

/// A borrow that a cell has dequeued but not yet returned.
///
/// The fabric tracks these so that a borrower's task exit can
/// release the lender's slot. Without this record, the fact that
/// the borrower holds the message is invisible to the fabric: the
/// message itself is a value in the borrower's context, and the
/// channel's ring no longer contains it.
///
/// A record is created by [`CapabilityCore::recv_message`] when it
/// returns a `BorrowIn` message, and removed by
/// [`CapabilityCore::return_capability`] when the borrow is
/// returned, or by [`CapabilityCore::break_borrows_for_cell`]
/// when the borrower's task exits.
#[derive(Clone, Copy)]
struct OutstandingBorrow {
    /// The cell that holds the dequeued message.
    borrower: CellId,

    /// The channel the message came from.
    ///
    /// Needed to remove the `BorrowIn` message from the channel's
    /// ring when the borrow is returned or broken.
    channel: ObjectId,

    /// The capability the borrow is for.
    ///
    /// Matches the `capability` field of the `BorrowIn` message.
    capability: CapabilityId,

    /// The borrow's tag.
    ///
    /// Identifies the borrow uniquely within the channel. Used to
    /// locate the message when returning or breaking.
    tag: u32,
}

/// Central authority for objects, capabilities, memory, address
/// spaces, and cells.
///
/// See the module documentation for the three-operation model, the
/// speculation barriers, and the invariants the core maintains.
pub struct CapabilityCore {
    /// Global capability table.
    itable: ITable,

    /// Registry containing all live kernel objects.
    registry: ObjectRegistry,

    /// Execution cells managed by this capability domain.
    cells: [Option<Cell>; MAX_CELLS],

    /// Memory objects managed by this capability domain.
    memory_objects: [Option<MemoryObject>; MAX_MEMORY_OBJECTS],

    /// Address spaces managed by this capability domain.
    address_spaces: [Option<AddressSpace>; MAX_ADDRESS_SPACES],

    /// IPC channels managed by this capability domain.
    ///
    /// A channel is an object; it holds the message ring. When a
    /// channel object is destroyed, its `Drop` handler drops any
    /// pending messages and reverts any capabilities they carried
    /// to their original owners.
    channels: [Option<Channel>; MAX_CHANNELS],

    /// Outstanding dequeued borrows.
    ///
    /// See [`OutstandingBorrow`] and
    /// [`CapabilityCore::recv_message`].
    outstanding_borrows: [Option<OutstandingBorrow>; MAX_OUTSTANDING_BORROWS],

    /// Identifier assigned to the next newly created cell.
    next_cell_id: u32,
}

impl CapabilityCore {
    /// Creates an empty capability core.
    pub const fn new() -> Self {
        Self {
            itable: ITable::new(),
            registry: ObjectRegistry::new(),
            cells: [const { None }; MAX_CELLS],
            memory_objects: [const { None }; MAX_MEMORY_OBJECTS],
            address_spaces: [const { None }; MAX_ADDRESS_SPACES],
            channels: [const { None }; MAX_CHANNELS],
            outstanding_borrows: [const { None }; MAX_OUTSTANDING_BORROWS],
            next_cell_id: 1,
        }
    }

    // =================================================================
    // Object lifecycle
    // =================================================================

    /// Creates a new object of the given kind.
    ///
    /// The object is registered but has no capabilities. A caller
    /// that wants to use the object must allocate a capability to
    /// it with [`CapabilityCore::allocate`].
    pub fn create_object(&mut self, kind: ObjectKind) -> Option<ObjectId> {
        self.registry.create(kind)
    }

    /// Resolves an object identifier.
    ///
    /// Returns the same ID if the object exists, `None` otherwise.
    /// Used by callers that have an `ObjectId` and want to know
    /// whether it names a live object without allocating a
    /// capability.
    pub fn lookup_object(&self, object: ObjectId) -> Option<ObjectId> {
        self.registry.lookup(object)
    }

    /// Returns the kind of the object named by `id`, if it exists.
    pub fn lookup_object_kind(&self, object: ObjectId) -> Option<ObjectKind> {
        self.registry.kind(object)
    }

    /// Destroys an object and revokes every capability referring to
    /// it.
    ///
    /// If the object is a memory object, its frames are returned to
    /// the frame allocator. If it is an address space, its page
    /// directory is returned. If it is a channel, its pending
    /// messages are cleaned up:
    ///
    /// - `Move` messages' capabilities are revoked. They have no
    ///   owner: the sender's cell no longer holds them, and no
    ///   receiver has dequeued them. The message ring is the sole
    ///   holder, and destroying the channel drops that holder.
    ///
    /// - `BorrowIn` messages that are still in the ring have
    ///   their lenders' slots reverted to `Owned`. The lender's
    ///   slot was frozen when the borrow was made; releasing it
    ///   before the message disappears is what prevents the slot
    ///   from being stuck forever.
    ///
    /// - Outstanding-borrow records for this channel have their
    ///   lenders' slots reverted and the records removed. These
    ///   are borrows whose messages were dequeued by a borrower
    ///   and not yet returned; the record is the fabric's
    ///   knowledge of them.
    ///
    /// # Ordering
    ///
    /// Channel cleanup happens *before* the capability revocation
    /// loop at the bottom of this function. The order matters:
    ///
    /// 1. Walk the channel's ring and revoke every `Move` message's
    ///    capability. These capabilities are held by the message,
    ///    not by any cell, so revoking them here is the only way
    ///    they are ever released. Doing this *before* the general
    ///    `revoke_object` call below means that if a message's
    ///    capability happens to refer to the same object being
    ///    destroyed, the general revocation finds it already gone
    ///    and skips it.
    ///
    /// 2. Walk the ring again and revert the lenders of every
    ///    `BorrowIn` message still in the ring. The lender's
    ///    `BorrowedOut` slot must be released before the channel
    ///    is dropped; otherwise the slot is frozen forever.
    ///
    /// 3. Process every outstanding-borrow record for the channel.
    ///    A record whose message is still in the ring was already
    ///    handled by pass 2; a record whose message was dequeued
    ///    but not returned is handled here. In both cases the
    ///    lender's slot is reverted and the record is removed.
    ///
    /// 4. Revoke every remaining capability referring to the
    ///    object. This catches the channel's own capability (the
    ///    one the caller holds to reach the channel) and any
    ///    other capabilities the fabric has allocated to the
    ///    object.
    ///
    /// 5. Drop the object's storage: memory frames, page directory,
    ///    or channel ring. The channel's `Drop` is trivial — it
    ///    does not itself revoke anything — because steps 1
    ///    through 3 have already done the cleanup.
    ///
    /// # Return value
    ///
    /// Returns `true` if the object existed and was destroyed.
    pub fn destroy_object(&mut self, object: ObjectId) -> bool {
        if self.registry.lookup(object).is_none() {
            return false;
        }

        // ---- Channel-specific cleanup. ----

        let is_channel = self.registry.kind(object) == Some(ObjectKind::Channel);

        if is_channel {
            // ---- Pass 1: revoke `Move` messages' capabilities. ----
            //
            // The scan collects capability IDs into a fixed-size
            // stack buffer before revoking, so that we do not
            // hold a borrow of the channel while mutating the
            // ITable.
            let mut to_revoke: [CapabilityId; crate::capability::channel::MAX_CHANNEL_MESSAGES] =
                [CapabilityId::INVALID; crate::capability::channel::MAX_CHANNEL_MESSAGES];
            let mut revoke_count = 0;

            if let Some(channel) = self.channel(object) {
                for offset in 0..channel.len() {
                    let message = match channel.peek(offset) {
                        Some(m) => m,
                        None => continue,
                    };

                    if message.kind == MessageKind::Move
                        && message.capability != CapabilityId::INVALID
                    {
                        if revoke_count < to_revoke.len() {
                            to_revoke[revoke_count] = message.capability;
                            revoke_count += 1;
                        }
                    }
                }
            }

            for i in 0..revoke_count {
                self.itable.revoke(to_revoke[i]);
            }

            // ---- Pass 2: revert lenders of `BorrowIn` messages
            //      still in the ring. ----
            //
            // The loop removes one `BorrowIn` message per iteration
            // because `remove_borrow_in` shifts the ring;
            // re-scanning from the start of the channel after each
            // removal keeps the offsets correct.
            loop {
                let found = {
                    let channel = match self.channel(object) {
                        Some(ch) => ch,
                        None => break,
                    };

                    let mut found: Option<(CapabilityId, u32)> = None;

                    for offset in 0..channel.len() {
                        let message = match channel.peek(offset) {
                            Some(m) => m,
                            None => continue,
                        };

                        if message.kind == MessageKind::BorrowIn {
                            found = Some((message.capability, message.tag));
                            break;
                        }
                    }

                    found
                };

                let (capability, tag) = match found {
                    Some(t) => t,
                    None => break,
                };

                let lender_cell = match lender_from_tag(tag) {
                    Some(c) => c,
                    None => break,
                };

                let removed = match self.channel_mut(object) {
                    Some(channel) => channel.remove_borrow_in(capability, tag),
                    None => false,
                };

                if !removed {
                    break;
                }

                if let Some(cell) = self.cell_mut(lender_cell) {
                    cell.revert_to_owned(capability);
                }
            }

            // ---- Pass 3: process outstanding-borrow records. ----
            //
            // A record whose message is still in the ring was
            // already handled by pass 2. A record whose message
            // was dequeued but not returned is handled here: the
            // lender's slot must be reverted before the record is
            // removed, or the slot stays frozen forever.
            //
            // The records for this channel are collected first,
            // then each is processed (revert lender, remove
            // record). Collection-then-process avoids iterating
            // the table while mutating it.
            let mut to_clear: [Option<OutstandingBorrow>; MAX_OUTSTANDING_BORROWS] =
                [None; MAX_OUTSTANDING_BORROWS];
            let mut clear_count = 0;

            for slot in self.outstanding_borrows.iter() {
                if let Some(borrow) = slot {
                    if borrow.channel == object && clear_count < to_clear.len() {
                        to_clear[clear_count] = Some(*borrow);
                        clear_count += 1;
                    }
                }
            }

            for i in 0..clear_count {
                let borrow = match to_clear[i] {
                    Some(b) => b,
                    None => continue,
                };

                // Revert the lender's slot. The lender's cell is
                // encoded in the borrow's tag.
                if let Some(lender_cell) = lender_from_tag(borrow.tag) {
                    if let Some(cell) = self.cell_mut(lender_cell) {
                        cell.revert_to_owned(borrow.capability);
                    }
                }

                // Remove the record.
                self.remove_borrow_record(
                    borrow.borrower,
                    borrow.channel,
                    borrow.capability,
                    borrow.tag,
                );
            }
        }

        // ---- Revoke every capability referring to the object. ----
        //
        // This includes the channel's own capability (the caller's
        // handle to it) and any other capabilities the fabric has
        // allocated to the object. For a channel, this runs after
        // the passes above; the `Move` messages' capabilities have
        // already been revoked, so this call finds them gone and
        // does not double-revoke them.
        self.itable.revoke_object(object);

        // ---- Drop the object's storage. ----

        for slot in self.memory_objects.iter_mut() {
            if let Some(obj) = slot {
                if obj.id() == object {
                    *slot = None;
                    break;
                }
            }
        }

        for slot in self.address_spaces.iter_mut() {
            if let Some(obj) = slot {
                if obj.id() == object {
                    *slot = None;
                    break;
                }
            }
        }

        for slot in self.channels.iter_mut() {
            if let Some(channel) = slot {
                if channel.id() == object {
                    *slot = None;
                    break;
                }
            }
        }

        // ---- Free the registry slot. ----

        self.registry.destroy(object)
    }

    // =================================================================
    // Capability lifecycle
    // =================================================================

    /// Creates a capability for an existing object.
    ///
    /// This is the fabric's raw capability-creation primitive.
    /// Callers should prefer [`CapabilityCore::move_capability`] or
    /// [`CapabilityCore::copy_capability`], which enforce the
    /// affine and leaf-only invariants. `allocate` is public
    /// because the fabric itself and a handful of bootstrap paths
    /// need to create a first capability to a newly created object
    /// before any move or copy is possible.
    ///
    /// The rights are stored verbatim. `allocate` does not check
    /// whether they are meaningful for the object's kind; that
    /// check is a policy decision that belongs in a
    /// kind-specific wrapper, not in the raw primitive.
    pub fn allocate(&mut self, object: ObjectId, rights: CapabilityRights) -> Option<CapabilityId> {
        if self.registry.lookup(object).is_none() {
            return None;
        }

        self.itable.allocate(object, rights)
    }

    /// Performs the actual ITable lookup, without a speculation
    /// barrier.
    ///
    /// This is the fabric's internal primitive. Public methods
    /// call it and place their own barrier at the point where
    /// their result is about to leave the fabric. Internal
    /// methods call it directly because their callers are the
    /// fabric's callers, and the barrier belongs at that outer
    /// boundary.
    ///
    /// See the module documentation on the `lookup_checked` /
    /// `lookup` split for the rationale.
    fn lookup_checked(&self, capability: CapabilityId) -> Option<&Capability> {
        self.itable.lookup(capability)
    }

    /// Resolves a capability.
    ///
    /// Returns `None` if the ID does not resolve: the slot is
    /// unoccupied, the generation does not match, or the ID was
    /// never valid. A `Some` result is a live reference valid until
    /// the capability is revoked.
    ///
    /// # Speculation barrier
    ///
    /// A [`crate::cpu::speculate::capability_barrier`] is
    /// dispatched before returning, so the check cannot be
    /// speculatively bypassed by a subsequent out-of-order load.
    /// See the module documentation for the rationale.
    pub fn lookup(&self, capability: CapabilityId) -> Option<&Capability> {
        let result = self.lookup_checked(capability);

        crate::cpu::speculate::capability_barrier();

        result
    }

    /// Revokes a capability.
    ///
    /// After this call, the ID no longer resolves. Any attempt to
    /// use it (as a source for move, as an argument to `map_memory`,
    /// as a lookup key) fails with a not-found result, the same as
    /// if the ID had never existed.
    ///
    /// Returns `true` if the capability existed and was revoked.
    /// Returns `false` if it did not resolve, which includes
    /// attempts to revoke the same ID twice.
    pub fn revoke(&mut self, capability: CapabilityId) -> bool {
        self.itable.revoke(capability)
    }

    /// Revokes every capability referring to an object.
    ///
    /// Returns the number of capabilities revoked. Used by
    /// [`CapabilityCore::destroy_object`]; exposed separately so
    /// that a caller can revoke a set of capabilities without
    /// destroying the underlying object.
    ///
    /// The scan is O(n) in the ITable size. Once the per-object
    /// capability list exists, this becomes O(k) in the number of
    /// capabilities to the object.
    pub fn revoke_object(&mut self, object: ObjectId) -> usize {
        self.itable.revoke_object(object)
    }

    /// Determines whether a capability contains the requested
    /// rights.
    ///
    /// Returns `false` if the capability does not resolve.
    ///
    /// # Speculation barrier
    ///
    /// A [`crate::cpu::speculate::capability_barrier`] is
    /// dispatched before returning. The result — "the capability
    /// permits this right" — is what the caller uses to decide
    /// whether to proceed to access the object, so the check
    /// itself is what the barrier protects.
    pub fn has_rights(&self, capability: CapabilityId, required: CapabilityRights) -> bool {
        let allowed = match self.lookup_checked(capability) {
            Some(cap) => cap.rights().contains(required),
            None => false,
        };

        crate::cpu::speculate::capability_barrier();

        allowed
    }

    /// Resolves the object referenced by a capability.
    ///
    /// Returns `None` if the capability does not resolve.
    ///
    /// # Speculation barrier
    ///
    /// A [`crate::cpu::speculate::capability_barrier`] is
    /// dispatched before returning. The returned `ObjectId` is
    /// what the caller uses to name the object in subsequent
    /// fabric operations, so the check that produced it is what
    /// the barrier protects.
    pub fn object(&self, capability: CapabilityId) -> Option<ObjectId> {
        let id = self.lookup_checked(capability).map(|cap| cap.object());

        crate::cpu::speculate::capability_barrier();

        id
    }

    // =================================================================
    // The three capability operations
    // =================================================================

    /// Affine ownership transfer: consume `source`, produce a fresh
    /// capability in `target_cell` with identical object and
    /// rights.
    ///
    /// # Semantics
    ///
    /// After a successful move:
    ///
    /// - `lookup(source)` returns `None`. The source slot's
    ///   generation was bumped by `revoke`, so the source
    ///   `CapabilityId` is stale. This is the affine "consume."
    /// - A new `CapabilityId` resolves in `target_cell`'s
    ///   namespace with the same `(object, rights)` as the source
    ///   had.
    /// - The affine invariant holds: at most one `Owned` token to
    ///   the object exists at any time.
    ///
    /// Move does not require `SHARE` on the source. A capability
    /// you own is a capability you can give away; `SHARE` gates
    /// copy, not move.
    ///
    /// # Failure modes
    ///
    /// - `source` does not resolve (already consumed, revoked, or
    ///   never valid): returns `None`. The source was already gone.
    /// - `target_cell` does not exist: returns `None`. The source
    ///   is **untouched**, because target validation happens
    ///   before revoke.
    /// - The ITable is full: returns `None`. The source is
    ///   **untouched**, because allocation happens before revoke.
    /// - The target cell is full: returns `None`. The source is
    ///   **untouched**, because allocation is rolled back.
    ///
    /// In every failure mode the source survives, so the caller can
    /// retry against a different target or after freeing a slot.
    /// This is the "allocate first, then revoke" ordering. See
    /// [`CapabilityCore::try_move_capability`] for the opposite.
    pub fn move_capability(
        &mut self,
        source: CapabilityId,
        target_cell: CellId,
    ) -> Option<CapabilityId> {
        let (object, rights) = self.itable.lookup_parts(source)?;

        if self.cell(target_cell).is_none() {
            return None;
        }

        let new_cap = self.itable.allocate(object, rights)?;

        if !self.grant_capability(target_cell, new_cap) {
            self.itable.revoke(new_cap);
            return None;
        }

        let consumed = self.itable.revoke(source);
        debug_assert!(
            consumed,
            "source resolved in step 1 but not in step 5; \
             concurrent mutation of the ITable is a bug"
        );

        Some(new_cap)
    }

    /// Consuming move: like [`CapabilityCore::move_capability`],
    /// but the source is consumed even if the target side fails.
    ///
    /// # When to use this
    ///
    /// Use `try_move_capability` when the caller has already
    /// committed to spending the token — for example, a `send_once`
    /// syscall where the kernel must not return the token to
    /// userspace on failure, because the attempt itself is the use.
    /// Use `move_capability` when the caller wants the token to
    /// survive a failed move and can retry.
    ///
    /// # Failure modes
    ///
    /// - `source` does not resolve: returns `None`. Nothing was
    ///   consumed; there was nothing to consume.
    /// - `target_cell` does not exist: returns `None`. The source
    ///   **is consumed anyway**.
    /// - The ITable is full: returns `None`. The source **is
    ///   consumed anyway**.
    /// - The target cell is full: returns `None`. The source **is
    ///   consumed anyway**.
    ///
    /// The affine invariant holds in every case: at most one
    /// `Owned` token to the object exists, and after a failed
    /// `try_move` it is zero.
    pub fn try_move_capability(
        &mut self,
        source: CapabilityId,
        target_cell: CellId,
    ) -> Option<CapabilityId> {
        let (object, rights) = self.itable.lookup_parts(source)?;

        let consumed = self.itable.revoke(source);
        debug_assert!(consumed, "source resolved a moment ago");

        if self.cell(target_cell).is_none() {
            return None;
        }

        let new_cap = self.itable.allocate(object, rights)?;

        if !self.grant_capability(target_cell, new_cap) {
            self.itable.revoke(new_cap);
            return None;
        }

        Some(new_cap)
    }

    /// Leaf-only duplication: produce a second capability to the
    /// same object with the requested rights, stripped of `SHARE`.
    ///
    /// # Semantics
    ///
    /// The source is retained. The new capability is a *leaf*: it
    /// has no `SHARE` right regardless of what was requested. This
    /// is what makes the copy safe — a leaf cannot be the root of
    /// a derivation tree, so the total number of copies of a
    /// capability stays bounded by the number of calls to
    /// `copy_capability`, not by a recursive fan-out.
    ///
    /// # Rights handling
    ///
    /// `requested_rights` must be a subset of the source's rights,
    /// with one exception: `SHARE` is **rejected**, not stripped
    /// silently. If `requested_rights` contains `SHARE`, the call
    /// returns `None` regardless of whether the source has it.
    ///
    /// This is deliberate. A caller who writes
    /// `copy_capability(src, cell, READ | SHARE)` is asking for a
    /// copy that can be re-delegated, which is exactly what the
    /// architecture forbids. Silently stripping `SHARE` and
    /// succeeding would let the caller believe they have a
    /// shareable copy, and the mistake would surface later as a
    /// missing capability. Rejecting makes the mistake surface at
    /// the call site, which is where it belongs.
    ///
    /// Note that the source's own `SHARE` bit is **not** checked.
    /// A capability without `SHARE` can still be copied; the
    /// result simply lacks `SHARE` as well, so it is also a leaf.
    /// The invariant is that no copy ever carries `SHARE`, which
    /// holds regardless of how many times a capability is copied.
    ///
    /// # Failure modes
    ///
    /// - `source` does not resolve: returns `None`.
    /// - `requested_rights` contains `SHARE`: returns `None`.
    /// - `requested_rights` is not a subset of the source's
    ///   rights: returns `None`.
    /// - `target_cell` does not exist: returns `None`.
    /// - The ITable is full or the target cell is full: returns
    ///   `None`.
    ///
    /// In no case is the source modified. `copy_capability` is
    /// non-destructive.
    pub fn copy_capability(
        &mut self,
        source: CapabilityId,
        target_cell: CellId,
        requested_rights: CapabilityRights,
    ) -> Option<CapabilityId> {
        let (object, source_rights) = self.itable.lookup_parts(source)?;

        if requested_rights.contains(CapabilityRights::SHARE) {
            return None;
        }

        if !source_rights.contains(requested_rights) {
            return None;
        }

        if self.cell(target_cell).is_none() {
            return None;
        }

        let new_cap = self.itable.allocate(object, requested_rights)?;

        if !self.grant_capability(target_cell, new_cap) {
            self.itable.revoke(new_cap);
            return None;
        }

        Some(new_cap)
    }

    // =================================================================
    // Memory objects
    // =================================================================

    /// Allocates a memory object with `pages` frames and returns a
    /// capability to it.
    ///
    /// See the module docs on `memory/object.rs` for the object's
    /// role. The operation is:
    ///
    /// 1. Allocate `pages` frames from the frame allocator.
    /// 2. Create a `MemoryObject` holding them.
    /// 3. Register the object with the fabric.
    /// 4. Create a capability with the requested rights.
    ///
    /// Returns `None` on any failure, with full rollback of any
    /// partial state.
    ///
    /// The returned capability is the caller's to move, copy, or
    /// revoke. It is not automatically granted to any cell; a
    /// caller that wants to place it in a cell uses
    /// [`CapabilityCore::grant_capability`] or, more commonly,
    /// [`CapabilityCore::move_capability`] from a cell the caller
    /// already holds.
    pub fn allocate_memory(
        &mut self,
        pages: usize,
        rights: CapabilityRights,
    ) -> Option<(ObjectId, CapabilityId)> {
        let mut object = MemoryObject::new(pages)?;

        let slot_index = self.memory_objects.iter().position(|slot| slot.is_none())?;

        let id = self.registry.create(ObjectKind::MemoryObject)?;

        object.set_id(id);
        self.memory_objects[slot_index] = Some(object);

        match self.itable.allocate(id, rights) {
            Some(cap) => Some((id, cap)),
            None => {
                self.memory_objects[slot_index] = None;
                self.registry.destroy(id);
                None
            }
        }
    }

    /// Returns a reference to a memory object named by a capability.
    ///
    /// Returns `None` if the capability does not resolve, or if it
    /// names an object that is not a memory object.
    pub fn memory_object(&self, cap: CapabilityId) -> Option<&MemoryObject> {
        let capability = self.lookup_checked(cap)?;
        let id = capability.object();

        if self.registry.kind(id)? != ObjectKind::MemoryObject {
            return None;
        }

        for slot in &self.memory_objects {
            if let Some(obj) = slot {
                if obj.id() == id {
                    return Some(obj);
                }
            }
        }

        None
    }

    // =================================================================
    // Address spaces
    // =================================================================

    /// Allocates an address space and returns a capability to it.
    ///
    /// The operation is:
    ///
    /// 1. Allocate a page directory from the frame allocator.
    /// 2. Copy the kernel mappings into it.
    /// 3. Register the object with the fabric.
    /// 4. Create a capability with the requested rights.
    ///
    /// Returns `None` on any failure, with full rollback.
    pub fn allocate_address_space(
        &mut self,
        rights: CapabilityRights,
    ) -> Option<(ObjectId, CapabilityId)> {
        let mut aspace = AddressSpace::new()?;

        let slot_index = self.address_spaces.iter().position(|slot| slot.is_none())?;

        let id = self.registry.create(ObjectKind::AddressSpace)?;

        aspace.set_id(id);
        self.address_spaces[slot_index] = Some(aspace);

        match self.itable.allocate(id, rights) {
            Some(cap) => Some((id, cap)),
            None => {
                self.address_spaces[slot_index] = None;
                self.registry.destroy(id);
                None
            }
        }
    }

    /// Registers the kernel's own address space with the fabric.
    ///
    /// The kernel's address space is established by `paging::init`
    /// during early boot, before the fabric exists. This method
    /// wraps it as a fabric-managed object so that the kernel can
    /// use `map_memory` to install new mappings into its own
    /// virtual range.
    ///
    /// The distinction from `allocate_address_space` is important:
    ///
    /// - `allocate_address_space` allocates a *new* page directory
    ///   and returns a capability to an address space that has no
    ///   mappings beyond the inherited kernel ones.
    /// - `register_kernel_address_space` wraps the *existing*
    ///   page directory, which already contains the identity map,
    ///   the direct map, and every kernel code and data mapping
    ///   installed during boot.
    ///
    /// Only one kernel address space exists. This method should be
    /// called exactly once, after `capability::init` and before any
    /// caller needs a capability to the kernel's address space.
    ///
    /// # Returns
    ///
    /// `Some((ObjectId, CapabilityId))` on success, with the
    /// capability carrying `rights`. `None` if the fabric's
    /// address-space table is full, the registry is full, or the
    /// ITable is full.
    ///
    /// # Panics
    ///
    /// Panics if an address space has already been registered at
    /// the kernel's page directory address. This would indicate a
    /// programming error: the method must be called exactly once.
    pub fn register_kernel_address_space(
        &mut self,
        rights: CapabilityRights,
    ) -> Option<(ObjectId, CapabilityId)> {
        let kernel_pd = paging::page_directory_address();

        for slot in &self.address_spaces {
            if let Some(obj) = slot {
                if obj.page_directory() == kernel_pd {
                    panic!("kernel address space already registered");
                }
            }
        }

        let mut aspace = AddressSpace::from_page_directory(kernel_pd);

        let slot_index = self.address_spaces.iter().position(|slot| slot.is_none())?;

        let id = self.registry.create(ObjectKind::AddressSpace)?;

        aspace.set_id(id);
        self.address_spaces[slot_index] = Some(aspace);

        match self.itable.allocate(id, rights) {
            Some(cap) => Some((id, cap)),
            None => {
                self.address_spaces[slot_index] = None;
                self.registry.destroy(id);
                None
            }
        }
    }

    /// Returns a reference to an address space named by a
    /// capability.
    ///
    /// Returns `None` if the capability does not resolve, or if it
    /// names an object that is not an address space.
    pub fn address_space(&self, cap: CapabilityId) -> Option<&AddressSpace> {
        let capability = self.lookup_checked(cap)?;
        let id = capability.object();

        if self.registry.kind(id)? != ObjectKind::AddressSpace {
            return None;
        }

        for slot in &self.address_spaces {
            if let Some(obj) = slot {
                if obj.id() == id {
                    return Some(obj);
                }
            }
        }

        None
    }

    // =================================================================
    // IPC channels
    // =================================================================

    /// Creates an IPC channel and returns a capability to it.
    ///
    /// The channel is an object, registered with the registry
    /// like any other. The returned capability is the caller's to
    /// move, copy, or grant to a cell; it is not automatically
    /// placed anywhere.
    ///
    /// A newly created channel is empty and has no senders or
    /// receivers. The caller is responsible for distributing
    /// capabilities to the cells that should be able to use the
    /// channel: a capability with `SEND` on the channel goes to
    /// the sending cell, a capability with `RECV` goes to the
    /// receiving cell, and the channel's owner retains a
    /// capability with `DESTROY` (and usually nothing else).
    ///
    /// # Rights
    ///
    /// The `rights` argument is the rights the returned
    /// capability carries. Callers that will grant the
    /// capability to other cells should include `SHARE`, so that
    /// `copy_capability` can derive attenuated copies for those
    /// cells. A caller that keeps the capability for itself may
    /// omit `SHARE`.
    ///
    /// # Return value
    ///
    /// `Some((ObjectId, CapabilityId))` on success. `None` if the
    /// channel table is full, the registry is full, or the ITable
    /// is full.
    pub fn create_channel(&mut self, rights: CapabilityRights) -> Option<(ObjectId, CapabilityId)> {
        let slot_index = self.channels.iter().position(|slot| slot.is_none())?;

        let id = self.registry.create(ObjectKind::Channel)?;

        let mut channel = Channel::new();
        channel.set_id(id);
        self.channels[slot_index] = Some(channel);

        match self.itable.allocate(id, rights) {
            Some(cap) => Some((id, cap)),
            None => {
                self.channels[slot_index] = None;
                self.registry.destroy(id);
                None
            }
        }
    }

    /// Sends a capability over a channel as a `Move` message.
    ///
    /// The source cell must hold the capability, and the
    /// capability must be in the `Owned` state (a `BorrowedOut`
    /// capability cannot be sent). The capability's ITable slot is
    /// **consumed**: after a successful send, the source cell no
    /// longer holds it and the capability ID is stale.
    ///
    /// The channel's ring receives a `Move` message carrying the
    /// original capability ID. That ID is now valid for the
    /// receiver: when the receiver dequeues the message, it
    /// obtains the capability.
    ///
    /// # Why the ID survives the move
    ///
    /// Under the fabric's normal `move_capability` operation, the
    /// source ID is revoked and the target gets a *fresh* ID. For
    /// IPC, that would require the fabric to allocate a new ITable
    /// slot for every message, and the message ring would carry
    /// the fresh ID rather than the original.
    ///
    /// The IPC path takes a different route: it *does not* revoke
    /// the source ID. Instead, it removes the ID from the source
    /// cell's namespace and places the same ID in the message
    /// ring. The ID's ITable slot remains live throughout; what
    /// changes is which cell's namespace references it.
    ///
    /// This is not a weakening of the affine invariant. The
    /// invariant is "at most one cell's namespace references this
    /// capability ID." Sending moves the reference from the
    /// source cell to the message ring, which is a kernel-side
    /// holder, not a cell. When the receiver dequeues, the
    /// reference moves from the message ring to the receiver's
    /// cell. At every point, exactly one holder exists.
    ///
    /// The ITable slot's generation does not change, so the ID
    /// remains valid. If the receiver never dequeues, the message
    /// ring holds the ID until the channel is destroyed, at
    /// which point the channel's `Drop` revokes every pending
    /// message's capability.
    ///
    /// # Rights
    ///
    /// The channel capability must carry `SEND`. The sender's
    /// cell is checked against the source cell parameter: the
    /// capability must be in that cell. The source cell itself is
    /// not otherwise validated; it is the caller's responsibility
    /// to pass the correct one.
    ///
    /// # Return value
    ///
    /// `true` if the message was placed on the channel. `false` if:
    ///
    /// - the channel capability does not resolve, is not a
    ///   channel, or lacks `SEND`;
    /// - the source cell does not hold the capability;
    /// - the capability is `BorrowedOut`;
    /// - the channel is full.
    ///
    /// On failure, the source cell is unchanged.
    pub fn send_capability(
        &mut self,
        channel_cap: CapabilityId,
        source_cell: CellId,
        capability: CapabilityId,
    ) -> bool {
        // 1. Resolve the channel capability and check SEND.
        let channel_object = match self.lookup_checked(channel_cap) {
            Some(cap) => {
                if self.registry.kind(cap.object()) != Some(ObjectKind::Channel) {
                    return false;
                }
                if !cap.rights().contains(CapabilityRights::SEND) {
                    return false;
                }
                cap.object()
            }
            None => return false,
        };

        // 2. Verify the source cell holds the capability and is
        //    not borrowed out.
        match self.cell(source_cell) {
            Some(cell) => {
                if !cell.has_capability(capability) {
                    return false;
                }
                if cell.borrow_state(capability)
                    != Some(crate::capability::cell::BorrowState::Owned)
                {
                    return false;
                }
            }
            None => return false,
        }

        // 3. Verify the capability resolves in the ITable.
        if self.lookup_checked(capability).is_none() {
            return false;
        }

        // 4. Push the message. This is the only fallible step
        //    that touches the channel; if the channel is full, we
        //    return without touching the source cell.
        let message = Message::move_capability(capability);

        let pushed = match self.channel_mut(channel_object) {
            Some(channel) => channel.push(message),
            None => return false,
        };

        if !pushed {
            return false;
        }

        // 5. Remove the capability from the source cell. This is
        //    the "consume the source" step. The ITable slot
        //    remains live; only the cell's reference to it goes
        //    away.
        if let Some(cell) = self.cell_mut(source_cell) {
            cell.remove_capability(capability);
        }

        true
    }

    /// Receives a message from a channel.
    ///
    /// Pops the oldest message from the channel's ring and returns
    /// it. The caller decides what to do with the message: for a
    /// `Move` message, the caller typically places the capability
    /// in its own cell with `grant_capability`; for a `BorrowIn`
    /// message, the caller uses the capability and later returns
    /// it with `return_capability`.
    ///
    /// The message is the residence of any capability it carries.
    /// A received `Move` message's capability is *not* in any
    /// cell until the receiver grants it. A received `BorrowIn`
    /// message's capability is likewise not in any cell; the
    /// borrower holds the message, uses the capability, and
    /// returns it.
    ///
    /// # Borrower tracking
    ///
    /// When the dequeued message is a `BorrowIn`, the fabric
    /// records the borrow in its outstanding-borrows table, keyed
    /// by the borrower's cell. The record lets
    /// [`CapabilityCore::break_borrows_for_cell`] release the
    /// lender's slot if the borrower exits without returning.
    ///
    /// If the table is full, the receive is **declined**: the
    /// message is pushed back onto the front of the channel's
    /// ring and `None` is returned. An untracked borrow is a
    /// borrow that cannot be cleaned up on task exit, and it is
    /// better to fail the receive than to accept a borrow the
    /// fabric cannot account for.
    ///
    /// Pushing the message back to the *front* (rather than
    /// leaving it wherever the pop put it) preserves the
    /// invariant that the ring's order is FIFO. The next
    /// `recv_message` on the channel, by any receiver, will see
    /// the same message.
    ///
    /// # Arguments
    ///
    /// `channel_cap` must resolve to a channel and carry `RECV`.
    /// `borrower_cell` identifies the cell that is receiving.
    /// It is used only for borrow tracking; a `Move` message's
    /// receive does not consult it. Callers that receive from a
    /// channel they do not own (which is unusual but not
    /// forbidden) still must supply a cell ID for the record.
    ///
    /// # Return value
    ///
    /// `Some(message)` on success. `None` if:
    ///
    /// - the channel capability does not resolve, is not a
    ///   channel, or lacks `RECV`;
    /// - the channel is empty;
    /// - the message is a `BorrowIn` and the borrow table is
    ///   full.
    pub fn recv_message(
        &mut self,
        channel_cap: CapabilityId,
        borrower_cell: CellId,
    ) -> Option<Message> {
        let channel_object = match self.lookup_checked(channel_cap) {
            Some(cap) => {
                if self.registry.kind(cap.object()) != Some(ObjectKind::Channel) {
                    return None;
                }
                if !cap.rights().contains(CapabilityRights::RECV) {
                    return None;
                }
                cap.object()
            }
            None => return None,
        };

        // Pop the message.
        let message = match self.channel_mut(channel_object) {
            Some(channel) => channel.pop(),
            None => return None,
        }?;

        // If the message is a borrow, record it. If the borrow
        // table is full, push the message back and decline.
        if message.kind == MessageKind::BorrowIn {
            let borrow = OutstandingBorrow {
                borrower: borrower_cell,
                channel: channel_object,
                capability: message.capability,
                tag: message.tag,
            };

            if !self.record_borrow(borrow) {
                // Push the message back onto the *front* of the
                // ring. The pop removed it from the front; to
                // restore it, we push all current messages
                // forward by one slot and place the message at
                // the head.
                if let Some(channel) = self.channel_mut(channel_object) {
                    channel.push_front(message);
                }
                return None;
            }
        }

        Some(message)
    }

    /// Borrows a capability over a channel as a `BorrowIn` message.
    ///
    /// The source cell must hold the capability, and the
    /// capability must be in the `Owned` state. On success:
    ///
    /// 1. The source cell's slot for the capability transitions to
    ///    `BorrowedOut`. The slot is frozen: the capability cannot
    ///    be moved, copied, or used until the borrow is returned.
    ///
    /// 2. A `BorrowIn` message is pushed onto the channel's ring.
    ///    The message carries the capability ID and a *borrow tag*
    ///    that encodes the lender's cell.
    ///
    /// The lender's capability ID does *not* leave the lender's
    /// cell. The borrower sees the ID via the message; the lender
    /// still holds the ID, but marked `BorrowedOut`. When the
    /// borrower returns, the message is removed from the ring and
    /// the lender's slot reverts to `Owned`.
    ///
    /// # Borrow tag encoding
    ///
    /// The tag is `(lender_cell_raw << 16) | counter`. The
    /// counter is per-channel and monotonic. The encoding lets a
    /// channel's `Drop` (or the fabric's `destroy_object`) find
    /// the lender of an outstanding borrow without keeping a
    /// separate ledger: extract the lender cell from the tag,
    /// revert that cell's slot.
    ///
    /// The high 16 bits carry the lender's cell ID. The low 16
    /// bits carry a counter that is per-channel and monotonic.
    /// The counter skips 0, because a tag with a zero high half
    /// cannot be decoded by [`lender_from_tag`] and would be
    /// rejected by [`CapabilityCore::return_capability`].
    ///
    /// # Rights
    ///
    /// The channel capability must carry `SEND`.
    ///
    /// # Return value
    ///
    /// `Some(tag)` on success, where `tag` is the borrow's tag.
    /// The caller uses the tag to match the eventual return.
    ///
    /// `None` if:
    ///
    /// - the channel capability does not resolve, is not a
    ///   channel, or lacks `SEND`;
    /// - the source cell does not hold the capability;
    /// - the capability is already `BorrowedOut` (no nested
    ///   borrows);
    /// - the channel is full.
    ///
    /// On failure, the source cell is unchanged.
    pub fn borrow_capability(
        &mut self,
        channel_cap: CapabilityId,
        source_cell: CellId,
        capability: CapabilityId,
    ) -> Option<u32> {
        // 1. Resolve the channel capability and check SEND.
        let channel_object = match self.lookup_checked(channel_cap) {
            Some(cap) => {
                if self.registry.kind(cap.object()) != Some(ObjectKind::Channel) {
                    return None;
                }
                if !cap.rights().contains(CapabilityRights::SEND) {
                    return None;
                }
                cap.object()
            }
            None => return None,
        };

        // 2. Verify the source cell holds the capability.
        match self.cell(source_cell) {
            Some(cell) => {
                if !cell.has_capability(capability) {
                    return None;
                }
            }
            None => return None,
        }

        // 3. Verify the capability resolves.
        if self.lookup_checked(capability).is_none() {
            return None;
        }

        // 4. Generate the tag. The counter is per-channel; the
        //    lender cell is packed into the high bits.
        let counter = match self.channel_mut(channel_object) {
            Some(channel) => channel.next_borrow_tag(),
            None => return None,
        };

        let tag = ((source_cell.raw() & 0xffff) << 16) | (counter & 0xffff);

        // 5. Transition the source cell's slot to BorrowedOut.
        //    This is done before the message push so that if the
        //    push fails, we can revert. If the push succeeds but
        //    the transition had already failed, we would have an
        //    inconsistent state.
        let transitioned = match self.cell_mut(source_cell) {
            Some(cell) => cell.borrow_out(capability),
            None => return None,
        };

        if !transitioned {
            return None;
        }

        // 6. Push the BorrowIn message.
        let message = Message::borrow_in(capability, tag);

        let pushed = match self.channel_mut(channel_object) {
            Some(channel) => channel.push(message),
            None => {
                // Revert the transition. The channel was
                // validated earlier, so this should not happen;
                // if it does, revert and return.
                if let Some(cell) = self.cell_mut(source_cell) {
                    cell.revert_to_owned(capability);
                }
                return None;
            }
        };

        if !pushed {
            // Channel is full. Revert the transition.
            if let Some(cell) = self.cell_mut(source_cell) {
                cell.revert_to_owned(capability);
            }
            return None;
        }

        Some(tag)
    }

    /// Returns a borrowed capability.
    ///
    /// The borrower calls this after finishing its use of the
    /// borrowed capability. The channel's ring is scanned for a
    /// `BorrowIn` message with the given tag and capability; if
    /// found, it is removed. The lender's cell is identified from
    /// the tag, and the lender's slot reverts to `Owned`.
    ///
    /// The borrow's outstanding-borrow record (created by
    /// [`CapabilityCore::recv_message`]) is removed as part of
    /// the return.
    ///
    /// # Why the message must still be in the ring
    ///
    /// The borrow's residence is the channel's message ring. A
    /// return requires the message to still be present: it
    /// identifies the lender and the capability unambiguously.
    ///
    /// If the message is no longer in the ring, the borrow cannot
    /// be returned by this operation. This happens in two cases:
    ///
    /// - The channel was destroyed, which drops the message and
    ///   reverts the lender's slot. The return is a no-op: the
    ///   borrow has already been broken.
    ///
    /// - The lender revoked the capability while it was borrowed.
    ///   In this case, `revoke` cascades to the borrow, and the
    ///   message is removed. The return is a no-op.
    ///
    /// In both cases, the operation is a safe no-op and returns
    /// `false`.
    ///
    /// # Return value
    ///
    /// `true` if the message was found and the lender's slot
    /// reverted to `Owned`. `false` otherwise.
    /// Returns a borrowed capability.
    ///
    /// The borrower calls this after finishing its use of the
    /// borrowed capability. The fabric uses its outstanding-borrow
    /// record to identify the lender and the capability, reverts
    /// the lender's slot to `Owned`, and removes the record.
    ///
    /// The `BorrowIn` message is *not* in the channel's ring at
    /// this point: it was removed when the borrower called
    /// [`CapabilityCore::recv_message`]. The record created by
    /// that receive is the fabric's knowledge of the borrow; the
    /// return consults the record, not the ring.
    ///
    /// # Why the ring is not consulted
    ///
    /// A `BorrowIn` message's lifetime is: pushed to the ring by
    /// `borrow_capability`, popped from the ring by
    /// `recv_message`. Once popped, the message lives in the
    /// borrower's context. The fabric's `OutstandingBorrow` record
    /// is what tracks the borrow after that point.
    ///
    /// The alternative — leaving the message in the ring and
    /// peeking at it — would require `recv_message` to not remove
    /// `BorrowIn` messages and would require additional state to
    /// prevent double-receives. The record table already solves
    /// that problem, and it keeps the ring's semantics simple: a
    /// ring message is a message in transit, not a message in
    /// use.
    ///
    /// # Return value
    ///
    /// `true` if a matching outstanding-borrow record was found
    /// and the lender's slot reverted to `Owned`. `false` if no
    /// record matched, which means the borrow was never
    /// outstanding (already returned, or the message was never
    /// received).
    pub fn return_capability(
        &mut self,
        channel_cap: CapabilityId,
        borrower_cell: CellId,
        capability: CapabilityId,
        tag: u32,
    ) -> bool {
        // 1. Resolve the channel capability. We do not strictly
        //    need it for the return (the record identifies the
        //    channel), but we check it to preserve the invariant
        //    that a return is always scoped to a channel the
        //    caller holds.
        let channel_object = match self.lookup_checked(channel_cap) {
            Some(cap) => {
                if self.registry.kind(cap.object()) != Some(ObjectKind::Channel) {
                    return false;
                }
                cap.object()
            }
            None => return false,
        };

        // 2. Identify the lender from the tag.
        let lender_cell = match lender_from_tag(tag) {
            Some(cell) => cell,
            None => return false,
        };

        // 3. Remove the outstanding-borrow record. If no record
        //    matches, the borrow was not outstanding: the caller
        //    is either returning a borrow that was never made, or
        //    returning the same borrow twice.
        let removed = self.remove_borrow_record(borrower_cell, channel_object, capability, tag);

        if !removed {
            return false;
        }

        // 4. Revert the lender's slot to Owned.
        if let Some(cell) = self.cell_mut(lender_cell) {
            cell.revert_to_owned(capability);
        }

        true
    }

    /// Breaks every outstanding borrow held by or owed to a cell.
    ///
    /// Called by the scheduler when a task exits. The exiting
    /// cell may be:
    ///
    /// - **A lender**: its capabilities include slots in the
    ///   `BorrowedOut` state. Those slots have a matching
    ///   `BorrowIn` message in some channel's ring. The borrow is
    ///   broken: the message is removed, and the slot is reverted
    ///   to `Owned`.
    ///
    /// - **A borrower**: it has dequeued one or more `BorrowIn`
    ///   messages and not returned them. The outstanding-borrow
    ///   table records these. Each is broken: the record is
    ///   removed, and the lender's slot reverts to `Owned`. The
    ///   message is not in the channel's ring — it was popped by
    ///   [`CapabilityCore::recv_message`] — so no ring operation
    ///   is performed for this direction.
    ///
    /// Both directions are handled because a borrow has two
    /// participants, and either can exit.
    ///
    /// # Return value
    ///
    /// The number of borrows broken.
    pub fn break_borrows_for_cell(&mut self, cell: CellId) -> usize {
        let mut broken = 0;

        // ---- Direction 1: the cell is a borrower. ----
        //
        // Scan the outstanding-borrows table for records whose
        // borrower is `cell`. For each, remove the record and
        // revert the lender's slot.
        //
        // The message is not removed from any channel's ring: it
        // was already popped by `recv_message`, which is what
        // created the record in the first place. The record is the
        // fabric's knowledge of the borrow after that point.
        //
        // The records are collected into a fixed-size stack
        // buffer before any mutation, so that we do not iterate
        // the table while mutating it.
        let mut borrower_records: [Option<OutstandingBorrow>; MAX_OUTSTANDING_BORROWS] =
            [None; MAX_OUTSTANDING_BORROWS];
        let mut borrower_count = 0;

        for slot in self.outstanding_borrows.iter() {
            if let Some(borrow) = slot {
                if borrow.borrower == cell && borrower_count < borrower_records.len() {
                    borrower_records[borrower_count] = Some(*borrow);
                    borrower_count += 1;
                }
            }
        }

        for i in 0..borrower_count {
            let borrow = match borrower_records[i] {
                Some(b) => b,
                None => continue,
            };

            // Remove the record. The ring is not consulted: the
            // message is no longer in it.
            self.remove_borrow_record(
                borrow.borrower,
                borrow.channel,
                borrow.capability,
                borrow.tag,
            );

            // Revert the lender's slot.
            if let Some(lender_cell) = lender_from_tag(borrow.tag) {
                if let Some(cell_ref) = self.cell_mut(lender_cell) {
                    cell_ref.revert_to_owned(borrow.capability);
                }
            }

            broken += 1;
        }

        // ---- Direction 2: the cell is a lender. ----
        //
        // Scan every channel's ring for `BorrowIn` messages whose
        // lender is `cell`. For each, remove the message and
        // revert the lender's slot.
        //
        // The loop removes one message per iteration because
        // `remove_borrow_in` shifts the ring; re-scanning from
        // the start of each channel after each removal keeps the
        // offsets correct.
        for channel_index in 0..self.channels.len() {
            loop {
                let (channel_object, found) = {
                    let channel = match self.channels[channel_index].as_ref() {
                        Some(ch) => ch,
                        None => break,
                    };

                    let mut found: Option<(CapabilityId, u32)> = None;

                    for offset in 0..channel.len() {
                        let message = match channel.peek(offset) {
                            Some(m) => m,
                            None => continue,
                        };

                        if message.kind != MessageKind::BorrowIn {
                            continue;
                        }

                        let lender_is_exiting = match lender_from_tag(message.tag) {
                            Some(lender) => lender == cell,
                            None => false,
                        };

                        if lender_is_exiting {
                            found = Some((message.capability, message.tag));
                            break;
                        }
                    }

                    (channel.id(), found)
                };

                let (capability, tag) = match found {
                    Some(t) => t,
                    None => break,
                };

                if let Some(channel) = self.channel_mut(channel_object) {
                    if channel.remove_borrow_in(capability, tag) {
                        broken += 1;

                        if let Some(cell_ref) = self.cell_mut(cell) {
                            cell_ref.revert_to_owned(capability);
                        }
                    }
                }
            }
        }

        broken
    }

    /// Maps a memory object into an address space.
    ///
    /// The caller must hold `MAP` on both capabilities. The memory
    /// object's frames are installed at `virtual_address`,
    /// `virtual_address + 4096`, and so on, one page per frame.
    ///
    /// # Rights
    ///
    /// Two capabilities are required, and both are checked:
    ///
    /// - `address_space_cap` must name an address space and carry
    ///   `MAP`
    /// - `memory_object_cap` must name a memory object and carry
    ///   `MAP`
    ///
    /// Failing either check returns the corresponding
    /// [`MapError`] variant. The two rights are independent: a
    /// capability to the address space does not imply any right
    /// over the memory object, and vice versa.
    ///
    /// # Address range
    ///
    /// The permitted virtual range depends on the address space:
    ///
    /// - **Kernel address space** (`is_kernel == true`): any PDE
    ///   except PDE 0.
    /// - **User address space** (`is_kernel == false`): PDEs 1-767
    ///   only.
    ///
    /// An address outside the permitted range returns
    /// [`MapError::ForbiddenVirtualAddress`].
    ///
    /// # Size limit
    ///
    /// `map_memory` installs all of a memory object's frames in a
    /// single call, copying the frame addresses into a
    /// stack-allocated scratch buffer first. The buffer is sized
    /// for [`MAX_MAPPABLE_FRAMES`] frames. A larger object is
    /// rejected with [`MapError::ObjectTooLarge`].
    ///
    /// # Atomicity
    ///
    /// The operation is atomic. If any frame fails to map, the
    /// frames already installed are unmapped and the call returns
    /// an error. On return, the address space is either fully
    /// mapped or unchanged — never in a partial state.
    ///
    /// # Errors
    ///
    /// See [`MapError`] for the full set of failure modes.
    pub fn map_memory(
        &mut self,
        address_space_cap: CapabilityId,
        memory_object_cap: CapabilityId,
        virtual_address: u32,
        writable: bool,
        user: bool,
    ) -> Result<(), MapError> {
        let (as_id, is_kernel) = {
            let cap = self
                .lookup_checked(address_space_cap)
                .ok_or(MapError::InvalidAddressSpace)?;

            if self.registry.kind(cap.object()) != Some(ObjectKind::AddressSpace) {
                return Err(MapError::InvalidAddressSpace);
            }

            if !cap.rights().contains(CapabilityRights::MAP) {
                return Err(MapError::MissingAddressSpaceMapRight);
            }

            let as_id = cap.object();

            let is_kernel = self
                .address_space(address_space_cap)
                .ok_or(MapError::InvalidAddressSpace)?
                .is_kernel();

            (as_id, is_kernel)
        };

        let mo_id = {
            let cap = self
                .lookup_checked(memory_object_cap)
                .ok_or(MapError::InvalidMemoryObject)?;

            if self.registry.kind(cap.object()) != Some(ObjectKind::MemoryObject) {
                return Err(MapError::InvalidMemoryObject);
            }

            if !cap.rights().contains(CapabilityRights::MAP) {
                return Err(MapError::MissingMemoryObjectMapRight);
            }

            cap.object()
        };

        if virtual_address & 0xfff != 0 {
            return Err(MapError::UnalignedVirtualAddress);
        }

        let pde = (virtual_address >> 22) as usize;

        if is_kernel {
            if pde == 0 {
                return Err(MapError::ForbiddenVirtualAddress);
            }
        } else {
            if pde == 0 || pde >= 768 {
                return Err(MapError::ForbiddenVirtualAddress);
            }
        }

        let frame_count = {
            let mem = self
                .memory_object(memory_object_cap)
                .ok_or(MapError::InvalidMemoryObject)?;

            mem.page_count()
        };

        if frame_count > MAX_MAPPABLE_FRAMES {
            return Err(MapError::ObjectTooLarge);
        }

        let mut frames: [Option<u32>; MAX_MAPPABLE_FRAMES] = [None; MAX_MAPPABLE_FRAMES];

        {
            let mem = self
                .memory_object(memory_object_cap)
                .ok_or(MapError::InvalidMemoryObject)?;

            for i in 0..frame_count {
                frames[i] = mem.frame(i).map(|f| f.address());
            }
        }

        let _ = mo_id;

        for slot in self.address_spaces.iter_mut() {
            if let Some(aspace) = slot {
                if aspace.id() == as_id {
                    for i in 0..frame_count {
                        let va = virtual_address + (i as u32) * 4096;
                        let pa = frames[i].ok_or(MapError::OutOfMemory)?;

                        if !aspace.map(va, pa, writable, user) {
                            for j in 0..i {
                                let undo_va = virtual_address + (j as u32) * 4096;
                                aspace.unmap(undo_va);
                            }
                            return Err(MapError::OutOfMemory);
                        }
                    }
                    return Ok(());
                }
            }
        }

        Err(MapError::InvalidAddressSpace)
    }

    /// Removes a mapping from an address space.
    ///
    /// The caller must hold `UNMAP` on the address space. The
    /// virtual address must have been previously mapped by
    /// `map_memory`.
    ///
    /// This is the mirror of [`CapabilityCore::map_memory`].
    pub fn unmap_memory(
        &mut self,
        address_space_cap: CapabilityId,
        virtual_address: u32,
    ) -> Result<u32, MapError> {
        let as_id = {
            let cap = self
                .lookup_checked(address_space_cap)
                .ok_or(MapError::InvalidAddressSpace)?;

            if self.registry.kind(cap.object()) != Some(ObjectKind::AddressSpace) {
                return Err(MapError::InvalidAddressSpace);
            }

            if !cap.rights().contains(CapabilityRights::UNMAP) {
                return Err(MapError::MissingAddressSpaceUnmapRight);
            }

            cap.object()
        };

        if virtual_address & 0xfff != 0 {
            return Err(MapError::UnalignedVirtualAddress);
        }

        for slot in self.address_spaces.iter_mut() {
            if let Some(aspace) = slot {
                if aspace.id() == as_id {
                    return aspace
                        .unmap(virtual_address)
                        .ok_or(MapError::InvalidAddressSpace);
                }
            }
        }

        Err(MapError::InvalidAddressSpace)
    }

    // =================================================================
    // Execution cells
    // =================================================================

    /// Creates a new execution cell.
    ///
    /// The cell is empty: it has no capabilities and no identity
    /// beyond its `CellId`. Callers populate it with
    /// [`CapabilityCore::grant_capability`] or
    /// [`CapabilityCore::copy_capability`] or
    /// [`CapabilityCore::move_capability`].
    ///
    /// Returns `None` if the cell table is full.
    pub fn create_cell(&mut self) -> Option<CellId> {
        for slot in self.cells.iter_mut() {
            if slot.is_none() {
                let id = CellId::new(self.next_cell_id);

                self.next_cell_id = self.next_cell_id.wrapping_add(1);

                if self.next_cell_id == 0 {
                    self.next_cell_id = 1;
                }

                *slot = Some(Cell::new(id));

                return Some(id);
            }
        }

        None
    }

    /// Returns a reference to a cell by ID.
    ///
    /// Returns `None` if no cell with that ID exists.
    pub fn cell(&self, id: CellId) -> Option<&Cell> {
        self.cells.iter().flatten().find(|cell| cell.id() == id)
    }

    /// Returns a mutable reference to a cell by ID.
    ///
    /// Returns `None` if no cell with that ID exists.
    fn cell_mut(&mut self, id: CellId) -> Option<&mut Cell> {
        self.cells.iter_mut().flatten().find(|cell| cell.id() == id)
    }

    /// Returns a reference to a channel by ID.
    ///
    /// Returns `None` if no channel with that ID exists.
    ///
    /// This is a read-only accessor. It is public because tests
    /// and diagnostics need to inspect a channel's ring without
    /// consuming its messages, and there is no other way to reach
    /// the channel's internals from outside the fabric. It does
    /// not check any capability: it takes an `ObjectId`, which is
    /// the fabric's internal name for the object, not a
    /// `CapabilityId`, which is the caller's authority. A caller
    /// that has an `ObjectId` is already inside the trust
    /// boundary; a caller that has only a `CapabilityId` must go
    /// through `recv_message` or a similar checked operation.
    ///
    /// The distinction matters. `channel(id: ObjectId)` reveals a
    /// channel's internal state; `recv_message(cap: CapabilityId)`
    /// checks `RECV` and mutates the ring. The former is for
    /// inspection, the latter is for use.
    pub fn channel(&self, id: ObjectId) -> Option<&Channel> {
        self.channels.iter().flatten().find(|ch| ch.id() == id)
    }

    /// Returns a mutable reference to a channel by ID.
    ///
    /// Returns `None` if no channel with that ID exists.
    fn channel_mut(&mut self, id: ObjectId) -> Option<&mut Channel> {
        self.channels.iter_mut().flatten().find(|ch| ch.id() == id)
    }

    /// Grants an existing capability to an execution cell.
    ///
    /// This is a raw grant: it adds the capability to the cell's
    /// namespace without consuming it from anywhere else. It is
    /// used during bootstrap, when a capability has just been
    /// created and needs to be placed in its initial cell. For
    /// authority transfer between cells, use
    /// [`CapabilityCore::move_between_cells`].
    ///
    /// The capability must resolve. Returns `false` if it does
    /// not, or if the cell does not exist, or if the cell is at
    /// capacity.
    ///
    /// If the cell already holds the capability, this is a no-op
    /// and returns `true`.
    pub fn grant_capability(&mut self, cell: CellId, capability: CapabilityId) -> bool {
        if self.lookup_checked(capability).is_none() {
            return false;
        }

        match self.cell_mut(cell) {
            Some(cell) => cell.add_capability(capability),
            None => false,
        }
    }

    /// Determines whether an execution cell possesses a capability.
    ///
    /// Returns `false` if the cell does not exist, regardless of
    /// whether the capability does.
    pub fn cell_has_capability(&self, cell: CellId, capability: CapabilityId) -> bool {
        match self.cell(cell) {
            Some(cell) => cell.has_capability(capability),
            None => false,
        }
    }

    /// Copies a capability from one cell to another, as a leaf.
    ///
    /// The source cell must hold the capability. The target cell
    /// receives a new capability to the same object, with
    /// `requested_rights` attenuated from the source's rights and
    /// with `SHARE` rejected (see
    /// [`CapabilityCore::copy_capability`]).
    ///
    /// The source cell retains its capability. This is the
    /// non-destructive delegation operation: use it when the
    /// source should still be able to use the object after the
    /// copy.
    ///
    /// Returns the new capability's ID, or `None` if any
    /// precondition fails.
    pub fn copy_between_cells(
        &mut self,
        source_cell: CellId,
        target_cell: CellId,
        capability: CapabilityId,
        requested_rights: CapabilityRights,
    ) -> Option<CapabilityId> {
        if !self.cell_has_capability(source_cell, capability) {
            return None;
        }

        self.copy_capability(capability, target_cell, requested_rights)
    }

    /// Moves a capability from one cell to another.
    ///
    /// The source cell must hold the capability. After a
    /// successful move, the source cell no longer holds it and the
    /// target cell holds a fresh `CapabilityId` to the same object
    /// with the same rights.
    ///
    /// This is the destructive transfer operation: use it when the
    /// source is giving up the capability, not lending it.
    ///
    /// Returns the new capability's ID, or `None` if any
    /// precondition fails. On failure, the source cell is
    /// unchanged.
    pub fn move_between_cells(
        &mut self,
        source_cell: CellId,
        target_cell: CellId,
        capability: CapabilityId,
    ) -> Option<CapabilityId> {
        if !self.cell_has_capability(source_cell, capability) {
            return None;
        }

        let new_cap = self.move_capability(capability, target_cell)?;

        if let Some(cell) = self.cell_mut(source_cell) {
            cell.remove_capability(capability);
        }

        Some(new_cap)
    }

    /// Records a dequeued borrow.
    ///
    /// Returns `true` if the record was added, `false` if the
    /// table is full. A `false` return is a signal to the caller
    /// that the borrow cannot be tracked, and it should decline
    /// the receive: an untracked borrow is a borrow that a task
    /// exit cannot clean up.
    fn record_borrow(&mut self, borrow: OutstandingBorrow) -> bool {
        for slot in self.outstanding_borrows.iter_mut() {
            if slot.is_none() {
                *slot = Some(borrow);
                return true;
            }
        }

        false
    }

    /// Removes the outstanding-borrow record matching the given
    /// fields.
    ///
    /// Returns `true` if a matching record was found and removed.
    /// Matching is by `(borrower, channel, capability, tag)`, all
    /// four fields, because a borrower can have multiple
    /// outstanding borrows on the same channel with different
    /// tags.
    fn remove_borrow_record(
        &mut self,
        borrower: CellId,
        channel: ObjectId,
        capability: CapabilityId,
        tag: u32,
    ) -> bool {
        for slot in self.outstanding_borrows.iter_mut() {
            if let Some(borrow) = slot {
                if borrow.borrower == borrower
                    && borrow.channel == channel
                    && borrow.capability == capability
                    && borrow.tag == tag
                {
                    *slot = None;
                    return true;
                }
            }
        }

        false
    }

    /// Removes every outstanding-borrow record for a channel.
    ///
    /// Called by `destroy_object` when a channel is destroyed.
    /// The channel's pending messages are being cleaned up, so
    /// the records that reference it are now meaningless.
    ///
    /// Returns the number of records removed.
    fn remove_borrow_records_for_channel(&mut self, channel: ObjectId) -> usize {
        let mut removed = 0;

        for slot in self.outstanding_borrows.iter_mut() {
            if let Some(borrow) = slot {
                if borrow.channel == channel {
                    *slot = None;
                    removed += 1;
                }
            }
        }

        removed
    }
}

/// Extracts the lender cell from a borrow tag.
///
/// Borrow tags are constructed by [`CapabilityCore::borrow_capability`]
/// as `(lender_cell_raw << 16) | counter`. This function reverses
/// the encoding.
///
/// Returns `None` if the tag's high bits are zero, which means
/// the tag was not generated by `borrow_capability`.
fn lender_from_tag(tag: u32) -> Option<CellId> {
    let raw = (tag >> 16) & 0xffff;

    if raw == 0 {
        return None;
    }

    Some(CellId::new(raw))
}
