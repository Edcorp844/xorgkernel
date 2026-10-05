pub const BOOT_INFO_ADDRESS: usize = 0x5000;

const E820_MAX_ENTRIES: usize = 64;

const E820_TYPE_USABLE: u32 = 1;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct MemoryRegion {
    pub base: u64,
    pub length: u64,
    pub region_type: u32,
    pub attributes: u32,
}

#[repr(C)]
pub struct BootInfo {
    pub magic: u32,
    pub memory_map_count: u32,
    pub memory_map_address: u32,
    pub kernel_start: u32,
    pub kernel_end: u32,
}

pub const BOOT_INFO_MAGIC: u32 = 0x4B42_494F; // "KBIO"

pub struct MemoryMap {
    entries: &'static [MemoryRegion],
}

impl MemoryMap {
    pub fn entries(&self) -> &'static [MemoryRegion] {
        self.entries
    }

    pub fn usable_regions(&self) -> impl Iterator<Item = &'static MemoryRegion> {
        self.entries
            .iter()
            .filter(|entry| entry.region_type == E820_TYPE_USABLE)
    }
}

pub fn boot_info() -> &'static BootInfo {
    unsafe { &*(BOOT_INFO_ADDRESS as *const BootInfo) }
}

pub fn memory_map() -> MemoryMap {
    let info = boot_info();

    if info.magic != BOOT_INFO_MAGIC {
        panic!("invalid boot information");
    }

    let count = info.memory_map_count as usize;

    if count > E820_MAX_ENTRIES {
        panic!("invalid memory map count");
    }

    let address = info.memory_map_address as usize;

    let entries = unsafe { core::slice::from_raw_parts(address as *const MemoryRegion, count) };

    MemoryMap { entries }
}
