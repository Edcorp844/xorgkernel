# XORG (OCAP Fabric OS)

A clean-sheet, high-performance operating system architecture built around **affine object ownership, unforgeable capabilities, object-based resources, and a capability-routing fabric**.

XORG (OCAP Fabric OS) is designed to combine the performance characteristics traditionally associated with monolithic systems with the isolation and composability associated with microkernels and capability-based operating systems.

The fundamental abstraction is neither the POSIX process nor the raw pointer.

It is the **capability-bearing object**.

---

# Architectural Overview

XORG (OCAP Fabric OS) replaces the traditional model of:

```text
Process
   │
   ├── raw pointers
   ├── global paths
   ├── shared kernel objects
   └── unrestricted system calls
```

with:

```text
Execution Cell
      │
      │ capabilities
      ▼
Capability Fabric
      │
      ▼
Object
```

Every meaningful system resource is represented as an object whose use is governed by explicit authority.

Examples include:

* memory objects
* address spaces
* threads
* processes / execution cells
* IPC channels
* files
* directories
* devices
* DMA buffers
* GPU surfaces
* network endpoints
* timers
* synchronization primitives

The architecture does not attempt to eliminate hardware mechanisms such as paging, privilege levels, or the MMU. Instead, those mechanisms become **substrate enforcement mechanisms beneath the capability model**.

The application-visible architecture is therefore object- and capability-oriented rather than page-table- and pointer-oriented.

---

# System Model

```text
                    ┌──────────────────────────────┐
                    │          Applications        │
                    │                              │
                    │ GTK / Qt / CLI / Android /   │
                    │ Native Fabric Applications   │
                    └───────────────┬──────────────┘
                                    │
                                    │
                         Compatibility APIs
                                    │
                    ┌───────────────▼──────────────┐
                    │     Translation / ABI Layer  │
                    │                              │
                    │ POSIX · Linux · Android ·    │
                    │ Legacy ABI · Native ABI      │
                    └───────────────┬──────────────┘
                                    │
                                    │
                    ┌───────────────▼───────────────┐
                    │       Capability Fabric       |
                    │                               │
                    │  ITable                       │
                    │  Capability Router            │
                    │  Object Lifetimes             │
                    │  IPC / Channels               │
                    │  Revocation                   │
                    │  Resource Accounting          │
                    │  Authority Transfer           │
                    └───────────────┬───────────────┘
                                    │
              ┌─────────────────────┼─────────────────────┐
              │                     │                     │
       ┌──────▼──────┐       ┌──────▼──────┐       ┌──────▼──────┐
       │ Memory      │       │ Drivers     │       │ Storage     │
       │ Fabric      │       │ Cells       │       │ Object FS   │
       │             │       │             │       │             │
       │ Frames      │       │ PCI         │       │ Objects     │
       │ Pages       │       │ USB         │       │ CoW         │
       │ Address     │       │ GPU         │       │ Journaling  │
       │ Spaces      │       │ Network     │       │ Snapshots   │
       └──────┬──────┘       └──────┬──────┘       └──────┬──────┘
              │                     │                     │
              └─────────────────────┼─────────────────────┘
                                    │
                         ┌──────────▼──────────┐
                         │ Hardware Substrate  │
                         │                     │
                         │ CPU / MMU / DMA /   │
                         │ PCI / USB / GPU /   │
                         │ Timers / Interrupts │
                         └─────────────────────┘
```

---

# Architectural Layers

## 1. Hardware Substrate

The substrate is the lowest layer of the system.

It is responsible for translating the abstract capability architecture onto real hardware.

Responsibilities include:

* CPU initialization
* interrupt and exception handling
* context switching
* physical memory discovery
* page-table management
* MMU configuration
* I/O privilege enforcement
* DMA/IOMMU management
* hardware discovery
* timer infrastructure
* architecture-specific boot code

The substrate is intentionally small in conceptual scope.

Hardware mechanisms still exist.

The difference is that higher layers should not expose those mechanisms directly as the system's primary security model.

For example, an application does not receive:

```text
physical_address = 0x12345000
```

as authority.

Instead it receives a capability referring to a memory object.

---

# 2. Capability Fabric

The **Capability Fabric** is the architectural heart of XORG (OCAP Fabric OS).

It provides the mechanisms through which execution cells obtain, transfer, restrict, and revoke authority over system objects.

The fabric provides:

* capability creation
* capability lookup
* capability transfer
* capability duplication
* capability restriction
* capability revocation
* object lifetime management
* IPC
* resource accounting
* ownership tracking
* synchronization
* object discovery

