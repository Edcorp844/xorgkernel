//! Multiboot2 boot information.
//!
//! GRUB passes a pointer to a Multiboot2 information structure in
//! EBX when it jumps to the kernel. The structure is a list of
//! tagged records describing the machine: memory map, framebuffer,
//! boot command line, modules, and so on.
//!
//! # Copying the structure
//!
//! GRUB places the structure somewhere in physical memory, often
//! above 4 MiB. The kernel's identity map covers only the first
//! 4 MiB, so the structure becomes unreachable once paging is
//! enabled. To avoid that, `init` copies the structure into a
//! static buffer in the kernel's `.bss` before paging is set up.
//!
//! All subsequent readers use the copy.
//!
//! # Iteration
//!
//! The structure is walked using the `TagIter` iterator. Each tag
//! has an 8-byte header (type and size) followed by a body of
//! tag-specific data. Tags are padded to 8-byte boundaries, and
//! the tag list ends with a type-0 tag.
//!
//! This module provides two entry points:
//!
//! - [`memory_map`] returns an iterator over the physical memory
//!   regions described by the memory-map tag.
//! - [`framebuffer_info`] returns the parameters of the linear
//!   framebuffer, if the bootloader provided one.

use core::cell::UnsafeCell;

/// Buffer for a copy of the Multiboot2 info structure.
///
/// GRUB places the structure somewhere in low physical memory,
/// typically above 4 MiB. This buffer holds a copy that survives
/// the switch to paging.
const MULTIBOOT2_BUFFER_SIZE: usize = 8192;

static mut MULTIBOOT2_BUFFER: [u8; MULTIBOOT2_BUFFER_SIZE] = [0; MULTIBOOT2_BUFFER_SIZE];

/// The Multiboot2 information magic. GRUB passes this in EAX.
pub const MULTIBOOT2_BOOTLOADER_MAGIC: u32 = 0x36d7_6289;

/// Tag type: end of the tag list.
const TAG_END: u32 = 0;

/// Tag type: memory map.
const TAG_MEMORY_MAP: u32 = 6;

/// Tag type: framebuffer information.
const TAG_FRAMEBUFFER: u32 = 8;

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
///
/// The values match the Multiboot2 memory-map entry types.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RegionKind {
    /// Available for the kernel to use (type 0).
    ///
    /// Note: in Multiboot2, type 0 is *reserved* and type 1 is
    /// *usable*. This enum uses the semantics rather than the
    /// numeric value.
    Usable,

    /// Reserved by the firmware or hardware (type 0).
    Reserved,

    /// Reclaimable ACPI memory (type 2).
    AcpiReclaimable,

    /// ACPI non-volatile storage (type 3).
    AcpiNvs,

    /// Memory marked bad and unusable (type 4).
    BadMemory,

    /// Memory used by the bootloader and reclaimable once the
    /// kernel has taken over (type 5).
    BootloaderReclaimable,

    /// Kernel image and loaded modules (type 6).
    KernelAndModules,

    /// Framebuffer memory (type 7).
    Framebuffer,

    /// Any type the kernel does not recognise.
    Unknown,
}

