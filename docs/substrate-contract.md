# Substrate Contract

This document states the invariants that each kernel subsystem
maintains and that every other subsystem may rely on.

A subsystem's **contract** is the set of promises it makes about
its observable behavior. Contracts are what make the code
composable: a caller can reason about a subsystem without reading
its implementation, provided the contract is honored.

## `arch` — Linker symbol access

**Provides:** Functions that return the addresses of
linker-defined symbols (`__kernel_start`, `__kernel_end`,
`__bootstrap_start`, `__bootstrap_stack_top`, and so on).

**Contract:**

- Each function returns a stable address, fixed at link time.
- Addresses are physical addresses; the kernel runs with an
  identity map for the low 4 MiB, so they are also valid linear
  addresses.

## `boot` — Boot protocol

**Provides:** A view of the machine as described by the
bootloader's handoff structure.

**Contract:**

- `boot::multiboot2::init(pointer)` copies the structure into a
  static buffer in the kernel's `.bss`. After this call, the
  original pointer is never dereferenced again.
- `boot::multiboot2::memory_map()` returns an iterator over the
  physical memory regions. The iterator is valid for the
  lifetime of the kernel.
- `boot::multiboot2::framebuffer_info()` returns the
  framebuffer parameters, or `None` if the bootloader did not
  provide one.
- The buffer used for the copy is 8192 bytes. If the
  bootloader's structure is larger, `init` panics. This is a
  known limit and will be raised if needed.

## `cpu::control` — Control registers

**Provides:** Reads and writes of CR0, CR2, CR3, CR4.

**Contract:**

- Reads return the current value of the register.
- Writes update the register. Writes to CR0, CR3, and CR4 are
  `unsafe` because they have side effects (enabling paging,
  flushing the TLB, changing the page-size extension).

## `cpu::gdt` — Global Descriptor Table

**Provides:** A flat 32-bit segment layout.

**Contract:**

- After `init`, CS references a ring-0 code segment at selector
  `0x08`. DS, ES, FS, GS, SS reference a ring-0 data segment at
  selector `0x10`.
- Both descriptors cover the full 4 GiB address space with
  4 KiB granularity.
- `init` reloads CS with a far return. This is required because
  `lgdt` alone does not update CS's cached descriptor.

## `cpu::idt` — Interrupt Descriptor Table

**Provides:** A 256-entry IDT with handlers for CPU exceptions
0–31 and hardware IRQs 32–47.

**Contract:**

- After `init`, delivering any of the 48 installed vectors
  transfers control to the corresponding assembly stub.
- Vectors 48–255 are marked not-present. Delivering one of them
  raises a general protection fault.
- The IDT is loaded with `lidt`. Reloading it requires a full
  `init` call.

## `cpu::exception` — Exception handling

**Provides:** A dispatcher for CPU exceptions.

**Contract:**

- The exception stub for vector N calls
  `exception_dispatch(N, registers, frame)`.
- `exception_dispatch` reports the exception to the console and
  halts the CPU. It never returns.
- Exceptions are always fatal.

## `cpu::interrupts` — IRQ dispatch

**Provides:** A registry of IRQ handlers and a dispatcher.

**Contract:**

- `register(irq, handler)` installs a handler for IRQ `irq`.
  Only one handler per IRQ is supported; registering a second
  replaces the first.
- The IRQ stub calls `irq_dispatch(irq)`.
- `irq_dispatch` sends an end-of-interrupt to the PIC **before**
  invoking the handler. This is what allows the handler to
  switch to another task without leaving the PIC waiting for
  an EOI.
- Handlers run with interrupts disabled (interrupt gates clear
  IF). Long work must be deferred.

## `cpu::pic` — 8259 PIC

**Provides:** PIC remapping, mask/unmask, and EOI.

**Contract:**

- `remap()` moves IRQ0-15 to vectors 0x20-0x2F. Must be called
  before any IRQ is unmasked.
- `unmask(irq)` enables an IRQ line. The IDT must have a valid
  handler for the corresponding vector, or the CPU will fault
  when the interrupt arrives.
- `end_of_interrupt(irq)` sends an EOI to the appropriate PIC.

## `cpu::pit` — 8254 PIT

**Provides:** A periodic timer on channel 0, wired to IRQ0.

**Contract:**

- `init(frequency_hz)` programs channel 0 to fire IRQ0 at
  approximately the requested frequency.
- The actual frequency is the closest achievable given the
  PIT's integer divisor. For typical frequencies (50–1000 Hz),
  the error is under 0.1%.

## `memory::paging` — Bootstrap paging

**Provides:** An identity map for the first 4 MiB and access to
the kernel page directory.

**Contract:**

- After `init`, virtual addresses in `0x00000000`–`0x003FFFFF`
  translate to the same physical addresses. The kernel image,
  the initial stack, and the bootstrap regions are all mapped.
- `page_directory_address()` returns the physical address of
  the kernel page directory. This is the value to load into
  CR3 to activate the kernel's address space.
- `map_page(va, pa, flags)` installs a mapping in the first
  page table. Only VA below 4 MiB are supported.

## `memory::direct_map` — Physical memory window

**Provides:** A window on the low 128 MiB of physical memory at
`0xc0000000`.

**Contract:**

- After `init`, virtual address `0xc0000000 + pa` translates to
  physical address `pa` for any `pa` in `0–128 MiB`.
- The mapping uses 4 MiB pages and PSE.
- `phys_to_virt(pa)` returns `0xc0000000 + pa`.
- `virt_to_phys(va)` returns `Some(pa)` if `va` is in the
  window, `None` otherwise.
