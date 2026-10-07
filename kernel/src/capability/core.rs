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
    /// directory is returned. If it is a cell, its capabilities
    /// are released.
    ///
    /// # Ordering
    ///
    /// Capabilities are revoked before the object's storage is
    /// dropped. This is required for memory safety: the object's
    /// `Drop` implementation returns frames to the frame allocator,
    /// and any cell still holding a capability to the object would
    /// be holding a stale reference after the frames were reused.
    /// The order is:
    ///
    /// 1. Revoke every capability to the object.
    /// 2. Remove the object's storage (`MemoryObject` or
    ///    `AddressSpace`), which drops it and returns its frames.
    /// 3. Mark the registry slot free.
    ///
    /// Returns `true` if the object existed and was destroyed.
    pub fn destroy_object(&mut self, object: ObjectId) -> bool {
        if self.registry.lookup(object).is_none() {
            return false;
        }

        self.itable.revoke_object(object);

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
}
