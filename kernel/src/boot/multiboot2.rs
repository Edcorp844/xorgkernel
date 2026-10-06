//! Multiboot2 boot information.
//!
//! GRUB passes a pointer to a Multiboot2 information structure in
//! EBX when it jumps to the kernel. The structure is a list of
//! tagged records describing the machine: memory map, framebuffer,
//! boot command line, modules, and so on.
//!
//! This module stores the pointer received at entry and provides
//! iterators over the tags and the memory map.

use core::cell::UnsafeCell;

/// Buffer for a copy of the Multiboot2 info structure.
///
/// GRUB places the structure somewhere in low physical memory,
/// typically above 4 MiB. The kernel's identity map only covers
/// the first 4 MiB, so the structure becomes inaccessible after
/// paging is enabled. We copy it into this buffer before that,
/// and read from the copy thereafter.
const MULTIBOOT2_BUFFER_SIZE: usize = 4096;

static mut MULTIBOOT2_BUFFER: [u8; MULTIBOOT2_BUFFER_SIZE] = [0; MULTIBOOT2_BUFFER_SIZE];

/// The Multiboot2 information magic. GRUB passes this in EAX.
pub const MULTIBOOT2_BOOTLOADER_MAGIC: u32 = 0x36d7_6289;

/// Tag type: end of the tag list.
const TAG_END: u32 = 0;

/// Tag type: memory map.
const TAG_MEMORY_MAP: u32 = 6;

/// A tag header. Every tag begins with these two fields.
#[repr(C)]
struct TagHeader {
    type_: u32,
    size: u32,
}

/// The memory-map tag body.
#[repr(C)]
struct MemoryMapTag {
    header: TagHeader,
    entry_size: u32,
    entry_version: u32,
}

/// One memory-map entry.
#[repr(C)]
struct MemoryMapEntry {
    base_addr: u64,
    length: u64,
    type_: u32,
    reserved: u32,
}

/// A single memory region, in the form the kernel's frame allocator
/// wants to consume it.
#[derive(Clone, Copy)]
pub struct MemoryRegion {
    pub base: u64,
    pub length: u64,
    pub kind: RegionKind,
}

/// The kind of a memory region.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RegionKind {
    Usable,
    Reserved,
    AcpiReclaimable,
    AcpiNvs,
    BadMemory,
    BootloaderReclaimable,
    KernelAndModules,
    Framebuffer,
    Unknown,
}

impl RegionKind {
    fn from_multiboot2(value: u32) -> Self {
        match value {
            0 => Self::Reserved,
            1 => Self::Usable,
            2 => Self::AcpiReclaimable,
            3 => Self::AcpiNvs,
            4 => Self::BadMemory,
            5 => Self::BootloaderReclaimable,
            6 => Self::KernelAndModules,
            7 => Self::Framebuffer,
            _ => Self::Unknown,
        }
    }
}

/// Global storage for the Multiboot2 info pointer.
///
/// Written once by `init`, read by the memory subsystem.
struct BootInfoPointer(UnsafeCell<*const u8>);

unsafe impl Sync for BootInfoPointer {}

static BOOT_INFO: BootInfoPointer = BootInfoPointer(UnsafeCell::new(core::ptr::null()));

/// Stores the Multiboot2 information pointer.
///
/// # Safety
///
/// Must be called once, from the kernel entry point, with a
/// pointer GRUB has provided.
pub unsafe fn init(pointer: *const u8) {
    if pointer.is_null() {
        return;
    }

    // Read the total size from the source.
    let total_size = unsafe { core::ptr::read_unaligned(pointer as *const u32) } as usize;

    if total_size > MULTIBOOT2_BUFFER_SIZE {
        // The structure is larger than our buffer. Copy only the
        // first part; some tags will be missing. Panic would be
        // better than silent truncation.
        panic!("Multiboot2 structure too large");
    }

    // Copy the structure into our buffer.
    unsafe {
        core::ptr::copy_nonoverlapping(
            pointer,
            core::ptr::addr_of_mut!(MULTIBOOT2_BUFFER) as *mut u8,
            total_size,
        );
    }
}