impl RegionKind {
    /// Maps a Multiboot2 memory-map entry type to a `RegionKind`.
    ///
    /// Multiboot2 numbers the entry types as:
    ///
    /// ```text
    ///   0  reserved
    ///   1  usable
    ///   2  ACPI reclaimable
    ///   3  ACPI NVS
    ///   4  bad memory
    ///   5  bootloader reclaimable
    ///   6  kernel and modules
    ///   7  framebuffer
    /// ```
    ///
    /// Values 8 and above are undefined; they map to `Unknown`.
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
/// Written once by `init`, read by the memory subsystem and the
/// console initialization code.
struct BootInfoPointer(UnsafeCell<*const u8>);

unsafe impl Sync for BootInfoPointer {}

static BOOT_INFO: BootInfoPointer = BootInfoPointer(UnsafeCell::new(core::ptr::null()));

/// Copies the Multiboot2 information structure into the kernel's
/// static buffer.
///
/// Must be called exactly once, before paging is enabled, with the
/// pointer GRUB provided in EBX.
///
/// # Safety
///
/// `pointer` must be the address GRUB passed in EBX, and the
/// memory it points to must be the Multiboot2 information
/// structure.
pub unsafe fn init(pointer: *const u8) {
    if pointer.is_null() {
        return;
    }

    // Read the total size from the source. The first field of the
    // Multiboot2 structure is a 32-bit total size in bytes.
    let total_size = unsafe { core::ptr::read_unaligned(pointer as *const u32) } as usize;

    if total_size > MULTIBOOT2_BUFFER_SIZE {
        panic!("Multiboot2 structure too large");
    }

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

// ---------------------------------------------------------------------
// Tag iteration
// ---------------------------------------------------------------------

/// Iterator over the tags in the Multiboot2 structure.
struct TagIter {
    current: *const u8,
    end: *const u8,
}

impl TagIter {
    /// Creates an iterator over the tags.
    ///
    /// # Safety
    ///
    /// `info` must point at a copied Multiboot2 structure.
    unsafe fn new(info: *const u8) -> Self {
        // The structure starts with a 4-byte total size followed by
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

        // Advance to the next tag. Each tag's size field includes
        // the header and is padded to an 8-byte boundary.
        let aligned_size = (size as usize + 7) & !7;
        self.current = unsafe { self.current.add(aligned_size) };

        Some(header)
    }
}

// ---------------------------------------------------------------------
// Memory map
// ---------------------------------------------------------------------

/// Returns an iterator over the memory-map entries.
///
/// Returns `None` if no memory-map tag is present in the structure.
pub fn memory_map() -> Option<MemoryMapIter> {
    let info = info_ptr();

    if info.is_null() {
        return None;
    }

    let iter = unsafe { TagIter::new(info) };

    for tag in iter {
        let type_ = unsafe { core::ptr::read_unaligned(core::ptr::addr_of!((*tag).type_)) };

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

// ---------------------------------------------------------------------
// Framebuffer
// ---------------------------------------------------------------------

/// Framebuffer information from the Multiboot2 response.
///
/// The Multiboot2 framebuffer tag has this layout:
///
/// ```text
///   +0   u32  type            (= 8)
///   +4   u32  size            (tag size in bytes)
///   +8   u64  framebuffer_addr
///   +16  u32  framebuffer_pitch
///   +20  u32  framebuffer_width
///   +24  u32  framebuffer_height
///   +28  u8   framebuffer_bpp
///   +29  u8   framebuffer_type
///   +30  u8   reserved
///   +31  u8   reserved
///   +32  u8   red_field_position      (RGB type only)
///   +33  u8   red_mask_size
///   +34  u8   green_field_position
///   +35  u8   green_mask_size
///   +36  u8   blue_field_position
///   +37  u8   blue_mask_size
/// ```
///
/// The `framebuffer_type` field distinguishes three modes:
///
/// ```text
///   0  indexed     (palette-based, not supported by this kernel)
///   1  RGB         (direct colour, supported)
///   2  EGA text    (text-mode framebuffer)
/// ```
#[derive(Clone, Copy)]
pub struct FramebufferInfo {
    /// Physical address of the framebuffer.
    pub address: u64,

    /// Bytes per row of pixels.
    pub pitch: u32,

    /// Width in pixels.
    pub width: u32,

    /// Height in pixels.
    pub height: u32,

    /// Bits per pixel (typically 16, 24, or 32).
    pub bpp: u8,

    /// Framebuffer type (0 = indexed, 1 = RGB, 2 = EGA text).
    pub framebuffer_type: u8,

    /// Bit position of the red channel.
    pub red_shift: u8,

    /// Width in bits of the red channel.
    pub red_size: u8,

    /// Bit position of the green channel.
    pub green_shift: u8,

    /// Width in bits of the green channel.
    pub green_size: u8,

    /// Bit position of the blue channel.
    pub blue_shift: u8,

    /// Width in bits of the blue channel.
    pub blue_size: u8,
}

/// Returns the framebuffer information from the Multiboot2 response.
///
/// Returns `None` if:
///
/// - the structure is not present,
/// - no framebuffer tag is present, or
/// - the tag describes a non-RGB framebuffer (indexed or text).
pub fn framebuffer_info() -> Option<FramebufferInfo> {
    let info = info_ptr();
    if info.is_null() {
        return None;
    }

    let iter = unsafe { TagIter::new(info) };

    for tag in iter {
        let type_ = unsafe { core::ptr::read_unaligned(core::ptr::addr_of!((*tag).type_)) };

        if type_ == TAG_FRAMEBUFFER {
            let tag_addr = tag as *const u8;

            let address = unsafe { read_u64_unaligned(tag_addr.add(8)) };
            let pitch = unsafe { read_u32_unaligned(tag_addr.add(16)) };
            let width = unsafe { read_u32_unaligned(tag_addr.add(20)) };
            let height = unsafe { read_u32_unaligned(tag_addr.add(24)) };
            let bpp = unsafe { read_u8(tag_addr.add(28)) };
            let framebuffer_type = unsafe { read_u8(tag_addr.add(29)) };

            // Only RGB framebuffers are supported. Indexed and EGA
            // text modes are ignored.
            if framebuffer_type != 1 {
                return None;
            }

            let red_shift = unsafe { read_u8(tag_addr.add(32)) };
            let red_size = unsafe { read_u8(tag_addr.add(33)) };
            let green_shift = unsafe { read_u8(tag_addr.add(34)) };
            let green_size = unsafe { read_u8(tag_addr.add(35)) };
            let blue_shift = unsafe { read_u8(tag_addr.add(36)) };
            let blue_size = unsafe { read_u8(tag_addr.add(37)) };

            return Some(FramebufferInfo {
                address,
                pitch,
                width,
                height,
                bpp,
                framebuffer_type,
                red_shift,
                red_size,
                green_shift,
                green_size,
                blue_shift,
                blue_size,
            });
        }
    }

    None
}

// ---------------------------------------------------------------------
// Unaligned reads
// ---------------------------------------------------------------------

/// Reads one byte from a raw pointer.
///
/// # Safety
///
/// `ptr` must be a valid pointer to at least one readable byte.
unsafe fn read_u8(ptr: *const u8) -> u8 {
    unsafe { core::ptr::read_unaligned(ptr) }
}

/// Reads a little-endian 32-bit value from a raw pointer.
///
/// # Safety
///
/// `ptr` must be a valid pointer to at least four readable bytes.
/// The pointer need not be 4-byte aligned.
unsafe fn read_u32_unaligned(ptr: *const u8) -> u32 {
    unsafe { core::ptr::read_unaligned(ptr as *const u32) }
}

/// Reads a little-endian 64-bit value from a raw pointer.
///
/// # Safety
///
/// `ptr` must be a valid pointer to at least eight readable bytes.
/// The pointer need not be 8-byte aligned.
unsafe fn read_u64_unaligned(ptr: *const u8) -> u64 {
    unsafe { core::ptr::read_unaligned(ptr as *const u64) }
}
