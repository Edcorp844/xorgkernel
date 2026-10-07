# Cell Model

This document describes the design XORG is built toward:
two tiers of execution cells, one capability model, one
fabric.

For the current implementation, see `architecture.md` and
`substrate-contract.md`. For the roadmap, see `README.md`.

## The problem

XORG aims to combine three things that are traditionally in
tension:

1. **Native performance.** Code should run as fast as it can
   on the hardware.
2. **Absolute isolation.** A cell should not be able to
   affect anything outside its capabilities, no matter what
   its code does.
3. **Compatibility.** Unmodified Linux, POSIX, and Android
   binaries should run.

Rings give (2) and (3) but not (1): every syscall traps,
every context switch flushes the TLB. Verified bytecode gives
(1) and (2) but not (3): legacy binaries contain instructions
the verifier rejects. Neither alone gives all three.

## The two-tier model

XORG resolves the tension by splitting execution into two
tiers.

### Tier 1: Native Fabric Cells

Cells that are written for XORG, compiled to a verified
format, and loaded by the fabric.

- **Instruction set.** A verified bytecode or an SFI-checked
  native form. The instruction set does not include
  privileged instructions, raw pointers, or undefined
  behavior.
- **Memory model.** A linear array. Every access is
  bounds-checked by the runtime or masked by the compiler.
- **Authority.** The cell's imports. Each import is a
  capability operation. A cell can only call what it was
  given.
- **Privilege level.** CPL 0, the same level as the fabric.
- **Enforcement.** The bytecode verifier (or the SFI
  compiler) proves that the cell cannot escape its sandbox.
  If the verifier is correct, the enforcement is total.
- **Cost.** No ring transitions. No TLB flushes on cell
  switch. Direct memory access.

The WebAssembly (Wasm) model is the reference for this tier.
Wasm modules execute in the same address space as their host
and cannot execute privileged instructions or access memory
outside their linear memory. Their imports are their
capabilities.

eBPF is an alternative for cells that are small and
structure-specific. Its verifier is smaller and its execution
model is specifically designed for safe in-kernel execution.

A custom bytecode is the simplest option. It is appropriate
if native cells are small and do not need general-purpose
computation.

### Tier 2: Legacy Translation Cells

Cells that run unmodified legacy binaries.

- **Instruction set.** The native machine instructions of
  the binary. No verification is possible; the code contains
  raw pointers, privileged instructions, and system calls.
- **Memory model.** A hardware page table. The kernel
  controls what the cell can reach.
- **Authority.** The cell's capability set, enforced by the
  translation shim on every syscall.
- **Privilege level.** CPL 3. The ring boundary prevents the
  cell from executing privileged instructions.
- **Enforcement.** The MMU (page tables) and the ring
  boundary (CPL 3). The kernel's page tables reflect exactly
  the cell's capability set.
- **Cost.** Ring transitions on every syscall. TLB flush on
  context switch. Compatibility with unmodified binaries.

### The substrate

The substrate is the only code at CPL 0 that is not itself
sandboxed. It includes:

- The capability fabric.
- The scheduler.
- The interrupt and exception handlers.
- The page-table manipulator.
- The bytecode verifier (Tier 1).
- The hardware sandbox manager (Tier 2).
- The syscall dispatcher.

The substrate is trusted. It is small by design. Every line
of code at CPL 0 is a potential escape from the capability
model.

## The unified model

Both tiers are subject to the same policy: **a cell can only
do what its capabilities allow.** The policy is uniform. Only
the enforcement mechanism varies.

A Legacy Translation Cell runs at CPL 3. When it makes a
syscall, control traps into the substrate. The translation
shim receives the syscall, converts it into one or more
capability operations, and forwards those to the fabric. The
fabric verifies the cell's capabilities and performs the
operation, or rejects it. The result travels back through the
shim to the cell, which sees only the syscall's normal return
value. The cell does not know that its syscall was translated
into a capability operation.

A Native Fabric Cell runs at CPL 0, in the same address space
as the fabric. When it calls one of its imports, control
enters the fabric directly, without a ring transition. The
import is bound to a capability that the cell holds. The
fabric verifies the capability and performs the operation. The
cell cannot call an import it was not given, because the
import does not exist in its module: the loader only resolves
imports that the fabric has agreed to provide.