/// Returns the Multiboot2 information pointer.
///
/// Returns a null pointer if `init` has not been called.
fn info_ptr() -> *const u8 {
    core::ptr::addr_of!(MULTIBOOT2_BUFFER) as *const u8
}

/// Iterator over the tags in the Multiboot2 structure.
struct TagIter {
    current: *const u8,
    end: *const u8,
}

impl TagIter {
    unsafe fn new(info: *const u8) -> Self {
        // The Multiboot2 structure starts with a 4-byte total size,
        // 4 bytes of reserved, then the first tag. Tags are 8-byte
        // aligned.
        let total_size = unsafe { core::ptr::read_unaligned(info as *const u32) };
        let tags_start = unsafe { info.add(8) };
        let end = unsafe { info.add(total_size as usize) };
        Self {
            current: tags_start,
            end,
        }
    }
}

impl Iterator for TagIter {
    type Item = *const TagHeader;

    fn next(&mut self) -> Option<Self::Item> {
        if self.current >= self.end {
            return None;
        }

        let header = self.current as *const TagHeader;
        let type_ = unsafe { core::ptr::read_unaligned(core::ptr::addr_of!((*header).type_)) };
        let size = unsafe { core::ptr::read_unaligned(core::ptr::addr_of!((*header).size)) };

        if type_ == TAG_END {
            return None;
        }

        // Advance to the next tag. Sizes are padded to 8 bytes.
        let aligned_size = (size as usize + 7) & !7;
        self.current = unsafe { self.current.add(aligned_size) };

        Some(header)
    }
}

/// Iterates the memory-map entries in the Multiboot2 structure.
///
/// Returns `None` if the structure has no memory-map tag.pub fn memory_map() -> Option<MemoryMapIter> {
pub fn memory_map() -> Option<MemoryMapIter> {
    let info = info_ptr();

    if info.is_null() {
        crate::println!("multiboot2: info pointer is null");
        return None;
    }

    let total_size = unsafe { core::ptr::read_unaligned(info as *const u32) };
    crate::println!("multiboot2: total_size = {}", total_size);

    let iter = unsafe { TagIter::new(info) };

    for tag in iter {
        let type_ = unsafe { core::ptr::read_unaligned(core::ptr::addr_of!((*tag).type_)) };
        let size = unsafe { core::ptr::read_unaligned(core::ptr::addr_of!((*tag).size)) };
        crate::println!("multiboot2: tag type={} size={}", type_, size);

        if type_ == TAG_MEMORY_MAP {
            let map_tag = tag as *const MemoryMapTag;

            let entry_size =
                unsafe { core::ptr::read_unaligned(core::ptr::addr_of!((*map_tag).entry_size)) };
            let header_size = core::mem::size_of::<MemoryMapTag>();
            let tag_size =
                unsafe { core::ptr::read_unaligned(core::ptr::addr_of!((*map_tag).header.size)) };

            let entries_start = unsafe { (tag as *const u8).add(header_size) };
            let entries_end = unsafe { (tag as *const u8).add(tag_size as usize) };

            return Some(MemoryMapIter {
                current: entries_start,
                end: entries_end,
                entry_size: entry_size as usize,
            });
        }
    }
    None
}

/// Iterator over the memory-map entries.
pub struct MemoryMapIter {
    current: *const u8,
    end: *const u8,
    entry_size: usize,
}

impl Iterator for MemoryMapIter {
    type Item = MemoryRegion;

    fn next(&mut self) -> Option<Self::Item> {
        if self.current >= self.end {
            return None;
        }

        let entry = self.current as *const MemoryMapEntry;

        let base = unsafe { core::ptr::read_unaligned(core::ptr::addr_of!((*entry).base_addr)) };
        let length = unsafe { core::ptr::read_unaligned(core::ptr::addr_of!((*entry).length)) };
        let type_ = unsafe { core::ptr::read_unaligned(core::ptr::addr_of!((*entry).type_)) };

        self.current = unsafe { self.current.add(self.entry_size) };

        Some(MemoryRegion {
            base,
            length,
            kind: RegionKind::from_multiboot2(type_),
        })
    }
}
