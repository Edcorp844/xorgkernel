# Contributing

This document explains how to build the kernel, how to run it,
how to add tests, and what conventions the codebase follows.

For the architectural vision, see `README.md`. For the code
structure, see `docs/architecture.md`. For the invariants each
subsystem maintains, see `docs/substrate-contract.md`.

## Prerequisites

The kernel targets i686 and is built as a freestanding
binary. It requires:

- A nightly Rust toolchain, installed via `rustup`.
- `rust-src` for the nightly toolchain, installed via
  `rustup component add rust-src --toolchain nightly`.
- Clang and LLD, for building the assembly stubs and linking.
- GRUB 2, for producing the bootable ISO.
- `xorriso` and `mtools`, required by `grub-mkrescue`.
- QEMU, for running the kernel.

On Debian or Ubuntu:
```
sudo apt install clang lld grub-pc-bin xorriso mtools qemu-system-x86
```

On Fedora:
```
sudo dnf install clang lld grub2-tools xorriso mtools qemu-system-x86
```

On Arch:
```
sudo pacman -S clang lld grub libisoburn mtools qemu-system-x86
```

The exact package names differ. What matters is that
`clang`, `ld.lld`, `grub-mkrescue`, `xorriso`, and
`qemu-system-i386` are all on the `PATH`.

## Building

The project has one top-level Makefile. All build commands run
from the repository root.

**Build the ISO.**
```
make
```

This compiles the Rust kernel, assembles the assembly stubs,
copies the kernel into a GRUB-bootable ISO staging directory,
and runs `grub-mkrescue` to produce `build/xorg.iso`.

**Build only the kernel.**
```
make kernel
```

This compiles the Rust crate and links it into an ELF. The
result is `kernel/target/i686-none/debug/kernel`.

**Clean the build tree.**
```
make clean
```

This removes `build/` and runs `cargo clean` in the kernel
crate.

## Running

**Run the kernel in QEMU.**
```
make run
```

This builds the ISO if needed and starts QEMU with the ISO as
a CD-ROM. Serial output is connected to the terminal. The
QEMU window shows the framebuffer output.

**Run with a debug log.**
```
make debug
```

This is the same as `make run` but suppresses the QEMU window
and writes debug-port (0xE9) output to `debugcon.log`. Useful
for debugging early boot, when serial and framebuffer are not
yet available.

**Run with GDB.**
```
make debug-gdb
```

This starts QEMU with a GDB stub on port 1234, paused. In
another terminal:
```
gdb kernel/target/i686-none/debug/kernel
(gdb) target remote localhost:1234
```

You can set breakpoints, step through code, and inspect
memory.

## Making changes

The code is organized into nine subsystems. Before making a
change, read `docs/architecture.md` to see where the change
belongs.

A typical change follows this sequence:

1. Make the change in the appropriate module.
2. Add or update tests in `kernel/src/tests/`.
3. Add or update the module's documentation and the relevant
   document under `docs/`.
4. Build and run the kernel. Verify that the tests pass.
5. Commit.

## Testing

The kernel's tests live in `kernel/src/tests/`, one file per
subsystem. Tests are run at boot, after the substrate is
initialized and before the scheduler takes over. They run
with interrupts disabled.

To add a test:

1. Write the test function in the appropriate file. It should
   print progress to the console with `println!` and panic on
   failure with `assert!` or `assert_eq!`.
2. Add the function to `kernel/src/tests/mod.rs`'s
   `run_all`.

Tests should be self-contained and should not depend on
other tests having run first. If a test allocates resources,
it should free them before returning.

## Coding conventions

**Formatting.** The code is formatted with `rustfmt` using
default settings. Run `cargo fmt` before committing.

**Naming.** Rust's standard conventions apply:

- Types are `UpperCamelCase`.
- Functions and variables are `snake_case`.
- Constants are `SCREAMING_SNAKE_CASE`.
- Modules are `snake_case`.

**Documentation.** Public items have `///` doc comments. The
first line is a one-sentence summary. Subsequent paragraphs
explain behavior, invariants, and edge cases.

Modules have `//!` doc comments at the top of the file. The
first paragraph explains the module's purpose. Later
paragraphs explain its design, its invariants, and its
relationship to other modules.

**Comments.** Comments explain *why*, not *what*. The code
already says what it does. Comments say why it does that
instead of something else.

**Unsafe.** Every `unsafe` block has a `// SAFETY:` comment
explaining why the operations inside are sound. The safety
comment should be specific: not "this is safe because it's a
kernel" but "this pointer is valid because it was obtained
from `Box::into_raw` and the `Box` has not been dropped."

**Errors.** In the kernel, most errors are programmer errors
and are handled with `panic!` or `assert!`. Fallible
operations that a caller might reasonably encounter (frame
allocation failure, I/O failure, capability denial) return
`Option` or `Result`.

## Documentation

The `docs/` directory contains design documents. Each explains
a subsystem, its design, and its open questions.

- `architecture.md` — module layout and dependencies.
- `boot-sequence.md` — the path from firmware to the first
  task.
- `substrate-contract.md` — invariants each subsystem
  maintains.
- `memory.md` — the memory hierarchy.
- `capability.md` — the fabric.
- `scheduler.md` — the scheduler.
- `cell-model.md` — the two-tier execution model.

When you change a subsystem, update its document. When you
add a subsystem, add a document. The documents are the source
of truth for design intent.

## Adding a subsystem

To add a new subsystem:

1. Create a directory under `kernel/src/`.
2. Add a `mod.rs` that lists its submodules and re-exports
   its public API.
3. Add a `pub mod` line to `kernel/src/main.rs`.
4. Add tests in `kernel/src/tests/` if the subsystem has
   behavior worth testing.
5. Add a document in `docs/` if the subsystem has design
   decisions worth explaining.
6. Update `docs/architecture.md` with the new subsystem.

## Committing

Commits should be self-contained and should have a clear
message. The message format follows the common convention:
```
Short summary (50 characters or less)

Longer explanation of what changed and why. Wrap at 72
characters. Explain the reasoning, not just the mechanics.

    Bullet points for individual changes

    Each bullet describes one coherent change

Verified:

    What you tested

    What you observed
```

The summary line should be in the imperative mood: "Add
scheduler" not "Added scheduler". The body explains the
motivation and any non-obvious decisions. The "Verified"
section describes what you actually ran and what you saw.

## Getting help

The repository's issue tracker is the place to ask questions
and report bugs. Before opening an issue, check that the
problem is not already addressed in the documentation.

For design questions, refer to `docs/`. For code structure,
refer to `docs/architecture.md`. For subsystem behavior,
refer to the relevant design document.
