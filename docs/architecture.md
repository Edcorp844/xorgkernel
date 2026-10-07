# Kernel Architecture

This document describes the kernel crate's module structure:
what each module is responsible for, how the modules depend
on each other, and where to find the code for a given
subsystem.

For the architectural vision, see `README.md`. For the boot
sequence, see `boot-sequence.md`. For the invariants each
subsystem maintains, see `substrate-contract.md`. For the
plan to run user code, see `cell-model.md`.

## Module layout

The kernel source is organized into nine subsystems, each in
its own directory under `kernel/src/`. The top-level files
and directories are:

- `main.rs` — Entry point, `kernel_main`, panic handler.
- `macros.rs` — `print!` and `println!`.
- `arch.rs` — Linker symbol access.
- `boot/` — Boot protocol and handoff parsing.
- `cpu/` — Processor-level infrastructure.
- `memory/` — Physical and virtual memory.
- `capability/` — The capability fabric.
- `sched/` — Scheduling.
- `console/` — Output.
- `sync/` — Synchronization primitives.
- `tests/` — Test suite.

## Subsystem contents

### `boot/`

Parses the Multiboot2 handoff from the bootloader.

- `mod.rs` — Module root.
- `multiboot2.rs` — Multiboot2 tag parsing. Memory map,
  framebuffer info, and the raw structure copy.
- `info.rs` — Memory map access for the frame allocator.
  Wraps the Multiboot2 parser in the shape the frame
  allocator wants.

### `cpu/`

Manages the processor: descriptor tables, exceptions,
interrupts, and timers.

- `mod.rs` — Module root.
- `control.rs` — Reads and writes CR0, CR2, CR3, CR4.
- `gdt.rs` — Global Descriptor Table. Flat 32-bit
  segments, ring 0.
- `idt.rs` — Interrupt Descriptor Table. Exception and IRQ
  vectors.
- `exception.rs` — Exception dispatch and reporting.
- `exceptions.S` — Exception entry stubs.
- `interrupts.rs` — IRQ dispatch and handler registry.
- `irq.S` — IRQ entry stubs.
- `pic.rs` — 8259 PIC: remap, mask, unmask, EOI.
- `pit.rs` — 8254 PIT: periodic timer on IRQ0.

### `memory/`

Manages physical frames, virtual mappings, and the kernel
heap.

- `mod.rs` — Module root.
- `paging.rs` — Bootstrap identity map for the first 4 MiB,
  CR3 loading, page-table access.
- `direct_map.rs` — Physical memory window at
  `0xc0000000`, plus MMIO mapping at `0x40000000`.
- `frame.rs` — O(1) physical frame allocator with a bitmap
  and a free list.
- `heap.rs` — Kernel heap, capability-mediated through the
  fabric.
- `address_space.rs` — The `AddressSpace` type: page
  directory plus user mappings.
- `object.rs` — The `MemoryObject` type: a bounded region of
  physical frames.

### `capability/`

The capability fabric. The heart of XORG's authority model.

- `mod.rs` — Module root and the global `CapabilityCore`.
- `capability.rs` — `CapabilityId`, `CapabilityRights`, and
  `Capability`.
- `cell.rs` — `CellId` and `Cell`.
- `core.rs` — `CapabilityCore`: the fabric's public API.
- `itable.rs` — The Indirection Table: capability storage.
- `object.rs` — `ObjectId` and `ObjectKind`.
- `registry.rs` — The object registry.

### `sched/`

The scheduler: preemptive, priority-based, with context
switching.

- `mod.rs` — Module root, `schedule_and_switch`, and
  `yield_task`.
- `task.rs` — The `Task` structure and `TaskState`.
- `list.rs` — Intrusive doubly-linked list, parameterized by
  link offsets.
- `scheduler.rs` — The `Scheduler` type: run queues and
  selection policy.
- `context.S` — The `switch_context` assembly routine.

### `console/`

Output. A dispatcher over multiple sinks.

- `mod.rs` — The `Console` dispatcher.
- `font.rs` — PSF1 and PSF2 font parser.
- `framebuffer.rs` — Framebuffer sink.
- `serial.rs` — COM1 sink.
- `vga.rs` — VGA text sink.
- `default.psf` — The embedded font file.

### `sync/`

Primitive synchronization.

- `mod.rs` — Module root.
- `spinlock.rs` — The `SpinLock<T>` type.

### `tests/`

The test suite. One file per subsystem.

- `mod.rs` — Module root and `run_all`.
- `capability.rs` — Fabric tests.
- `memory.rs` — Memory-object and mapping tests.
- `heap.rs` — Kernel heap tests.
- `address_space.rs` — Address-space tests.
- `frame.rs` — Frame allocator tests.
- `kernel_map.rs` — Kernel mapping sharing tests.
- `scheduler.rs` — Scheduler primitive tests.

## Dependency direction

The subsystems form a layered stack. Each layer depends only
on layers below it.

The substrate layers are, from lowest to highest:

- `arch` — No dependencies.
- `boot` — No dependencies.
- `cpu` — Depends on `arch` and `memory::paging`.
- `sync` — No dependencies.
- `memory` — Depends on `cpu`, `boot`, and `capability`.
- `capability` — Depends on `memory`.
- `sched` — Depends on `memory` and `capability`.
- `console` — Depends on `sync` and `boot`.
- `tests` — Depends on everything.

`main.rs` sits at the top. It calls into every subsystem to
boot the kernel.

The dependency from `memory` to `capability` is unusual: the
kernel heap allocates memory objects through the fabric. This
is deliberate. It means the heap itself is capability-mediated
and cannot bypass the fabric's checks.

## Where to look

**How does the kernel boot?** — `main.rs`, then
`docs/boot-sequence.md`.

**How does a capability work?** — `capability/capability.rs`
for the types, `capability/itable.rs` for storage,
`capability/core.rs` for the API.

**How is memory allocated?** — `memory/frame.rs` for
physical frames, `memory/heap.rs` for the kernel heap,
`memory/object.rs` for memory objects in the fabric.

**How does a task switch?** — `sched/context.S` for the
assembly, `sched/scheduler.rs` for the policy, `sched/mod.rs`
for `schedule_and_switch`.

**How does the console print?** — `console/mod.rs` for the
dispatcher, `console/serial.rs`, `console/vga.rs`, and
`console/framebuffer.rs` for the sinks.

**How does the fabric manage objects?** —
`capability/registry.rs` for the registry,
`capability/core.rs` for the allocation API.

## Adding a new subsystem

To add a new subsystem:

1. Create a directory under `kernel/src/`.
2. Add a `mod.rs` that lists its submodules and re-exports
   its public API.
3. Add a `pub mod` line in `kernel/src/main.rs`.
4. Add the subsystem's tests to `kernel/src/tests/` if
   appropriate.

The kernel currently has nine subsystems. Adding one should
not require touching more than four files.