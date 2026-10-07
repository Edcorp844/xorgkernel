# Boot Sequence

This document traces the path from firmware to the first
scheduled task. It describes what happens at each stage, which
files are involved, and what state is established.

For the code structure, see `architecture.md`. For the
invariants each subsystem maintains, see
`substrate-contract.md`. For the plan to run user code, see
`cell-model.md`.

## Stage 1: Firmware

The machine starts in real mode. The BIOS or UEFI firmware
runs its own initialization and eventually loads a bootloader
from the disk or ISO.

## Stage 2: GRUB

GRUB reads `grub.cfg` from the ISO, finds the kernel ELF,
parses its Multiboot2 header, loads the kernel's PT_LOAD
segments to their physical addresses, and jumps to the entry
point.

The kernel is linked at `0x100000`. GRUB also sets up a
linear framebuffer if the Multiboot2 header requests one, and
passes a pointer to the Multiboot2 information structure in
EBX.

At the moment GRUB jumps to the kernel:

- The CPU is in 32-bit protected mode.
- Paging is disabled.
- CS is the bootloader's code segment.
- DS, ES, SS are the bootloader's data segments.
- Interrupts are disabled.
- The stack is whatever GRUB set up.

The kernel is loaded at its link address, so every symbol in
the kernel image refers to the correct physical address once
the image is in memory.

## Stage 3: Kernel entry

The kernel's entry point `_start` is a small assembly stub in
`boot/grub_header.S`. It:

1. Sets up a stack using `__bootstrap_stack_top`, a
   linker-defined symbol.
2. Pushes the Multiboot2 info pointer (received in EBX) as
   an argument for the Rust entry point.
3. Calls `kernel_main`.

From this point, execution is in Rust.

## Stage 4: kernel_main

`kernel_main` in `main.rs` performs the boot sequence. The
steps are ordered by dependency: each step requires the
results of the steps above it.

**Step 1: Clear bootstrap regions.**

Call `clear_bootstrap_regions()`. This function zeros the
kernel's NOLOAD regions and `.bss`, using the linker symbols
`__bootstrap_start` and `__kernel_end`.

This must happen before any other code reads from those
regions, because they are not part of the disk image that the
bootloader loads.

**Step 2: Copy the Multiboot2 structure.**

Call `boot::multiboot2::init(multiboot_info)`. This copies
the bootloader's information structure into a static buffer
in the kernel's `.bss`.

The copy is necessary because GRUB typically places the
structure above the kernel's identity map. Once paging is
enabled, that memory becomes unreachable. Copying the
structure into the kernel's own memory makes it readable for
the rest of the boot.

**Step 3: Initialize the console.**

Call `console::init()`. This sets up the serial port and the
VGA text sink. From this point on, `println!` produces
output.

**Step 4: Load the GDT.**

Call `cpu::gdt::init()`. This loads the kernel's flat GDT and
reloads CS, DS, ES, FS, GS, SS.

The CS reload requires a far return, because `lgdt` alone
does not update CS's cached descriptor. Without the reload,
the CPU would continue with the bootloader's code segment
descriptor, and the first interrupt would fault because the
selector does not match the current GDT.

**Step 5: Load the IDT.**

Call `cpu::idt::init()`. This installs exception and IRQ
stubs in the Interrupt Descriptor Table.

**Step 6: Enable paging.**

Call `memory::paging::init()`. This builds the identity map
for the first 4 MiB, loads the kernel page directory's
physical address into CR3, and enables CR0.PG.

From this point on, the kernel runs on virtual addresses. The
identity map ensures that low addresses continue to translate
to themselves, so kernel code and data remain reachable.

**Step 7: Install the direct map.**

Call `memory::direct_map::init()`. This maps physical memory
0–128 MiB at virtual address `0xc0000000`, using 4 MiB pages
and the PSE feature.

The direct map gives the kernel a stable virtual window on
physical memory, which the frame allocator and page-table
manipulator use.

**Step 8: Register the framebuffer.**

Call `setup_framebuffer()`. This function:

1. Asks the Multiboot2 parser for the framebuffer tag.
2. If present, maps the framebuffer's physical range at
   `0x40000000` using `memory::direct_map::map_mmio`. The
   mapping uses PCD (page cache disable) because the
   framebuffer is MMIO.
3. Constructs a `Framebuffer` sink and registers it with the
   console.

From this point on, `println!` produces output on the
framebuffer as well as on serial and VGA text.

**Step 9: Initialize the frame allocator.**

Call `memory::frame::init()`. This:

1. Reads the Multiboot2 memory map.
2. Marks every usable region's frames as free in the bitmap.
3. Reserves the kernel image, the page directory, the page
   table, the IDT, the GDT, the frame bitmap, the bootstrap
   stack, and the first 64 KiB of physical memory.
4. Builds the free list from the remaining free frames.

