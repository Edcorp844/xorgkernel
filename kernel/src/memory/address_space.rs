//! Virtual address spaces.
//!
//! An [`AddressSpace`] owns a page directory and the user mappings
//! installed beneath it. Kernel mappings are inherited from the
//! kernel's own page directory so that the kernel remains executable
//! and reachable after switching to a new address space.
//!
//! # Layout assumptions
//!
//! The current kernel uses a simple, flat layout. Every address
//! space has the same PDE structure at the top level:
//!
//! ```text
//! PDE 0        identity map of the low 4 MiB
//!              (kernel code, data, stack)
//!
//! PDEs 1-767   user mappings
//!              (available for AddressSpace::map)
//!
//! PDEs 768-    kernel direct map and kernel-only regions
//! 1023         (copied verbatim from the kernel page directory)
//! ```
//!
//! The split is enforced by:
//!
//! - [`AddressSpace::copy_kernel_mappings`], which copies PDE 0 and
//!   PDEs 768-1023 from the kernel page directory.
//! - [`AddressSpace::map`] and [`AddressSpace::unmap`], which refuse
//!   to touch PDE 0 or any PDE at or above `KERNEL_PDE_START`.
//!
//! The identity map at PDE 0 is essential. Kernel code and the
//! kernel stack live in the low 4 MiB, and once CR3 points at a new
//! page directory, the CPU must still be able to fetch and execute
//! kernel instructions. Without PDE 0, the very next instruction
//! fetch after `activate` would fault.
//!
//! # Future direction
//!
//! As the kernel grows, this flat model will be replaced by a
//! higher-half layout, where the kernel lives at a dedicated virtual
//! range (for example `0xf000_0000` and above) and the low 4 GiB are
//! available for user mappings. That change does not affect the
//! structure of this module: it only changes which PDEs are copied
//! and which are reserved for users.

use crate::memory::direct_map;
use crate::memory::frame;
use crate::memory::paging;

/// Size of a single x86 page, in bytes.
pub const PAGE_SIZE: usize = 4096;

/// Page-directory entry flag: this entry maps a 4 MiB page.
///
/// When the PSE bit is set, the entry's address field points
/// directly at a 4 MiB-aligned physical frame instead of at a page
/// table.
const PAGE_SIZE_4MB: u32 = 1 << 7;

/// First page-directory entry belonging to the kernel.
///
/// PDEs from this index upward are reserved for kernel mappings and
/// are copied verbatim from the kernel page directory into every new
/// address space.
const KERNEL_PDE_START: usize = 768;

/// Number of entries in an x86 page directory.
const PAGE_DIRECTORY_ENTRIES: usize = 1024;

/// Mask for the page-offset portion of a virtual address.
///
/// Also used to strip flags from PDEs and PTEs, since their address
/// fields are aligned to 4 KiB.
const PAGE_OFFSET_MASK: u32 = 0x0000_0fff;

/// Page-table entry flag: page is present.
const PAGE_PRESENT: u32 = 1 << 0;

/// Page-table entry flag: page is writable.
const PAGE_WRITABLE: u32 = 1 << 1;

/// Page-table entry flag: page is accessible from user mode.
const PAGE_USER: u32 = 1 << 2;

/// A 32-bit x86 virtual address space.
///
/// Each address space owns its own page directory. User mappings are
/// installed below `KERNEL_PDE_START`; kernel mappings are inherited
/// from the kernel page directory when the address space is created.
///
/// The page directory's physical address is the value loaded into
/// CR3 to activate the address space. It is stable for the lifetime
/// of the address space.
pub struct AddressSpace {
    /// Physical address of this address space's page directory.
    ///
    /// Always page-aligned. Passed directly to CR3 by
    /// [`AddressSpace::activate`].
    page_directory: u32,
}

impl AddressSpace {
    /// Creates a new address space.
    ///
    /// The new address space receives:
    ///
    /// - a freshly allocated, zeroed page directory
    /// - the kernel identity map (PDE 0) copied from the kernel PD
    /// - the kernel direct map and kernel-only regions (PDEs
    ///   768-1023) copied from the kernel PD
    ///
    /// User mappings are not installed; the caller adds them with
    /// [`AddressSpace::map`].
    ///
    /// Returns `None` if the frame allocator cannot provide a frame
    /// for the page directory.
    pub fn new() -> Option<Self> {
        let page_directory = frame::allocate()?.address();

        direct_map::clear_page(page_directory);

        Self::copy_kernel_mappings(page_directory);

        Some(Self { page_directory })
    }

