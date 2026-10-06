//! Boot-time memory information.
//!
//! With GRUB, the memory map comes from the Multiboot2 information
//! structure. This module wraps the parser in
//! `crate::boot::multiboot2` and exposes it in the shape the frame
//! allocator wants.

use crate::boot::multiboot2;

/// A single usable memory region.
#[derive(Clone, Copy)]
pub struct MemoryRegion {
    pub base: u64,
    pub length: u64,
}

/// The kernel's physical load range, from the loader.
///
/// With GRUB, the kernel and its modules are described by a
/// separate Multiboot2 tag. For now, we use the linker symbols
/// (`__kernel_start`, `__kernel_end`) which are compiled into the
/// image and reflect the actual physical range.
pub fn kernel_range() -> (u32, u32) {
    let start = crate::arch::__kernel_start();
    let end = crate::arch::__kernel_end();
    (start, end)
}

/// Iterator over usable memory regions.
pub struct MemoryMap;

impl MemoryMap {
    /// Returns an iterator over every region the bootloader
    /// reported as usable.
    pub fn usable_regions(&self) -> impl Iterator<Item = MemoryRegion> {
        multiboot2::memory_map()
            .into_iter()
            .flatten()
            .inspect(|r| {
                println!(
                    "  raw region: base=0x{:x} length=0x{:x} kind={:?}",
                    r.base, r.length, r.kind
                );
            })
            .filter(|r| r.kind == multiboot2::RegionKind::Usable)
            .map(|r| MemoryRegion {
                base: r.base,
                length: r.length,
            })
    }
}

/// Returns the memory map.
///
/// Panics if the Multiboot2 structure is missing a memory-map tag,
/// which would indicate GRUB was misconfigured.
pub fn memory_map() -> MemoryMap {
    if multiboot2::memory_map().is_none() {
        panic!("Multiboot2 structure has no memory map tag");
    }
    MemoryMap
}
