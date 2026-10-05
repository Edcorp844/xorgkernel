use crate::capability::capability::CapabilityId;

pub const MAX_CELL_CAPABILITIES: usize = 256;

#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CellId(u32);

impl CellId {
    pub const INVALID: Self = Self(0);

    pub const fn new(id: u32) -> Self {
        Self(id)
    }

    pub const fn raw(self) -> u32 {
        self.0
    }

    pub const fn is_valid(self) -> bool {
        self.0 != 0
    }
}

pub struct Cell {
    id: CellId,

    ///
    /// The cell's local capability namespace.
    ///
    /// This does NOT create new capabilities.
    /// It records which global capability IDs this cell
    /// is permitted to possess.
    ///
    capabilities: [CapabilityId; MAX_CELL_CAPABILITIES],

    capability_count: usize,
}

impl Cell {
    pub const fn new(id: CellId) -> Self {
        Self {
            id,
            capabilities: [CapabilityId::INVALID; MAX_CELL_CAPABILITIES],
            capability_count: 0,
        }
    }

    pub const fn id(&self) -> CellId {
        self.id
    }

    pub fn add_capability(
        &mut self,
        capability: CapabilityId,
    ) -> bool {
        if capability == CapabilityId::INVALID {
            return false;
        }

        /*
         * Don't insert duplicates.
         */
        if self.has_capability(capability) {
            return true;
        }

        if self.capability_count >= MAX_CELL_CAPABILITIES {
            return false;
        }

        self.capabilities[self.capability_count] = capability;
        self.capability_count += 1;

        true
    }

    pub fn has_capability(
        &self,
        capability: CapabilityId,
    ) -> bool {
        for index in 0..self.capability_count {
            if self.capabilities[index] == capability {
                return true;
            }
        }

        false
    }

    pub fn remove_capability(
        &mut self,
        capability: CapabilityId,
    ) -> bool {
        for index in 0..self.capability_count {
            if self.capabilities[index] == capability {
                /*
                 * Compact the namespace.
                 */
                for next in index..(self.capability_count - 1) {
                    self.capabilities[next] =
                        self.capabilities[next + 1];
                }

                self.capabilities[self.capability_count - 1] =
                    CapabilityId::INVALID;

                self.capability_count -= 1;

                return true;
            }
        }

        false
    }

    pub const fn capability_count(&self) -> usize {
        self.capability_count
    }
}