**Step 10: Initialize the capability fabric.**

Call `capability::init()`. This prepares the ITable, object
registry, memory-object table, and address-space table. No
objects exist yet.

**Step 11: Register the kernel address space.**

Call
`capability::core_mut().register_kernel_address_space(...)`.
This wraps the kernel's own page directory as a fabric-managed
address space and returns a capability to it.

The kernel now holds a capability, with `MAP | UNMAP |
SHARE`, to its own address space.

**Step 12: Initialize the kernel heap.**

Call `memory::heap::init(kernel_as_cap)`. The heap receives
the kernel address-space capability. When the heap grows, it
uses this capability to install new regions through the
fabric.

From this point on, `Box`, `Vec`, and every other `alloc`
type work.

**Step 13: Run the test suite.**

Call `tests::run_all()`. Every subsystem is exercised. The
tests run with interrupts disabled, because they are not
written to tolerate concurrent timer ticks.

**Step 14: Create the initial tasks.**

Three tasks are created via `sched::scheduler_mut().create`:

- The idle task, priority 0. Its body loops in `hlt`.
- Test task A, priority 4. It prints and yields.
- Test task B, priority 4. It prints and yields.

Each task receives a kernel stack allocated from the heap and
is placed on its priority's run queue.

**Step 15: Initialize the scheduler.**

Call `sched::init()`. This prints the scheduler's
configuration.

**Step 16: Set up interrupts.**

Call `setup_interrupts()`. This:

1. Remaps the PIC so IRQ0-15 arrive at vectors 0x20-0x2F.
2. Registers the timer handler for IRQ0.
3. Programs the PIT to fire IRQ0 at 100 Hz.
4. Unmasks IRQ0.
5. Executes `sti`.

From this point on, the timer fires every 10 milliseconds.

**Step 17: Yield to the scheduler.**

Call `sched::schedule_and_switch()`. This:

1. Captures the current task ID, which is 0 (the kernel is
   running directly, not as a task).
2. Does not re-enqueue the current "task" because 0 is the
   bootstrap context.
3. Selects the highest-priority ready task.
4. Updates the scheduler's `current` field.
5. Calls `switch_context(bootstrap, selected)`.

From this point on, `kernel_main` never runs again. The
kernel runs as one of the tasks it created.

## Stage 5: The first task

When `switch_context(from, to)` runs, it performs a register
save and stack switch:

1. It saves the current task's callee-saved registers (EBP,
   EDI, ESI, EBX) on the current stack.
2. It stores the resulting ESP into `from->esp`.
3. It loads `to->esp` into ESP.
4. It pops the callee-saved registers from the new stack.
5. It executes `ret`.

For the first switch, `from` is the bootstrap task and `to`
is the first scheduled task. The bootstrap task's `esp` field
is overwritten with the address of the current stack frame.
If the bootstrap context were ever restored, `kernel_main`
would resume from the switch call. It never is.

The first scheduled task's stack was laid out by
`Task::create`. Its top contains four zeroed callee-saved
registers, followed by the task's entry point address.

After the four pops, ESP points at the entry point address.
The `ret` instruction pops it and jumps there. The task
begins executing with a fresh stack.

## Stage 6: Preemption

The timer fires every 10 milliseconds. The CPU pushes an
interrupt frame (EIP, CS, EFLAGS) onto the current task's
kernel stack, then jumps to the IRQ stub for vector 32.

The IRQ stub in `cpu/irq.S`:

1. Saves all general-purpose registers (`pushal`).
2. Pushes the IRQ number (0).
3. Calls `irq_dispatch`.
4. Removes the IRQ number from the stack.
5. Restores the general-purpose registers (`popal`).
6. Returns with `iret`.

`irq_dispatch` in `cpu/interrupts.rs`:

1. Sends an end-of-interrupt to the PIC before running the
   handler. This ordering matters: it lets the PIC deliver
   the same IRQ again as soon as interrupts are re-enabled,
   which is required for preemption.
2. Looks up the registered handler for IRQ 0.
3. Calls it.

`on_timer_tick` in `main.rs`:

1. Increments the tick counter.
2. Prints the counter once per second.
3. Calls `sched::schedule_and_switch()`.

`schedule_and_switch` in `sched/mod.rs`:

1. Captures the current task ID.
2. Re-enqueues the current task on its priority's run queue.
   The task will be picked again on a future tick.
3. Selects the highest-priority ready task.
4. If the selected task is different from the current one,
   calls `switch_context`.

The interrupted task is saved on its own stack, at the
boundary of the interrupt handler. When it is later resumed,
`switch_context` loads its `esp` and returns into the middle
of `irq_dispatch`. The IRQ stub finishes, `iret` restores the
interrupted context, and the task continues from where it was
preempted.

This is the complete cycle. Every 10 milliseconds, the
scheduler has a chance to switch to a different task. If no
other task is ready, the current task resumes.