Both paths reach the same fabric. Both are subject to the
same capability checks. Both produce the same observable
result.

## Consequences

**The fabric is the entire security model.** There are not
two models. Capabilities govern both tiers. A cell's
authority is its capability set, regardless of tier.

**The substrate is the trust root.** It is small, audited,
and holds all capabilities by default. It distributes them
through the fabric.

**Compatibility is a shim, not a mode.** Legacy binaries do
not run "in a mode." They run in a Tier 2 cell, and the shim
translates their syscalls. The binary does not know.

**Native cells are not "more trusted" than legacy cells.**
They are more efficient and more expressive, but their
authority is the same: whatever capabilities they hold. A
compromised native cell is as constrained as a compromised
legacy cell.

**The two tiers can share the same objects.** A memory object
created by the fabric can be mapped into a Tier 1 cell's
linear memory and into a Tier 2 cell's address space. Both
cells hold capabilities to it. Both can read and write it
according to their rights. The fabric does not care which
tier a capability holder belongs to.

**IPC between tiers is uniform.** A Tier 1 cell can transfer
a capability to a Tier 2 cell through the fabric. The Tier 2
cell receives a capability. Its syscalls now have authority
over the object. From the fabric's perspective, this is the
same operation as transferring a capability between two Tier
1 cells.

## Open design decisions

**1. What is the native cell format?**

The options are Wasm, eBPF, or a custom bytecode.

- **Wasm** is the most expressive. Its verifier is
  well-understood, its toolchain is mature, and its import
  and linear-memory model maps directly onto capabilities
  and memory objects. The runtime is larger than eBPF's.
- **eBPF** is smaller and simpler. Its verifier is designed
  for in-kernel execution, and its instruction set is
  minimal. It is well-suited to cells that are small and
  structure-specific, less well-suited to full applications.
- **A custom bytecode** is the smallest and simplest. It is
  appropriate if native cells are small and do not need
  general-purpose computation. It limits what native cells
  can do and requires writing a compiler.

The recommendation is Wasm.

**2. How is the fabric exposed to native cells?**

The fabric provides a set of imports. Each import corresponds
to one capability operation: `map_memory`, `unmap_memory`,
`allocate_memory`, `transfer_capability`, `revoke_capability`,
and so on. The loader binds each import to a capability that
the cell has been granted. A cell's imports are its
capabilities.

**3. How is the fabric exposed to legacy cells?**

The fabric is exposed through a syscall interface. The shim
receives POSIX syscalls and translates them to capability
operations. The translation is mechanical for most
operations:

- `open` becomes a lookup in the mount table the shim holds,
  followed by a capability request to the filesystem cell.
- `read` becomes a read operation on the file's memory
  object, subject to the file capability's rights.
- `write` is the same as `read`, with the `WRITE` right
  checked.
- `mmap` becomes a `map_memory` operation on the caller's
  address space.
- `socket` becomes a capability to a network endpoint.
- `pthread_create` becomes a `Task` object created in the
  calling cell's scheduling domain.

**4. What is the substrate's interface to the scheduler?**

The scheduler runs tasks. Tier 1 cells are tasks. Tier 2
cells are tasks. Both are scheduled the same way. The
scheduler does not know or care which tier a task belongs to.

**5. What is the substrate's interface to the console?**

The console is a substrate service. Cells do not have direct
access. A cell that wants to print requests a console
capability and calls `write`. The fabric mediates.

## Path to implementation

The order of work is:

1. **Finish the scheduler.** Exit, block, wake, mutex. These
   are needed for both tiers.

2. **Add hardware mode (Tier 2 support).** TSS, ring-3 GDT
   segments, syscall gate, `iret` transition, per-cell user
   stacks and address spaces. This is the substrate that
   legacy cells will run on.

3. **Build the syscall interface.** Every syscall is a
   capability operation. The interface is uniform across
   tiers.

4. **Run the first user program.** A CPL-3 cell that calls
   the syscall interface and prints "Hello."

5. **Choose and implement the native cell format.** Wasm,
   eBPF, or a custom bytecode.

6. **Build the translation shim.** POSIX-to-capability
   translation for legacy binaries.

7. **Run the first legacy program.** A statically-linked C
   binary that runs unmodified.

After step 7, the architecture is real: two tiers of cells,
one capability model, one fabric.