    /// Activates this address space by loading its page directory
    /// into CR3.
    ///
    /// After this call, every virtual address is translated through
    /// this address space's page tables. The kernel remains
    /// reachable because kernel mappings are copied into every
    /// address space at creation time.
    ///
    /// # Safety
    ///
    /// The caller must ensure that:
    ///
    /// - the page directory is well-formed (all PDEs point at valid
    ///   page tables or are marked not-present)
    /// - any memory the caller is about to touch is mapped in this
    ///   address space
    ///
    /// Violating either of these can lead to a page fault that the
    /// kernel cannot recover from, or to silent memory corruption.
    pub unsafe fn activate(&self) {
        unsafe {
            crate::cpu::control::write_cr3(self.page_directory);
        }
    }

    /// Returns the physical address of this address space's page
    /// directory.
    ///
    /// This is the value that would be loaded into CR3 to activate
    /// the address space.
    pub const fn page_directory(&self) -> u32 {
        self.page_directory
    }

    /// Copies the kernel portion of the kernel page directory into a
    /// newly created page directory.
    ///
    /// The following PDEs are copied verbatim:
    ///
    /// - PDE 0: the identity map covering the low 4 MiB. The kernel
    ///   image and the kernel stack live here, and the CPU must be
    ///   able to fetch kernel instructions and push/pop on the
    ///   kernel stack immediately after a CR3 switch.
    /// - PDEs 768-1023: the kernel direct map and other kernel-only
    ///   regions.
    ///
    /// PDEs 1-767 are left zero. These are the entries available
    /// for user mappings, and [`AddressSpace::map`] will allocate
    /// page tables for them on demand.
    fn copy_kernel_mappings(page_directory: u32) {
        let source_address = paging::page_directory_address();
        let source = direct_map::phys_to_virt(source_address) as *const u32;
        let destination = direct_map::phys_to_virt(page_directory) as *mut u32;

        // Identity map: the kernel's code, data, and stack live in
        // the low 4 MiB. Copying PDE 0 makes them reachable in the
        // new address space without any further setup.
        let entry_0 = unsafe { core::ptr::read_volatile(source.add(0)) };
        unsafe {
            core::ptr::write_volatile(destination.add(0), entry_0);
        }

        // Direct map and other kernel-only regions.
        for index in KERNEL_PDE_START..PAGE_DIRECTORY_ENTRIES {
            let entry = unsafe { core::ptr::read_volatile(source.add(index)) };

            unsafe {
                core::ptr::write_volatile(destination.add(index), entry);
            }
        }

        // PDEs 1-767 remain zero: this is where user mappings live.
    }

    /// Maps one virtual page to one physical page.
    ///
    /// If the page directory entry covering `virtual_address` does
    /// not yet point at a page table, a page table is allocated
    /// from the frame allocator and installed.
    ///
    /// # Restrictions
    ///
    /// The mapping is refused if:
    ///
    /// - either address is not page-aligned
    /// - `virtual_address` falls in PDE 0 (the kernel identity map)
    /// - `virtual_address` falls at or above `KERNEL_PDE_START`
    ///
    /// These restrictions keep user mappings from overwriting the
    /// kernel's own PDEs.
    ///
    /// Returns `true` on success, `false` if the mapping was
    /// refused or a page table could not be allocated.
    pub fn map(
        &mut self,
        virtual_address: u32,
        physical_address: u32,
        writable: bool,
        user: bool,
    ) -> bool {
        if !is_page_aligned(virtual_address) || !is_page_aligned(physical_address) {
            return false;
        }

        let directory_index = page_directory_index(virtual_address);
        let table_index = page_table_index(virtual_address);

        // Refuse PDE 0 (kernel identity map) and kernel PDEs.
        if directory_index == 0 || directory_index >= KERNEL_PDE_START {
            return false;
        }

        let page_directory = direct_map::phys_to_virt(self.page_directory) as *mut u32;

        let mut directory_entry =
            unsafe { core::ptr::read_volatile(page_directory.add(directory_index)) };

        if directory_entry & PAGE_PRESENT == 0 {
            // No page table yet. Allocate one.
            let Some(table_frame) = frame::allocate() else {
                return false;
            };

            let table_address = table_frame.address();

            direct_map::clear_page(table_address);

            let mut directory_flags = PAGE_PRESENT | PAGE_WRITABLE;

            if user {
                directory_flags |= PAGE_USER;
            }

            directory_entry = table_address | directory_flags;

            unsafe {
                core::ptr::write_volatile(page_directory.add(directory_index), directory_entry);
            }
        } else if user && directory_entry & PAGE_USER == 0 {
            // Page table already exists but is kernel-only. Promote
            // it so that user-mode accesses are permitted. This
            // grants user access at the PDE level; individual PTEs
            // still control which pages are user-accessible.
            directory_entry |= PAGE_USER;

            unsafe {
                core::ptr::write_volatile(page_directory.add(directory_index), directory_entry);
            }
        }

        let table_address = directory_entry & !PAGE_OFFSET_MASK;

        let page_table = direct_map::phys_to_virt(table_address) as *mut u32;

        let mut entry = physical_address | PAGE_PRESENT;

        if writable {
            entry |= PAGE_WRITABLE;
        }

        if user {
            entry |= PAGE_USER;
        }

        unsafe {
            core::ptr::write_volatile(page_table.add(table_index), entry);
        }

        true
    }