The fabric separates:

```text
Object
```

from:

```text
Authority to access the object
```

A process does not own an object merely because it knows an address.

It must possess an appropriate capability.

---

# 3. The ITable

The **ITable — Indirection Table** — is the primary mechanism through which capabilities are resolved into objects.

Conceptually:

```text
Capability
┌───────────────────────┐
│ Slot                  │
│ Generation            │
│ Rights                │
└───────────┬───────────┘
            │
            ▼
        ITable
┌───────────────────────┐
│ Object reference      │
│ Generation            │
│ Metadata              │
│ Lifetime state        │
└───────────┬───────────┘
            │
            ▼
          Object
```

A capability therefore does not directly expose a raw object pointer.

This provides several important properties.

### Revocation

A capability can be invalidated without searching every process for copies of a raw pointer.

```text
Capability
     │
     ▼
ITable entry
     │
     X
   revoked
```

### Generation protection

Recycled slots can use generation counters to prevent stale capabilities from accidentally referring to newly allocated objects.

```text
slot = 42
generation = 17
```

is different from:

```text
slot = 42
generation = 18
```

even though the slot itself is reused.

### Authority separation

Multiple capabilities can reference the same object while carrying different rights.

For example:

```text
Capability A
    READ

Capability B
    READ | WRITE

Capability C
    READ | EXECUTE
```

The object remains the same.

The authority is different.

---

# 4. Affine Object Ownership

XORG (OCAP Fabric OS) adopts an affine ownership model.

An object or capability may be:

* moved
* borrowed for restricted access
* shared with controlled rights
* revoked
* destroyed

A move transfers authority.

Conceptually:

```text
Before:

Cell A ───────► Object X


Move:


Cell A ──X
          \
           ▼
        Fabric
           │
           ▼
Cell B ───────► Object X
```

Cell A no longer possesses the moved authority.

This provides a foundation for zero-copy communication.

Instead of:

```text
A
 │
 │ copy 4 MiB
 ▼
B
```

the system can perform:

```text
A
 │
 │ transfer capability
 ▼
B
```

The underlying memory object does not need to move.

Only authority changes hands.

---

# 5. Borrowed Access

Not every interaction should require ownership transfer.

Read-only resources can be shared.

For example:

```text
             ┌─────────────┐
             │ Buffer      │
             └──────┬──────┘
                    │
             ┌──────┴──────┐
             │             │
          READ           READ
             │             │
             ▼             ▼
          Cell A          Cell B
```

Write authority remains exclusive or otherwise explicitly controlled.

This allows the system to express patterns similar to:

```text
immutable shared access
```

and:

```text
exclusive mutable access
```

without requiring every communication path to copy memory.

---

# 6. Object Model

The kernel and system services expose resources as objects.

A conceptual object hierarchy is:

```text
Object
│
├── MemoryObject
│
├── AddressSpace
│
├── Thread
│
├── ExecutionCell
│
├── Channel
│
├── File
│
├── Directory
│
├── Device
│
├── DMABuffer
│
├── GPUSurface
│
├── Socket
│
├── Timer
│
└── SynchronizationObject
```

Each object has:

```text
identity
lifetime
authority rules
resource accounting
capability representation
```

This creates a uniform system model.

A file is an object.

A GPU surface is an object.

A thread is an object.

A memory region is an object.

The capability system provides the authority required to operate on each one.

---

# 7. Execution Cells

XORG (OCAP Fabric OS) uses **execution cells** as the fundamental unit of isolated computation.

A cell contains some combination of:

```text
Execution context
Address-space authority
Capability space
Scheduling authority
Resource quotas
IPC endpoints
```

A cell does not automatically possess unrestricted access to the machine.

Its capabilities define what it can interact with.

For example:

```text
Cell: media-player

Capabilities:

READ   → music.db
READ   → song-buffer
WRITE  → audio-output
USE    → GPU-device
SEND   → audio-server
```

The media player therefore has no inherent authority over:

```text
filesystem root
network devices
other applications
raw PCI
arbitrary physical memory
```

unless such authority is explicitly granted.

---

# 8. Driver Cells

Hardware drivers are isolated into driver cells wherever practical.

Instead of:

```text
Application
     │
     ▼
Huge privileged kernel
     │
     ▼
Hardware
```

the architecture favors:

```text
Application
     │
     ▼
Capability
     │
     ▼
Driver Cell
     │
     ▼
Hardware
```

