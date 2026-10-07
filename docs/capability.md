# Capability Fabric

This document describes the design of the capability
fabric: the kernel's authority model, the ITable, the object
registry, and the operations the fabric exposes.

For the code, see `kernel/src/capability/`. For the
invariants the fabric maintains, see `substrate-contract.md`.
For the memory model that the fabric governs, see
`memory.md`. For how the fabric fits into the larger
architecture, see `cell-model.md`.

## Purpose

The fabric is the kernel's single authority model. Every
meaningful resource in the system — memory, address spaces,
devices, IPC channels, files, threads, execution cells — is
represented as an object, and every operation on an object
requires the caller to hold a capability that grants the
necessary rights.

The fabric enforces three properties:

**No ambient authority.** A caller cannot do anything to an
object merely by knowing its name, address, or identifier. It
must present a capability.

**Unforgeable authority.** A capability cannot be constructed
by a caller. It is issued by the fabric and identified by a
handle that cannot be guessed or duplicated without the
fabric's cooperation.

**Revocable authority.** A capability can be invalidated by
the fabric at any time. After revocation, the capability no
longer resolves to the object, even if the caller still holds
its handle.

## Objects

An object is a fabric-managed resource. Every object has:

- An `ObjectId`, assigned by the registry when the object is
  created.
- An `ObjectKind`, which tells the fabric what the object is.
- A lifetime. The fabric tracks whether the object is alive;
  when it is destroyed, its resources are released.
- Zero or more capabilities referring to it.

The current kinds are:

- `MemoryObject` — a bounded region of physical frames.
- `AddressSpace` — a page directory plus user mappings.
- `Cell` — an execution cell, a grouping of capabilities.

Future kinds will include channels, files, devices, threads,
timers, and anything else the system exposes.

The registry is the fabric's source of truth for what exists.
Every lookup of an object ID goes through the registry, and
the registry returns the object's kind along with its ID.

## The ITable

The ITable — Indirection Table — is the fabric's capability
store. Every capability that exists is a slot in the ITable,
and every lookup of a `CapabilityId` goes through it.

The ITable is a fixed-size array of slots. Each slot records:

- Whether it is occupied.
- A generation counter.
- The capability stored in it, if any.

A `CapabilityId` is a slot index combined with a generation
counter. The generation is incremented every time the slot is
freed and reused, so a stale ID from before the reuse is
rejected even though it names the same slot.

This generation scheme is what makes revocation sound. When a
capability is revoked, its slot is pushed onto the free list
and its generation is incremented. If a new capability is
later allocated into the same slot, it has a different
generation, and the old `CapabilityId` no longer resolves.

The ITable's allocation path is O(1). A free list of
unoccupied slot indices threads through the slots themselves.
Allocation pops the head; revocation pushes onto the head.

The ITable's size is fixed at 1024 slots. This is a bootstrap
constraint. When the ITable needs to grow, it will become a
dynamically allocated structure, probably a chain of
fixed-size chunks.

## Capabilities

A capability carries three things:

- Its own `CapabilityId`.
- The `ObjectId` of the object it names.
- A set of `CapabilityRights`.

The rights are a bitmask. Which bits are meaningful depends
on the kind of object the capability names. The fabric
enforces this: a capability with `EXECUTE` on a cell is
rejected, because `EXECUTE` has no meaning for a cell.

The rights are:

- `MAP` — install the object into an address space.
- `READ` — read the object's contents.
- `WRITE` — modify the object's contents.
- `EXECUTE` — use the object's contents as instructions.
- `SHARE` — derive a new capability to the same object with a
  subset of rights.
- `DESTROY` — destroy the object.
- `UNMAP` — remove a mapping from an address space.
- `ACTIVATE` — load an address space into CR3.
- `ENTER` — transition execution into a cell.
- `GRANT` — add a capability to a cell's capability space.
- `REVOKE` — remove a capability from a cell.

The rights a capability carries are not enforced by the type
system. They are enforced by the fabric: every operation that
takes a capability checks that the capability carries the
rights the operation requires. If it does not, the operation
returns an error.

## Delegation

A capability with `SHARE` can be used to produce a new
capability with the same object and a subset of rights. This
is the only way new capabilities come into existence.

The delegation operation is `transfer`. It takes a source
capability and a requested set of rights. The source must
carry `SHARE`, and the requested rights must be a subset of
the source's rights. If both conditions hold, the fabric
allocates a new capability with the requested rights. If
either fails, `transfer` returns `None`.

Because delegation can only shrink the rights of a
capability, never grow them, the invariant is that **rights
are monotonically non-increasing along any chain of
delegations**. This is what makes revocation tractable: the
fabric never needs to consult the object to decide whether a
delegation is legal.

The right `SHARE` is the right to delegate. A capability
without `SHARE` is a terminal grant: it can be used, but it
cannot be passed on. This is what makes authority containment
possible. A cell that holds a capability without `SHARE` can
use the capability but cannot give it to another cell.

## Revocation

Revocation is a first-class operation. The fabric can
invalidate a capability at any time. After revocation, the
capability no longer resolves to the object, even if the
caller still holds its handle.

The fabric has two revocation operations:

**`revoke(cap)`** revokes a single capability. Its slot is
pushed onto the ITable's free list, its generation is
incremented, and any subsequent lookup of its ID fails.

**`revoke_object(id)`** revokes every capability referring to
an object. It scans the ITable and revokes every slot whose
capability names the object. It is O(n) in the size of the
ITable. A future improvement is to maintain a per-object list
of capabilities, making it O(k) in the number of capabilities
to the object.

