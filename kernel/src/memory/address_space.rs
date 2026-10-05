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
//! # Relationship to the fabric
//!
//! An address space is a fabric-managed object. It is created by
//! [`crate::capability::core::CapabilityCore::allocate_address_space`],
//! which allocates the page directory, registers the object, and
//! returns a capability. Callers never hold a raw `AddressSpace`;
//! they hold a `CapabilityId` and present it to the fabric.
//!
//! The `id` field stores the fabric's identifier for this object.
//! It is set once, immediately after the fabric registers the
//! address space, and never changes.

use crate::capability::object::ObjectId;
use crate::memory::direct_map;
use crate::memory::frame;
use crate::memory::paging;

/// Size of a single x86 page.
pub const PAGE_SIZE: usize = 4096;

/// Page-directory entry: maps a 4 MiB page.
const PAGE_SIZE_4MB: u32 = 1 << 7;

/// First page-directory entry belonging to the kernel.
const KERNEL_PDE_START: usize = 768;

/// Number of entries in an x86 page directory.
const PAGE_DIRECTORY_ENTRIES: usize = 1024;

/// Mask for the page-offset portion of a virtual address.
const PAGE_OFFSET_MASK: u32 = 0x0000_0fff;

/// Page-table entry: page is present.
const PAGE_PRESENT: u32 = 1 << 0;

/// Page-table entry: page is writable.
const PAGE_WRITABLE: u32 = 1 << 1;