Drivers receive only the capabilities required for their hardware.

For example:

```text
USB Driver Cell

CAPABILITY:
    PCI device
    MMIO region
    interrupt
    DMA pool
    USB controller
```

A compromised driver therefore does not automatically receive authority over the entire machine.

---

# 9. Memory Fabric

The physical frame allocator is a substrate component rather than the final memory abstraction.

The intended hierarchy is:

```text
Physical Frames
       │
       ▼
Memory Objects
       │
       ▼
Address Spaces
       │
       ▼
Capabilities
       │
       ▼
Execution Cells
```

This allows the system to separate:

```text
physical ownership
```

from:

```text
virtual mapping
```

and:

```text
application authority
```

A memory object may therefore be:

* mapped into one address space
* shared read-only with another
* transferred to another cell
* revoked
* used as a DMA buffer
* attached to a GPU surface

without turning physical addresses into application-level authority.

---

# 10. Zero-Copy IPC

One of the major goals of the architecture is efficient IPC without unnecessary memory copying.

Traditional communication:

```text
Producer
   │
   │ copy
   ▼
Kernel buffer
   │
   │ copy
   ▼
Consumer
```

Capability-based transfer:

```text
Producer
   │
   │ capability transfer
   ▼
Capability Fabric
   │
   ▼
Consumer
```

The underlying object can remain physically where it is.

Only its ownership or access authority changes.

This is particularly important for:

* video
* audio
* networking
* GPU rendering
* storage
* large IPC messages
* shared datasets

---

# 11. Translation and Transmutation Layer

XORG (OCAP Fabric OS) is not intended to require the entire existing software ecosystem to be rewritten.

A compatibility layer translates existing operating-system abstractions into the capability fabric.

Conceptually:

```text
Linux / POSIX / Android
          │
          ▼
Translation Layer
          │
          ▼
Capability Fabric
          │
          ▼
Native Objects
```

Examples:

```text
POSIX open()
        ↓
File capability

POSIX mmap()
        ↓
Memory object + mapping capability

POSIX socket()
        ↓
Channel / network endpoint

pthread_create()
        ↓
Thread object

Android Binder
        ↓
Capability-based IPC
```

The compatibility layer is therefore an adapter between legacy semantics and the native architecture.

---

# 12. POSIX-to-Object Storage

Traditional Unix systems expose storage primarily through hierarchical paths:

```text
/etc/config
/home/user/file
/dev/device
```

XORG (OCAP Fabric OS) uses an object-oriented storage model internally.

The filesystem becomes an object graph.

Conceptually:

```text
Root Object
    │
    ├── Directory Object
    │       │
    │       ├── File Object
    │       ├── File Object
    │       └── Directory Object
    │
    └── Device Object
```

POSIX paths are resolved by the compatibility layer.

Native applications can eventually operate directly on storage capabilities.

This permits:

* copy-on-write
* snapshots
* object-level permissions
* capability-based access
* transactional updates
* journaling
* efficient sharing

---

# 13. Hardware and DMA Containment

Hardware is inherently stateful and frequently asynchronous.

XORG (OCAP Fabric OS) therefore treats hardware access as an explicitly managed capability.

DMA buffers are represented as objects.

A driver does not receive unrestricted access to physical memory.

Instead:

```text
Driver Cell
     │
     │ DMA capability
     ▼
DMA Buffer
     │
     ▼
IOMMU / DMA subsystem
     │
     ▼
Hardware
```

This creates a controlled boundary between:

```text
hardware DMA
```

and:

```text
system memory
```

where hardware support permits it.

---

# 14. Transactional Hardware State

Some hardware devices cannot be treated as simple stateless resources.

Devices such as:

* USB controllers
* GPUs
* storage controllers
* network interfaces
* PCI devices

may contain complex state.

The architecture therefore allows hardware operations to be modeled as transactions.

Conceptually:

```text
Operation
    │
    ▼
Prepare
    │
    ▼
Commit
    │
    ├── success → new state
    │
    └── failure → recovery/reset
```

Hardware reset and recovery can therefore be represented through explicit state transitions rather than scattered driver-specific recovery logic.

---

# 15. Unified Rendering Architecture

Applications do not need direct access to display hardware.

Rendering is separated into:

```text
Application
     │
     ▼
Render Object / Surface
     │
     ▼
Compositor
     │
     ▼
GPU
     │
     ▼
Display
```

Applications may render into capability-controlled surfaces.

