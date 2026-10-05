use crate::capability::capability::{Capability, CapabilityId, CapabilityRights};

use crate::capability::object::ObjectId;

const ITABLE_SIZE: usize = 1024;

#[repr(C)]
pub struct ITableEntry {
    generation: u16,
    occupied: bool,
    capability: Option<Capability>,
}

impl ITableEntry {
    const fn empty() -> Self {
        Self {
            generation: 1,
            occupied: false,
            capability: None,
        }
    }
}

pub struct ITable {
    entries: [ITableEntry; ITABLE_SIZE],
}

impl ITable {
    pub const fn new() -> Self {
        Self {
            entries: [const { ITableEntry::empty() }; ITABLE_SIZE],
        }
    }

    pub fn allocate(&mut self, object: ObjectId, rights: CapabilityRights) -> Option<CapabilityId> {
        for index in 0..ITABLE_SIZE {
            let entry = &mut self.entries[index];

            if !entry.occupied {
                let generation = entry.generation;

                let id = CapabilityId::new(index as u16, generation);

                entry.capability = Some(Capability::new(id, object, rights));

                entry.occupied = true;

                return Some(id);
            }
        }

        None
    }

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

        entry.generation = entry.generation.wrapping_add(1);

        if entry.generation == 0 {
            entry.generation = 1;
        }

        true
    }

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

                revoked += 1;
            }
        }

        revoked
    }

    pub fn derive(
        &mut self,
        parent: CapabilityId,
        rights: CapabilityRights,
    ) -> Option<CapabilityId> {
        let (object, parent_rights) = {
            let capability = self.lookup(parent)?;

            (capability.object(), capability.rights())
        };

        if !parent_rights.contains(CapabilityRights::GRANT) {
            return None;
        }

        if !parent_rights.contains(rights) {
            return None;
        }

        self.allocate(object, rights)
    }
}