- `map_mmio(pa, size)` installs a PCD (uncached) mapping for an
  MMIO region at `0x40000000` and returns the virtual address.

## `memory::frame` — Physical frame allocator

**Provides:** An O(1) allocator of 4 KiB physical frames.

**Contract:**

- `allocate()` returns a frame that is not reserved and not
  already allocated. The frame is marked used.
- `free(frame)` returns a frame to the free list. Panics on
  double free.
- After `init`, no frame returned by `allocate()` overlaps any
  bootstrap region (kernel image, page directory, page table,
  IDT, GDT, frame bitmap, bootstrap stack) or any region
  reserved by the bootloader.
- The allocator is a bitmap plus a free list. The bitmap is the
  source of truth for reservations; the free list is the fast
  path.

## `memory::heap` — Kernel heap

**Provides:** The kernel's global allocator, backing `Box`,
`Vec`, and the rest of `alloc`.

**Contract:**

- The heap grows by asking the fabric for memory objects of
  `REGION_FRAMES` (256) frames, mapping each into the kernel's
  address space at the next available virtual address in the
  range `0xd0000000`–`0xe0000000`.
- Each region is mapped through the fabric's `map_memory`, which
  verifies the kernel's `MAP` right on both the address space
  and the memory object.
- `dealloc` is a no-op. Freed bytes are not reclaimed.
- The heap is not interrupt-safe. Allocations must occur with
  interrupts disabled, or with a guarantee that no other task
  is allocating.
- `committed()` returns the number of bytes installed across all
  regions.

## `memory::address_space` — Address spaces

**Provides:** The `AddressSpace` type: a page directory plus
user mappings.

**Contract:**

- `new()` allocates a page directory from the frame allocator
  and copies the kernel's PDE 0 (identity map), the framebuffer
  PDEs (256–259), and PDEs 768–1023 (direct map and kernel
  regions).
- `map(va, pa, writable, user)` installs a mapping. Refuses to
  touch PDE 0, the framebuffer range, or (for user address
  spaces) PDEs 768–1023.
- `unmap(va)` removes a mapping and returns its physical
  address. Refuses the same ranges as `map`.
- `translate(va)` returns the physical address for `va`, or
  `None` if not mapped. Supports both 4 KiB and 4 MiB pages.
- `activate()` loads the page directory into CR3. This is
  `unsafe`: the caller must ensure the new address space maps
  everything the kernel is about to touch.

## `capability` — The fabric

**Provides:** The authority model. Object creation, capability
allocation, transfer, revocation, and lookup.

**Contract:**

- Every object is registered in the object registry with an
  `ObjectId` and an `ObjectKind`.
- Every capability is a slot in the ITable. It carries an
  `ObjectId`, a set of `CapabilityRights`, and a generation
  counter.
- `transfer(source, rights)` produces a new capability whose
  rights are a subset of the source's rights. The source must
  carry `SHARE`.
- Destroying an object revokes every capability referring to
  it. After destruction, no capability resolves to the object.
- A revoked capability's slot is pushed onto the ITable's free
  list. Its generation is incremented, so a stale ID cannot
  resolve to a new capability in the same slot.
- The `SHARE` right controls delegation. A capability without
  `SHARE` cannot be delegated further.
- `allocate_memory(pages, rights)` allocates frames, creates a
  memory object, registers it, and returns a capability.
- `allocate_address_space(rights)` allocates a page directory,
  wraps it as an address space, registers it, and returns a
  capability.
- `register_kernel_address_space(rights)` wraps the kernel's
  own page directory as a fabric-managed address space. Called
  once at boot.

## `sched` — Scheduling

**Provides:** A preemptive, priority-based scheduler.

**Contract:**

- Tasks are created with `create(name, entry, priority,
  stack_size)`. The task is placed in the `Ready` state and
  enqueued on its priority's run queue.
- Every task has a kernel stack allocated from the heap.
- The scheduler maintains an all-tasks list and one run queue
  per priority level. Both are intrusive doubly-linked lists
  threaded through fields in the `Task` struct.
- `schedule()` returns the highest-priority ready task's ID and
  sets that task to the `Running` state. It is O(1).
- `schedule_and_switch()` re-enqueues the current task,
  selects the next ready task, and, if it differs from the
  current task, calls `switch_context`.
- `yield_task()` invokes `schedule_and_switch` explicitly.
- Every timer tick invokes `schedule_and_switch`. Preemption is
  driven by the timer.
- `switch_context(from, to)` saves the callee-saved registers
  on `from`'s stack, stores the resulting ESP in `from->esp`,
  loads `to->esp`, restores registers, and `ret`s into `to`.
- The first time a task is scheduled, `switch_context` pops
  four zeroed registers and `ret`s to the task's entry point.
  The task function must have signature `fn() -> !`; it must
  not return.

## `console` — Output

**Provides:** A dispatcher over multiple output sinks.

**Contract:**

- `init()` sets up the serial and VGA text sinks.
- `init_framebuffer(fb)` registers a framebuffer sink. Called
  after the Multiboot2 information is parsed.
- `println!` writes to every registered sink. If a sink is not
  present, its writes are skipped.
- The console uses a spinlock. `println!` from interrupt context
  must be avoided, because the lock might be held by the
  interrupted code.

## `sync` — Synchronization primitives

**Provides:** A `SpinLock<T>`.

**Contract:**

- `lock()` returns a guard. The guard releases the lock when
  dropped.
- The lock is a single `AtomicBool` with acquire/release
  ordering.
- The lock is not interrupt-safe. Using it in a context where
  interrupts can fire while it is held will deadlock if the
  interrupt handler tries to acquire it.