`destroy_object` combines revocation with destruction: it
revokes every capability referring to the object, then
releases the object's resources. For a memory object, the
frames are returned to the frame allocator. For an address
space, the page directory is freed. After `destroy_object`,
no capability resolves to the object, and the object no
longer exists.

## Object lifetime

An object's lifetime is bounded by the capabilities that
refer to it. When the last capability to an object is
revoked, the object has no remaining authority holders.

The fabric does not automatically destroy an object when its
last capability is revoked. Destruction is an explicit
operation. This is deliberate: an object may be revoked
temporarily, or the caller may wish to observe the revocation
before destroying.

When an object is destroyed, its resources are released:

- A `MemoryObject` returns its frames to the frame allocator.
- An `AddressSpace` frees its page directory.
- A `Cell` releases its capabilities.

The fabric calls the appropriate destructor through the
object's `Drop` implementation. Rust's `Drop` guarantee is
what ensures resources are not leaked.

## The fabric's API

The fabric exposes these operations on `CapabilityCore`:

**Object lifecycle.**

- `create_object(kind)` — create a new object of the given
  kind. Returns the object's ID.
- `lookup_object(id)` — check that an object exists. Returns
  its ID if so.
- `lookup_object_kind(id)` — return an object's kind.
- `destroy_object(id)` — revoke every capability referring to
  the object, release its resources, and unregister it.

**Capability lifecycle.**

- `allocate(object, rights)` — create a capability for an
  existing object with the given rights.
- `lookup(cap)` — return a reference to the capability, or
  `None` if it has been revoked.
- `revoke(cap)` — revoke a single capability.
- `revoke_object(id)` — revoke every capability referring to
  an object.
- `has_rights(cap, required)` — check whether a capability
  contains the requested rights.
- `object(cap)` — return the object a capability names.
- `transfer(source, rights)` — delegate from a source
  capability with attenuated rights.

**Memory objects.**

- `allocate_memory(pages, rights)` — allocate a memory object
  with the given number of frames and return a capability to
  it.
- `memory_object(cap)` — return a reference to the memory
  object a capability names.

**Address spaces.**

- `allocate_address_space(rights)` — allocate an address
  space and return a capability to it.
- `address_space(cap)` — return a reference to the address
  space a capability names.
- `register_kernel_address_space(rights)` — wrap the kernel's
  own page directory as an address space and return a
  capability to it. Called once at boot.
- `map_memory(as_cap, mo_cap, va, writable, user)` — install
  a memory object's frames into an address space.
- `unmap_memory(as_cap, va)` — remove a mapping.

**Execution cells.**

- `create_cell()` — create a new execution cell.
- `grant_capability(cell, cap)` — add a capability to a
  cell's capability space.
- `cell_has_capability(cell, cap)` — check whether a cell
  holds a capability.
- `transfer_between_cells(source, target, cap, rights)` —
  delegate a capability from one cell to another.

## Design decisions

**Why an ITable instead of raw pointers?** A raw pointer to an
object would expose its address. An attacker who learned the
address could reach the object without going through the
fabric. The ITable adds an indirection: the capability names a
slot, the slot names the object. An attacker must know both
the slot index and the generation to reach the object, and
the generation changes on every reuse.

**Why generations?** Without generations, a revoked
capability's ID could be reused. A caller holding the old ID
would unknowingly resolve to a new object that happens to
occupy the same slot. Generations close this hole. A stale ID
has the wrong generation and is rejected.

**Why explicit revocation?** Automatic revocation on last
reference is tempting, but it makes the semantics of
`revoke(cap)` unclear: does it drop a reference, or does it
invalidate the capability regardless of other holders? The
fabric chooses the latter: `revoke(cap)` invalidates one
capability. To invalidate every capability to an object, use
`revoke_object`. To destroy the object as well, use
`destroy_object`.

**Why are rights not enforced by the type system?** A
type-system enforcement would require one capability type per
object kind, with different rights variants per kind. This
would complicate the API and the ITable, which store
capabilities uniformly. The fabric enforces rights at every
operation instead. This is the same choice seL4 and other
production capability kernels make.

**Why eight rights?** The rights are chosen to cover the
operations the fabric exposes today. As new operations are
added — for example, `USE` for device access, `SEND` for
channels — new rights will be added. There is no penalty for
having many rights, since they are a single bitmask.

## What is not yet implemented

- **Per-object capability lists.** `revoke_object` is O(n) in
  the ITable size. A per-object list would make it O(k).
- **Dynamic ITable growth.** The ITable is fixed at 1024
  slots.
- **Dynamic registry growth.** The registry is fixed at 1024
  slots.
- **Epoch-based reclamation.** Revoked slots are reused
  immediately. In a concurrent system, a slot in use by
  another CPU could be reissued before the other CPU has
  finished. This is not a problem on a single CPU.
- **Delegation trees.** The fabric does not track the parent
  of a capability. This means it cannot revoke the transitive
  closure of a delegation. Revocation is direct: revoking a
  capability invalidates that capability only, not the
  capabilities derived from it. Delegation trees would be
  needed for hierarchical revocation.
- **Per-capability resource accounting.** The fabric does not
  track how much memory, CPU time, or other resource a
  capability holder has consumed. This is required for quotas
  and will be added when quotas are.
- **Capability spaces as objects.** A cell's capability space
  is currently a fixed-size array inside the cell. In a
  capability-native design, the capability space itself is an
  object with its own capabilities. This is a larger
  refactor.

These will be added as the architecture grows. The current
fabric is the minimum needed to run the kernel, manage
memory, and pass authority between cells.