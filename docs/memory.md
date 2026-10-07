# Memory Model

This document describes the kernel's memory subsystem: how
physical frames are managed, how memory objects are built on
top of them, how address spaces map them, and how all of this
is governed by the capability fabric.

For the code, see `kernel/src/memory/`. For the invariants the
memory subsystem maintains, see `substrate-contract.md`. For
the fabric, see `capability.md`.

## The hierarchy

Memory in XORG passes through five layers, from lowest to
highest:

1. **Physical frames.** 4 KiB pages of physical memory. The
   frame allocator hands them out and takes them back.
2. **Memory objects.** Named, bounded regions of physical
   frames. A memory object is the fabric's unit of authority
   for memory.
3. **Address spaces.** Page directories plus mappings. An
   address space is how a memory object becomes visible at a
   virtual address.
4. **Mappings.** The installation of a memory object's frames
   into an address space at a chosen virtual address.
5. **Capabilities.** The authority to allocate memory objects,
   map them, transfer them, revoke them, and destroy them.

Each layer is a strict refinement of the one below it. A
frame does not know it is part of a memory object. A memory
object does not know it has been mapped. A mapping does not
know who holds the capability that authorized it.

The layers are separated so that each can be reasoned about
in isolation. The frame allocator is concerned only with
which physical frames are free. The memory object is
concerned only with which frames it owns. The address space
is concerned only with what is mapped where. The fabric is
concerned only with who has authority.

## Physical frames

Physical memory is managed by the frame allocator in
`memory/frame.rs`. It owns every 4 KiB frame in the system
and hands them out one at a time.

The allocator maintains two data structures:

- **A bitmap.** One bit per frame, covering the full 4 GiB
  32-bit physical address space. The bitmap is the source of
  truth for which frames are reserved. It is 128 KiB, placed
  in a linker-defined region.
- **A free list.** A singly-linked list threaded through the
  free frames themselves. The first four bytes of each free
  frame hold the frame number of the next free frame. The
  allocator keeps a head pointer.

Allocation pops the head of the free list and marks the frame
used in the bitmap. Freeing pushes onto the head and marks
the frame free. Both operations are O(1) and touch only one
frame's worth of memory.

The bitmap is retained because it is compact, easy to audit,
and gives O(1) `is_used` queries. It is not on the allocation
fast path. The free list is built once at boot, after every
reservation has been applied, and never rebuilt.

### Initialization

The frame allocator initializes in four phases.

1. **Reserve everything.** Every bit is set to used. This
   gives a known starting state from which the other phases
   only clear bits.
2. **Mark usable regions.** The bootloader's memory map tells
   the allocator which physical ranges are usable RAM. Frames
   in those ranges are cleared in the bitmap.
3. **Reserve boot memory.** The kernel image, the page
   directory, the page table, the IDT, the GDT, the frame
   bitmap, the bootstrap stack, and the first 64 KiB of
   physical memory are marked used again. Without this step,
   the allocator would hand out frames that overlap kernel
   code or bootstrap data.
4. **Build the free list.** Every frame that is still free is
   pushed onto the free list.

After initialization, the allocator hands out frames on
demand. The invariants are:

- Every frame returned by `allocate` is free in the bitmap
  and not reserved.
- Every frame returned by `allocate` is removed from the free
  list.
- `free` panics on double free.

### What is reserved at boot

The frame allocator reserves these ranges at boot:

- **0x00000000 – 0x00010000.** The first 64 KiB. Contains the
  BIOS interrupt vector table, the BIOS data area, the
  bootloader's boot sector, the boot info structure, and the
  E820 buffer.
- **The kernel image.** From `__kernel_start` to `__kernel_end`,
  as reported by the linker.
- **The bootstrap regions.** From `__bootstrap_start` to
  `__kernel_end`. Includes the page directory, page table,
  IDT, GDT, frame bitmap, and `.bss`.
- **The bootstrap stack.** From `__bootstrap_stack_bottom` to
  `__bootstrap_stack_top`.
- **The frame bitmap itself.** From `__frame_bitmap_start` to
  `__frame_bitmap_end`.

None of these ranges can be handed out as a frame.

## Memory objects

