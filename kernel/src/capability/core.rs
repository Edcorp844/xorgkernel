use crate::capability::capability::{Capability, CapabilityId, CapabilityRights};
use crate::capability::cell::{Cell, CellId};
use crate::capability::itable::ITable;
use crate::capability::object::ObjectId;
use crate::capability::registry::ObjectRegistry;

/// Maximum number of execution cells managed by one capability domain.
const MAX_CELLS: usize = 64;

/// Central authority for objects, capabilities, and execution cells.
pub struct CapabilityCore {
    /// Global capability table.
    itable: ITable,

    /// Registry containing all live kernel objects.
    registry: ObjectRegistry,

    /// Execution cells managed by this capability domain.
    cells: [Option<Cell>; MAX_CELLS],

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
            next_cell_id: 1,
        }
    }

    /// Creates a new kernel object.
    pub fn create_object(&mut self) -> Option<ObjectId> {
        self.registry.create()
    }

    /// Resolves an object identifier.
    pub fn lookup_object(&self, object: ObjectId) -> Option<ObjectId> {
        self.registry.lookup(object)
    }

    /// Destroys an object and revokes every capability referring to it.
    pub fn destroy_object(&mut self, object: ObjectId) -> bool {
        if self.registry.lookup(object).is_none() {
            return false;
        }

        self.itable.revoke_object(object);
        self.registry.destroy(object)
    }

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
    /// The source capability must contain [`CapabilityRights::GRANT`].
    /// The requested rights must be a subset of the source capability's
    /// rights.
    ///
    /// Rights can therefore only be attenuated during delegation.
    pub fn transfer(
        &mut self,
        source: CapabilityId,
        requested_rights: CapabilityRights,
    ) -> Option<CapabilityId> {
        let (object, source_rights) = {
            let capability = self.lookup(source)?;

            (capability.object(), capability.rights())
        };

        if !source_rights.contains(CapabilityRights::GRANT) {
            return None;
        }

        if !source_rights.contains(requested_rights) {
            return None;
        }

        self.allocate(object, requested_rights)
    }

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

    /// Returns an execution cell by identifier.
    fn cell(&self, id: CellId) -> Option<&Cell> {
        self.cells.iter().flatten().find(|cell| cell.id() == id)
    }

    /// Returns an execution cell mutably by identifier.
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
    ///
    /// The source cell must possess the source capability.
    /// The resulting capability is attenuated according to the requested
    /// rights and is inserted into the target cell's capability namespace.
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