/// Page-table entry: page is accessible from user mode.
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
    /// The fabric's ID for this object.
    ///
    /// Assigned by the registry when the address space is created
    /// through the fabric. `ObjectId::INVALID` before registration.
    id: ObjectId,

    /// Physical address of this address space's page directory.
    ///
    /// Always page-aligned. Passed directly to CR3 by
    /// [`AddressSpace::activate`].
    page_directory: u32,

    /// Whether this is the kernel's own address space.
    ///
    /// The kernel address space has authority over its entire
    /// virtual range (except PDE 0, which is the identity map). A
    /// user address space is restricted to PDEs 1-767 so that it
    /// cannot overwrite the kernel mappings it inherits.
    is_kernel: bool,
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
    ///
    /// This is a low-level constructor. Callers that want a
    /// fabric-managed address space should use
    /// [`crate::capability::core::CapabilityCore::allocate_address_space`],
    /// which registers the object and returns a capability.
    pub fn new() -> Option<Self> {
        let page_directory = frame::allocate()?.address();

        direct_map::clear_page(page_directory);

        Self::copy_kernel_mappings(page_directory);

        Some(Self {
            id: ObjectId::INVALID,
            page_directory,
            is_kernel: false,
        })
    }

    /// Wraps an existing page directory as an address space.
    ///
    /// Unlike `new`, this does not allocate a page directory and
    /// does not copy kernel mappings. It is used to wrap the
    /// kernel's own address space, which was established before
    /// the fabric existed, so that the fabric can hand out a
    /// capability to it.
    ///
    /// The `id` field is left as `INVALID`; the fabric sets it
    /// during registration.
    ///
    /// # Safety
    ///
    /// The caller must ensure that `page_directory` is a valid,
    /// page-aligned page directory that is currently in use (or
    /// intended to be used as) an address space. Passing a page
    /// directory that is not properly initialized will cause
    /// undefined behavior when the address space is activated.
    pub(crate) fn from_page_directory(page_directory: u32) -> Self {
        Self {
            id: ObjectId::INVALID,
            page_directory,
            is_kernel: true,
        }
    }

    pub fn is_kernel(&self) -> bool {
        self.is_kernel
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

    /// Returns the fabric ID assigned to this object.
    ///
    /// Valid only after the fabric has registered the object.
    /// Before registration this returns `ObjectId::INVALID`.
    pub const fn id(&self) -> ObjectId {
        self.id
    }

    /// Sets the fabric ID.
    ///
    /// Called by the fabric during registration. Not public outside
    /// the crate: the caller must go through
    /// `CapabilityCore::allocate_address_space`.
    pub(crate) fn set_id(&mut self, id: ObjectId) {
        self.id = id;
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

        let entry_0 = unsafe { core::ptr::read_volatile(source.add(0)) };
        unsafe {
            core::ptr::write_volatile(destination.add(0), entry_0);
        }

        for index in KERNEL_PDE_START..PAGE_DIRECTORY_ENTRIES {
            let entry = unsafe { core::ptr::read_volatile(source.add(index)) };

            unsafe {
                core::ptr::write_volatile(destination.add(index), entry);
            }
        }
    }

    /// Maps one virtual page to one physical page.
    ///
    /// If the page directory entry covering `virtual_address` does
    /// not yet point at a page table, a page table is allocated
    /// from the frame allocator and installed.
    ///
    /// # Allowed ranges
    ///
    /// The set of virtual addresses this method will accept depends
    /// on whether this address space is the kernel's own:
    ///
    /// - **Kernel address space** (`is_kernel == true`): any PDE
    ///   except PDE 0. The kernel identity map at PDE 0 must not be
    ///   overwritten, but the rest of the virtual space is available
    ///   to the kernel. This is what allows the kernel heap to live
    ///   at `0xd0000000` (PDE 832) and eventually what will allow
    ///   the kernel to move to a higher-half layout.
    ///
    /// - **User address space** (`is_kernel == false`): PDEs 1-767
    ///   only. PDE 0 holds the kernel identity map; PDEs 768-1023
    ///   hold the kernel direct map. Both are copied from the
    ///   kernel's page directory when the address space is created,
    ///   and a user address space must not be able to overwrite
    ///   them. All user mappings live in the middle range.
    ///
    /// Attempting to map outside the allowed range returns `false`
    /// without modifying any page tables.
    ///
    /// # Return value
    ///
    /// Returns `true` on success, `false` if:
    ///
    /// - either address is not page-aligned
    /// - `virtual_address` falls outside the allowed range for
    ///   this address space
    /// - a page table could not be allocated from the frame
    ///   allocator
    ///
    /// On `false`, no partial state is left behind: either nothing
    /// was modified, or the mapping was fully installed.
    pub fn map(
        &mut self,
        virtual_address: u32,
        physical_address: u32,
        writable: bool,
        user: bool,
    ) -> bool {
        // ---- Validate alignment. ----

        if !is_page_aligned(virtual_address) || !is_page_aligned(physical_address) {
            return false;
        }

        let directory_index = page_directory_index(virtual_address);
        let table_index = page_table_index(virtual_address);

        // ---- Validate the virtual address range. ----
        //
        // The permitted range depends on whether this is the
        // kernel's address space or a user address space. See the
        // method documentation for the rationale.

        if self.is_kernel {
            // Kernel address space: PDE 0 is off-limits (it holds
            // the identity map), everything else is available.
            if directory_index == 0 {
                return false;
            }
        } else {
            // User address space: PDE 0 and PDEs 768-1023 are
            // off-limits, because they hold kernel mappings that
            // were inherited from the kernel's page directory.
            if directory_index == 0 || directory_index >= KERNEL_PDE_START {
                return false;
            }
        }

        // ---- Ensure a page table exists for this PDE. ----

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

            // The PDE flags mirror the PTE flags that will be
            // installed below: writable and, if this is a user
            // mapping, user-accessible.
            let mut directory_flags = PAGE_PRESENT | PAGE_WRITABLE;

            if user {
                directory_flags |= PAGE_USER;
            }

            directory_entry = table_address | directory_flags;

            unsafe {
                core::ptr::write_volatile(page_directory.add(directory_index), directory_entry);
            }
        } else if user && directory_entry & PAGE_USER == 0 {
            // Page table already exists, but it was created for
            // kernel-only mappings. Promote it so that user-mode
            // accesses are permitted. Individual PTEs still control
            // which pages are user-accessible.
            directory_entry |= PAGE_USER;

            unsafe {
                core::ptr::write_volatile(page_directory.add(directory_index), directory_entry);
            }
        }

        // ---- Install the page-table entry. ----

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

        if directory_entry & PAGE_SIZE_4MB != 0 {
            let physical_base = directory_entry & 0xffc0_0000;
            let offset = virtual_address & 0x003f_ffff;
            return Some(physical_base | offset);
        }

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
const fn page_directory_index(address: u32) -> usize {
    ((address >> 22) & 0x3ff) as usize
}

/// Extracts the page-table index from a virtual address.
const fn page_table_index(address: u32) -> usize {
    ((address >> 12) & 0x3ff) as usize
}

/// Returns whether an address is aligned to a 4 KiB page boundary.
const fn is_page_aligned(address: u32) -> bool {
    (address & PAGE_OFFSET_MASK) == 0
}