A memory object is a bounded, named region of physical frames.
It is the fabric's unit of authority for memory: to allocate
memory is to create a memory object, and to share memory is to
transfer a capability to one.

A memory object is created by
`CapabilityCore::allocate_memory`, which:

1. Allocates the requested number of frames from the frame
   allocator.
2. Creates a `MemoryObject` holding them.
3. Registers the object in the object registry with
   `ObjectKind::MemoryObject`.
4. Returns a capability with the requested rights.

The caller never sees the object directly. It sees a
capability. To map, read, write, or transfer the object, the
caller presents the capability, and the fabric checks it.

### Storage

A memory object stores its frames in one of two forms,
depending on the object's size.

**Inline.** Objects of at most `INLINE_FRAMES` frames (256,
or 1 MiB) store the frame list as a fixed-size array inside
the object. No heap allocation is needed.

**Heap.** Larger objects store the frame list as a
heap-allocated `Vec<Frame>`.

The two-tier design exists to break a bootstrap cycle: the
kernel heap is itself backed by memory objects, and if
allocating a memory object required a heap allocation, the
heap could not grow. By keeping small objects inline, the
heap's own growth requests (which are always small) never
touch the heap.

The `INLINE_FRAMES` constant must be at least as large as the
heap's `REGION_FRAMES` constant, for the same reason. Both are
currently 256.

### Lifetime

A memory object owns its frames. When it is dropped, every
frame is returned to the frame allocator. The fabric drops the
object when the last capability to it is revoked and the
object itself is destroyed.

The order matters:

1. `CapabilityCore::destroy_object` revokes every capability
   referring to the object.
2. The registry marks the slot free.
3. The `MemoryObject` is dropped, returning its frames.

Because capabilities are revoked before the object is
dropped, no cell can be holding a stale reference when the
frames are freed.

## Address spaces

An address space is a page directory plus the user mappings
installed beneath it. It is how a memory object becomes
visible at a virtual address.

An address space is created by
`CapabilityCore::allocate_address_space`, which:

1. Allocates a page directory from the frame allocator.
2. Copies the kernel mappings from the kernel's own page
   directory. This includes the identity map at PDE 0, the
   framebuffer at PDEs 256–259, and the direct map and other
   kernel regions at PDEs 768–1023.
3. Registers the object in the object registry with
   `ObjectKind::AddressSpace`.
4. Returns a capability with the requested rights.

### Layout

The 32-bit virtual address space is divided into four
regions:

- **PDE 0.** Identity map of the low 4 MiB. Contains kernel
  code, data, and stack. Present in every address space.
- **PDEs 1–255.** User mappings.
- **PDEs 256–259.** Framebuffer mapping. Present in every
  address space.
- **PDEs 260–767.** More user mappings.
- **PDEs 768–1023.** Kernel direct map and other kernel-only
  regions. Present in every address space.

The identity map at PDE 0 is essential. Kernel code and the
kernel stack live in the low 4 MiB, and once CR3 points at a
new page directory, the CPU must still be able to fetch and
execute kernel instructions. Without PDE 0, the very next
instruction fetch after `activate` would fault.

The framebuffer at PDEs 256–259 is also essential. The console
writes to the framebuffer regardless of which address space is
active. Without the framebuffer mapping, any print after
switching CR3 would fault, and the fault handler would try to
print to the same console, causing a recursive fault.

### Kernel mappings are copied

When a new address space is created, the kernel's PDE 0,
PDEs 256–259, and PDEs 768–1023 are copied verbatim. These
PDEs are never modified by the address space's own `map` and
`unmap` operations.

The copy happens at creation time. After that, the address
space's own PDEs 1–255 and 260–767 are used for user
mappings. Each new mapping allocates a page table if one does
not yet exist.

### Range restrictions

`AddressSpace::map` refuses to touch:

- PDE 0, the kernel identity map.
- PDEs 256–259, the framebuffer.
- PDEs 768–1023, the direct map and other kernel regions.

These restrictions keep user mappings from overwriting kernel
mappings.

For the kernel's own address space, the framebuffer range is
also protected, because the heap maps its regions into PDE
832 (`0xd0000000`), which is in the direct map range, not in
the framebuffer range. So the framebuffer restrictions apply
uniformly.

## Mappings