    /// Removes a virtual-page mapping.
    ///
    /// Clears the page-table entry that maps `virtual_address`. The
    /// physical frame is not returned to the frame allocator; the
    /// caller is responsible for reclaiming it if appropriate.
    ///
    /// Returns the physical address that was mapped, or `None` if:
    ///
    /// - `virtual_address` is not page-aligned
    /// - the address falls in PDE 0 or at or above `KERNEL_PDE_START`
    /// - no mapping was present
    pub fn unmap(&mut self, virtual_address: u32) -> Option<u32> {
        if !is_page_aligned(virtual_address) {
            return None;
        }

        let directory_index = page_directory_index(virtual_address);
        let table_index = page_table_index(virtual_address);

        if directory_index == 0 || directory_index >= KERNEL_PDE_START {
            return None;
        }

        let page_directory = direct_map::phys_to_virt(self.page_directory) as *mut u32;

        let directory_entry =
            unsafe { core::ptr::read_volatile(page_directory.add(directory_index)) };

        if directory_entry & PAGE_PRESENT == 0 {
            return None;
        }

        let table_address = directory_entry & !PAGE_OFFSET_MASK;

        let page_table = direct_map::phys_to_virt(table_address) as *mut u32;

        let entry = unsafe { core::ptr::read_volatile(page_table.add(table_index)) };

        if entry & PAGE_PRESENT == 0 {
            return None;
        }

        unsafe {
            core::ptr::write_volatile(page_table.add(table_index), 0);
        }

        Some(entry & !PAGE_OFFSET_MASK)
    }

    /// Translates a virtual address to its physical address.
    ///
    /// Supports both 4 KiB pages and 4 MiB large pages. The latter
    /// are used by the kernel direct map, so translating a kernel
    /// virtual address requires understanding PDEs with the PS bit
    /// set.
    ///
    /// Returns `None` if the virtual page is not mapped.
    pub fn translate(&self, virtual_address: u32) -> Option<u32> {
        let directory_index = page_directory_index(virtual_address);
        let table_index = page_table_index(virtual_address);

        let page_directory = direct_map::phys_to_virt(self.page_directory) as *const u32;

        let directory_entry =
            unsafe { core::ptr::read_volatile(page_directory.add(directory_index)) };

        if directory_entry & PAGE_PRESENT == 0 {
            return None;
        }

        // 4 MiB large page: the PDE itself provides the physical
        // base address. The low 22 bits of the virtual address are
        // added directly.
        if directory_entry & PAGE_SIZE_4MB != 0 {
            let physical_base = directory_entry & 0xffc0_0000;

            let offset = virtual_address & 0x003f_ffff;

            return Some(physical_base | offset);
        }

        // Ordinary 4 KiB page: walk the page table.
        let table_address = directory_entry & !PAGE_OFFSET_MASK;

        let page_table = direct_map::phys_to_virt(table_address) as *const u32;

        let entry = unsafe { core::ptr::read_volatile(page_table.add(table_index)) };

        if entry & PAGE_PRESENT == 0 {
            return None;
        }

        let physical_page = entry & !PAGE_OFFSET_MASK;

        let offset = virtual_address & PAGE_OFFSET_MASK;

        Some(physical_page | offset)
    }

    /// Returns whether a virtual address is currently mapped in this
    /// address space.
    pub fn is_mapped(&self, virtual_address: u32) -> bool {
        self.translate(virtual_address).is_some()
    }
}

/// Extracts the page-directory index from a virtual address.
///
/// Bits 22-31 of the virtual address select a PDE.
const fn page_directory_index(address: u32) -> usize {
    ((address >> 22) & 0x3ff) as usize
}

/// Extracts the page-table index from a virtual address.
///
/// Bits 12-21 of the virtual address select a PTE within the page
/// table chosen by `page_directory_index`.
const fn page_table_index(address: u32) -> usize {
    ((address >> 12) & 0x3ff) as usize
}

/// Returns whether an address is aligned to a 4 KiB page boundary.
const fn is_page_aligned(address: u32) -> bool {
    (address & PAGE_OFFSET_MASK) == 0
}