The compositor owns presentation authority.

This provides a foundation for:

* GPU isolation
* zero-copy rendering
* Wayland-style surfaces
* remote rendering
* multiple UI frameworks
* sandboxed applications
* GPU resource accounting

GTK, Qt, Android and native Fabric applications can therefore converge on a common presentation infrastructure.

---

# 16. Server-Side UI Transmutation

The architecture permits a higher-level UI compatibility layer in which traditional toolkit semantics are translated into the system compositor.

Conceptually:

```text
GTK Application
       │
       ▼
GTK Compatibility Layer
       │
       ▼
UI Objects / Surfaces
       │
       ▼
System Compositor
       │
       ▼
GPU
```

The compositor controls:

* window placement
* composition
* scaling
* display configuration
* input routing
* accessibility integration
* presentation synchronization

The goal is not necessarily to reproduce every toolkit internally, but to provide a common rendering and interaction substrate.

---

# 17. Resource Accounting

Capabilities are also accounting boundaries.

Every execution cell can have explicit resource limits:

```text
CPU time
Memory
GPU memory
DMA buffers
IPC bandwidth
Storage
Network resources
```

For example:

```text
Application A

Memory:       512 MiB
GPU memory:   256 MiB
CPU quota:    20%
DMA buffers:  32
```

The capability fabric can associate resources with their owning cells and enforce quotas independently of application-level APIs.

---

# 18. Revocation

Revocation is a first-class operation.

Example:

```text
Application
    │
    └── GPU capability
```

The system may revoke that capability:

```text
Application
    │
    X
    │
 GPU capability revoked
```

without destroying unrelated resources.

This becomes particularly valuable for:

* sandboxing
* device removal
* process termination
* session logout
* security policy changes
* driver recovery
* resource reclamation

---

# 19. Epoch-Based Lifetimes

Capabilities and objects may use generation and epoch mechanisms to prevent stale references.

Conceptually:

```text
Object generation 41
       │
       ▼
Capability generation 41
```

After destruction:

```text
Object destroyed
       │
       ▼
Generation 42
```

An old capability referencing generation 41 is rejected.

Epoch-based reclamation can additionally delay physical object reclamation until outstanding readers have left the relevant epoch.

This separates:

```text
logical revocation
```

from:

```text
physical reclamation
```

which is important for high-performance concurrent systems.

---

# 20. Security Model

The security model is capability-oriented rather than path-oriented.

Possessing a name is not sufficient.

Authority must be explicitly granted.

The security principle is:

> **No capability, no authority.**

For example, knowing that a GPU exists does not grant access to it.

Knowing a file's name does not grant access to it.

Knowing a physical address does not grant ownership of that memory.

Knowing another process exists does not grant permission to interact with it.

Authority must arrive through an explicit capability.

---

# 21. Native vs Compatibility World

XORG (OCAP Fabric OS) intentionally maintains two conceptual worlds.

### Native Fabric

```text
Native application
       │
       ▼
Capability API
       │
       ▼
Capability Fabric
       │
       ▼
Native objects
```

This world is designed specifically around the architecture.

### Compatibility World

```text
Linux / POSIX / Android application
              │
              ▼
       Compatibility layer
              │
              ▼
       Capability translation
              │
              ▼
       Native Fabric objects
```

This allows the operating system to evolve independently from legacy operating-system abstractions.

---

# 22. Boot Architecture

The boot process is divided into clear stages.

```text
Firmware
   │
   ▼
Stage 1 Bootloader
   │
   ├── CPU initialization
   ├── disk access
   ├── kernel loading
   └── protected-mode transition
   │
   ▼
Rust Kernel Entry
   │
   ├── BSS initialization
   ├── IDT
   ├── physical memory discovery
   ├── paging
   └── physical frame allocator
   │
   ▼
Capability Substrate
   │
   ├── object manager
   ├── ITable
   ├── capability spaces
   ├── IPC
   └── execution cells
   │
   ▼
System Services
   │
   ├── storage
   ├── networking
   ├── drivers
   ├── compositor
   └── compatibility layers
   │
   ▼
Applications
```

The current implementation is at the early substrate stage.

The existing bootloader, paging subsystem, memory map discovery, IDT and physical frame allocator form the foundation upon which the capability fabric will be built.

---

# 23. Initial Implementation Roadmap

## Phase 0 — Hardware Substrate