A mapping is the installation of a memory object's frames into
an address space at a chosen virtual address. It is performed
by `CapabilityCore::map_memory`.

Mapping takes two capabilities, not one:

- A capability to the address space, carrying `MAP`.
- A capability to the memory object, carrying `MAP`.

Both rights are checked. This is the essence of the capability
model: you cannot map memory you do not hold a capability to,
and you cannot map into an address space you do not hold a
capability to.

The operation installs the object's frames at
`virtual_address`, `virtual_address + 4096`, and so on, one
page per frame. If any frame fails to map, the frames already
installed are unmapped and the call returns an error. No
partial state is left behind.

### Address translation

Address translation walks the page tables. For a 4 KiB page:

1. Extract the page-directory index from the virtual address.
2. Read the PDE. If not present, the page is unmapped.
3. If the PDE's PS bit is set, it is a 4 MiB page. The
   physical base is the PDE's address field, and the offset is
   the low 22 bits of the virtual address.
4. Otherwise, the PDE points at a page table. Extract the
   page-table index from the virtual address.
5. Read the PTE. If not present, the page is unmapped.
6. The physical page is the PTE's address field. The offset is
   the low 12 bits of the virtual address.

`AddressSpace::translate` implements this walk. It is used for
diagnostics and for verifying that mappings are installed
correctly.

## The kernel heap

The kernel heap is a general-purpose allocator for the
kernel's own bookkeeping. It backs `Box`, `Vec`, `String`, and
every other `alloc` type.

The heap is a bump allocator over a dedicated virtual range at
`0xd0000000`–`0xe0000000`. When it runs out of space, it asks
the fabric for a memory object of `REGION_FRAMES` frames,
maps it into the kernel's address space at the next available
virtual address, and continues from there.

The heap is capability-mediated. Every region it installs is
mapped through `map_memory`, which checks the kernel's `MAP`
right on both the address space and the memory object. The
kernel does not use the direct map to reach heap memory, and
does not assume that the frames backing a region are
physically contiguous.

The current implementation is a pure bump allocator: `dealloc`
is a no-op. Freed bytes are not reclaimed. This is acceptable
for the kernel's current usage, where the data structures are
long-lived. A free-list allocator will replace it when the
kernel has enough dynamic churn to justify it.

## The direct map

The direct map at `0xc0000000` gives the kernel a stable
virtual window on physical memory 0–128 MiB. It uses 4 MiB
pages and the PSE feature.

The direct map is used by the substrate itself: the frame
allocator's free list, the page-table manipulator, and the
fabric's internal bookkeeping all reach physical memory
through the direct map.

The direct map is not used by the heap. The heap uses its own
virtual range and reaches memory only through `map_memory`.

The framebuffer is a separate MMIO mapping at `0x40000000`,
with the PCD (cache disable) bit set. This is required because
the framebuffer is a device, not RAM: writes must go directly
to the device and not be buffered in the CPU cache.

## Interaction with the fabric

The fabric governs every operation on memory. It enforces:

- A memory object can only be created by
  `allocate_memory`, which checks that the caller has
  authority to allocate.
- A memory object can only be mapped by `map_memory`, which
  checks `MAP` on both the address space and the object.
- A memory object can only be transferred by `transfer`, which
  checks `SHARE` on the source capability and attenuates the
  rights.
- A memory object can only be destroyed by `destroy_object`,
  which revokes every capability referring to it.

The fabric never touches frames directly. It calls the frame
allocator to allocate and free them, and the memory object to
store them. Its responsibility is authority, not storage.

## What is not yet implemented

- **Reclaiming freed heap memory.** `dealloc` is a no-op.
- **Freeing page tables when an address space is destroyed.**
  The page directory frame is freed when the address space is
  dropped, but the page tables it points at are not.
- **Copy-on-write.** All mappings are explicit; there is no
  shared-then-copy mechanism.
- **Demand paging.** All frames are committed at allocation
  time. There is no lazy allocation.
- **Swapping.** There is no backing store for physical memory.
- **Sharing a memory object across cells.** The fabric allows
  it (transfer a capability), but no caller currently does.
- **DMA buffers.** There is no IOMMU support or DMA object
  type.

These will be added as the architecture grows. The current
memory subsystem is the minimum needed to run the kernel and
its tests.