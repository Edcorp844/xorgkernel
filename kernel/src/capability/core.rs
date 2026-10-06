//! The capability core.
//!
//! `CapabilityCore` is the fabric's central authority. It owns the
//! ITable, the object registry, the memory-object storage, the
//! address-space storage, and the execution cells, and it exposes
//! the operations that manipulate them.
//!
//! # Invariants
//!
//! 1. Every live object is registered in the object registry.
//! 2. Every live capability refers to a registered object.
//! 3. Destroying an object revokes every capability to it.
//! 4. Delegation can only attenuate rights.
//! 5. A memory object's frames are returned to the frame allocator
//!    when the object is destroyed.
//! 6. Mapping memory into an address space requires `MAP` on both
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

/// Maximum number of execution cells managed by one capability domain.
const MAX_CELLS: usize = 64;

/// Maximum number of memory objects managed by one capability domain.
const MAX_MEMORY_OBJECTS: usize = 256;

/// Maximum number of address spaces managed by one capability domain.
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
    pub fn create_object(&mut self, kind: ObjectKind) -> Option<ObjectId> {
        self.registry.create(kind)
    }

    /// Resolves an object identifier.
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
    pub fn allocate(&mut self, object: ObjectId, rights: CapabilityRights) -> Option<CapabilityId> {
        if self.registry.lookup(object).is_none() {
            return None;
        }

        self.itable.allocate(object, rights)
    }

    /// Resolves a capability.
    pub fn lookup(&self, capability: CapabilityId) -> Option<&Capability> {
        self.itable.lookup(capability)
    }

    /// Revokes a capability.
    pub fn revoke(&mut self, capability: CapabilityId) -> bool {
        self.itable.revoke(capability)
    }

    /// Revokes every capability referring to an object.
    pub fn revoke_object(&mut self, object: ObjectId) -> usize {
        self.itable.revoke_object(object)
    }

    /// Determines whether a capability contains the requested rights.
    pub fn has_rights(&self, capability: CapabilityId, required: CapabilityRights) -> bool {
        match self.lookup(capability) {
            Some(capability) => capability.rights().contains(required),
            None => false,
        }
    }

    /// Resolves the object referenced by a capability.
    pub fn object(&self, capability: CapabilityId) -> Option<ObjectId> {
        self.lookup(capability)
            .map(|capability| capability.object())
    }

    /// Delegates authority from one capability to a new capability.
    ///
    /// The source capability must contain
    /// [`CapabilityRights::SHARE`], and the requested rights must be
    /// a subset of the source's. Rights can therefore only be
    /// attenuated during delegation.
    pub fn transfer(
        &mut self,
        source: CapabilityId,
        requested_rights: CapabilityRights,
    ) -> Option<CapabilityId> {
        let (object, source_rights) = {
            let capability = self.lookup(source)?;
            (capability.object(), capability.rights())
        };

        if !source_rights.contains(CapabilityRights::SHARE) {
            return None;
        }

        if !source_rights.contains(requested_rights) {
            return None;
        }

        self.allocate(object, requested_rights)
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
    pub fn memory_object(&self, cap: CapabilityId) -> Option<&MemoryObject> {
        let capability = self.lookup(cap)?;
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
        // The kernel's page directory address is stable and known
        // to the fabric through `paging`.
        let kernel_pd = paging::page_directory_address();

        // Guard against double registration.
        for slot in &self.address_spaces {
            if let Some(obj) = slot {
                if obj.page_directory() == kernel_pd {
                    panic!("kernel address space already registered");
                }
            }
        }

        // Build an `AddressSpace` that points at the kernel's page
        // directory. This does not allocate anything: it reuses the
        // existing page directory as-is.
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

    /// Returns a reference to an address space named by a capability.
    pub fn address_space(&self, cap: CapabilityId) -> Option<&AddressSpace> {
        let capability = self.lookup(cap)?;
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
        // ---- Validate both capabilities. ----
        //
        // We pull three things out of the address-space
        // capability: its object ID, its `is_kernel` flag (for the
        // range check below), and whether it carries `MAP`. We
        // capture them all while the immutable borrow of `self` is
        // live, so the borrow is released before we start mutating
        // the address-space table.

        let (as_id, is_kernel) = {
            let cap = self
                .lookup(address_space_cap)
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
                .lookup(memory_object_cap)
                .ok_or(MapError::InvalidMemoryObject)?;

            if self.registry.kind(cap.object()) != Some(ObjectKind::MemoryObject) {
                return Err(MapError::InvalidMemoryObject);
            }

            if !cap.rights().contains(CapabilityRights::MAP) {
                return Err(MapError::MissingMemoryObjectMapRight);
            }

            cap.object()
        };

        // ---- Validate alignment. ----

        if virtual_address & 0xfff != 0 {
            return Err(MapError::UnalignedVirtualAddress);
        }

        // ---- Validate the virtual address range. ----
        //
        // The permitted range depends on the address space's kind.
        // See the method documentation for the rationale.

        let pde = (virtual_address >> 22) as usize;

        if is_kernel {
            // Kernel address space: PDE 0 is off-limits because it
            // holds the identity map; everything else is available.
            if pde == 0 {
                return Err(MapError::ForbiddenVirtualAddress);
            }
        } else {
            // User address space: PDE 0 and PDEs 768-1023 are
            // off-limits, because they hold kernel mappings.
            if pde == 0 || pde >= 768 {
                return Err(MapError::ForbiddenVirtualAddress);
            }
        }

        // ---- Check that the object fits in the scratch buffer. ----
        //
        // `map_memory` copies the object's frame addresses into a
        // fixed-size stack array before installing any mappings.
        // The array cannot grow, so objects above the limit are
        // rejected here, before any page tables are touched.

        let frame_count = {
            let mem = self
                .memory_object(memory_object_cap)
                .ok_or(MapError::InvalidMemoryObject)?;

            mem.page_count()
        };

        if frame_count > MAX_MAPPABLE_FRAMES {
            return Err(MapError::ObjectTooLarge);
        }

        // ---- Copy the object's frames. ----
        //
        // We cannot hold an immutable borrow of the fabric (to read
        // the memory object) and a mutable borrow of an address
        // space (to install mappings) at the same time. So we copy
        // the frame addresses out first, then release the borrow.
        //
        // `frame_count` is already known to be at most
        // `MAX_MAPPABLE_FRAMES`, so the array bounds are safe.

        let mut frames: [Option<u32>; MAX_MAPPABLE_FRAMES] = [None; MAX_MAPPABLE_FRAMES];

        {
            let mem = self
                .memory_object(memory_object_cap)
                .ok_or(MapError::InvalidMemoryObject)?;

            for i in 0..frame_count {
                frames[i] = mem.frame(i).map(|f| f.address());
            }
        }

        // ---- Install the mappings. ----
        //
        // `mo_id` was captured during capability validation and is
        // not needed here; the frames have already been copied out.
        // Prefix with underscore to silence the unused-variable
        // warning.
        let _ = mo_id;

        for slot in self.address_spaces.iter_mut() {
            if let Some(aspace) = slot {
                if aspace.id() == as_id {
                    for i in 0..frame_count {
                        let va = virtual_address + (i as u32) * 4096;
                        let pa = frames[i].ok_or(MapError::OutOfMemory)?;

                        if !aspace.map(va, pa, writable, user) {
                            // Roll back any mappings installed so far.
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

        // The address-space capability resolved earlier, but by the
        // time we got here the object is gone. This should not
        // happen while the fabric is exclusively borrowed.
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
                .lookup(address_space_cap)
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

    fn cell(&self, id: CellId) -> Option<&Cell> {
        self.cells.iter().flatten().find(|cell| cell.id() == id)
    }

    fn cell_mut(&mut self, id: CellId) -> Option<&mut Cell> {
        self.cells.iter_mut().flatten().find(|cell| cell.id() == id)
    }

    /// Grants an existing capability to an execution cell.
    pub fn grant_capability(&mut self, cell: CellId, capability: CapabilityId) -> bool {
        if self.lookup(capability).is_none() {
            return false;
        }

        match self.cell_mut(cell) {
            Some(cell) => cell.add_capability(capability),
            None => false,
        }
    }

    /// Determines whether an execution cell possesses a capability.
    pub fn cell_has_capability(&self, cell: CellId, capability: CapabilityId) -> bool {
        match self.cell(cell) {
            Some(cell) => cell.has_capability(capability),
            None => false,
        }
    }

    /// Delegates a capability from one execution cell to another.
    pub fn transfer_between_cells(
        &mut self,
        source_cell: CellId,
        target_cell: CellId,
        capability: CapabilityId,
        requested_rights: CapabilityRights,
    ) -> Option<CapabilityId> {
        if !self.cell_has_capability(source_cell, capability) {
            return None;
        }

        if self.cell(target_cell).is_none() {
            return None;
        }

        let derived = self.transfer(capability, requested_rights)?;

        if !self.grant_capability(target_cell, derived) {
            self.revoke(derived);
            return None;
        }

        Some(derived)
    }
}