* [x] Bootloader
* [x] Rust kernel entry
* [x] Physical memory discovery
* [x] Paging
* [x] Page tables
* [x] Physical frame allocator
* [x] IDT initialization
* [ ] Interrupt handling
* [ ] Scheduler primitives
* [ ] Context switching

## Phase 1 — Capability Core

* [ ] Object abstraction
* [ ] Capability representation
* [ ] Capability spaces
* [ ] ITable
* [ ] Generation counters
* [ ] Capability lookup
* [ ] Capability transfer
* [ ] Capability restriction
* [ ] Capability revocation
* [ ] Object lifetime management

## Phase 2 — Execution Fabric

* [ ] Execution cells
* [ ] Thread objects
* [ ] Address-space objects
* [ ] Scheduling domains
* [ ] IPC channels
* [ ] Shared-memory objects
* [ ] Zero-copy IPC
* [ ] Resource quotas

## Phase 3 — Device Fabric

* [ ] PCI subsystem
* [ ] Interrupt objects
* [ ] DMA objects
* [ ] IOMMU support
* [ ] Driver cells
* [ ] USB
* [ ] storage controllers
* [ ] network devices
* [ ] GPU abstraction

## Phase 4 — Object Storage

* [ ] Object filesystem
* [ ] Capability-based storage
* [ ] Copy-on-write
* [ ] Journaling
* [ ] Snapshots
* [ ] POSIX filesystem translation

## Phase 5 — Presentation

* [ ] Surface objects
* [ ] compositor
* [ ] input routing
* [ ] GPU buffer management
* [ ] Wayland compatibility
* [ ] GTK integration
* [ ] Qt integration

## Phase 6 — Compatibility

* [ ] POSIX API
* [ ] Linux userspace compatibility
* [ ] Linux ABI investigation
* [ ] GTK4 applications
* [ ] Android runtime
* [ ] Binder-to-capability translation

---

# 24. Architectural Goal

The ultimate goal is not to reproduce Linux with a different kernel.

It is to create a different underlying machine model:

```text
             Everything is an Object
                       │
                       ▼
             Authority is a Capability
                       │
                       ▼
             Communication is Transfer
                       │
                       ▼
             Memory is Owned or Borrowed
                       │
                       ▼
             Hardware is Capability-Bound
                       │
                       ▼
             Compatibility is Translation
```

The system should make unsafe global authority difficult to express rather than merely documenting that applications should avoid it.

---

# 25. Design Principles

XORG (OCAP Fabric OS) is guided by the following principles.

### 1. Authority must be explicit

No implicit access to system resources.

### 2. Objects over raw pointers

Applications manipulate resources through controlled object references.

### 3. Capabilities over names

Names identify things.

Capabilities grant authority.

### 4. Move instead of copy

Where possible, transfer ownership rather than duplicating data.

### 5. Borrow when ownership is unnecessary

Read-only sharing should not require unnecessary copying.

### 6. Revoke explicitly

Authority should be revocable independently of object identity.

### 7. Isolate hardware

Drivers should receive only the hardware authority they require.

### 8. Separate substrate from policy

The kernel provides mechanisms.

Higher-level services define policy.

### 9. Compatibility belongs above the native model

POSIX, Linux and Android should be translated into the capability architecture rather than defining it.

### 10. Performance and safety should reinforce each other

The architecture should seek zero-copy communication, explicit ownership and hardware isolation without sacrificing throughput.

---

# 26. Long-Term Vision

XORG (OCAP Fabric OS) is intended to become a system where the boundary between:

```text
memory
process
device
file
IPC
GPU resource
network endpoint
```

is no longer a collection of unrelated kernel mechanisms.

Instead, they become instances of one broader abstraction:

```text
                    ┌──────────────┐
                    │    Object    │
                    └──────┬───────┘
                           │
                  controlled by
                           │
                    ┌──────▼───────┐
                    │ Capability   │
                    └──────┬───────┘
                           │
                    transferred through
                           │
                    ┌──────▼───────┐
                    │    Fabric    │
                    └──────┬───────┘
                           │
              ┌────────────┼────────────┐
              │            │            │
           Memory       Hardware      Storage
              │            │            │
              └────────────┼────────────┘
                           │
                      Applications
```

The resulting operating system is neither purely monolithic nor purely microkernel.

It is a **capability-fabric architecture**: a system in which privileged substrate mechanisms, isolated services, object ownership, capability routing and compatibility environments cooperate through a single architectural model.

The objective is a machine where **authority has a shape, ownership has a lifetime, communication has a cost, and every resource has an explicit boundary.**
