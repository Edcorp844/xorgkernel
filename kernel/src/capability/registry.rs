use crate::capability::object::ObjectId;

const OBJECT_REGISTRY_SIZE: usize = 1024;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ObjectEntry {
    occupied: bool,
    generation: u16,
    object: ObjectId,
}

impl ObjectEntry {
    const fn empty() -> Self {
        Self {
            occupied: false,
            generation: 1,
            object: ObjectId::INVALID,
        }
    }
}

pub struct ObjectRegistry {
    entries: [ObjectEntry; OBJECT_REGISTRY_SIZE],
    next_id: u32,
}

impl ObjectRegistry {
    pub const fn new() -> Self {
        Self {
            entries: [const { ObjectEntry::empty() }; OBJECT_REGISTRY_SIZE],
            next_id: 1,
        }
    }

    pub fn create(&mut self) -> Option<ObjectId> {
        for entry in self.entries.iter_mut() {
            if !entry.occupied {
                let id = ObjectId::new(self.next_id);

                self.next_id = self.next_id.wrapping_add(1);

                if self.next_id == 0 {
                    self.next_id = 1;
                }

                entry.object = id;
                entry.occupied = true;

                return Some(id);
            }
        }

        None
    }

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