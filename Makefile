# ============================================================
# XORG kernel build
# ============================================================
#
# This Makefile builds the kernel, wraps it in a GRUB-bootable
# ISO, and runs it under QEMU. Every path, tool name, and QEMU
# flag is declared here as a variable, so a change to any of
# them is a one-line edit at the top.
#
# Usage:
#
#   make            build the ISO
#   make kernel     build just the kernel ELF
#   make run        build and run under QEMU with serial output
#   make debug      build and run with a debug log and no window
#   make clean      remove build artefacts
#
# ============================================================

# ---------- Directories ----------

# Root directory for build artefacts.
BUILD := build

# Directory containing the Rust kernel crate.
KERNEL_DIR := kernel

# Directory inside the ISO where the kernel ELF is placed.
ISO_BOOT_DIR := $(BUILD)/iso/boot

# Directory inside the ISO where GRUB reads its config.
ISO_GRUB_DIR := $(ISO_BOOT_DIR)/grub

# ---------- Kernel artefacts ----------

# Rust target triple, matching the JSON target spec in the crate.
KERNEL_TARGET := i686-none

# Cargo profile: debug or release.
KERNEL_PROFILE := debug

# Where Cargo places the compiled kernel ELF.
KERNEL_ELF := $(KERNEL_DIR)/target/$(KERNEL_TARGET)/$(KERNEL_PROFILE)/kernel

# Name of the kernel file inside the ISO.
ISO_KERNEL_NAME := kernel.elf

# Full path of the kernel inside the build tree's ISO staging area.
ISO_KERNEL := $(ISO_BOOT_DIR)/$(ISO_KERNEL_NAME)

# ---------- GRUB ----------

# Path to the GRUB configuration in the source tree.
GRUB_CONFIG_SRC := grub.cfg

# Path to the GRUB configuration in the ISO staging area.
GRUB_CONFIG_DST := $(ISO_GRUB_DIR)/grub.cfg

# ---------- Output image ----------

# Name of the final bootable ISO.
ISO := $(BUILD)/xorg.iso

# ---------- Tools ----------

CARGO := cargo
CARGO_NIGHTLY := +nightly
CARGO_BUILD_STD := core,alloc
CARGO_FLAGS := -Z build-std=$(CARGO_BUILD_STD) -Z json-target-spec
GRUB_MKRESCUE := grub-mkrescue
CP := cp
MKDIR := mkdir -p
RM := rm -rf
TEST := test

# ---------- QEMU ----------

QEMU := qemu-system-i386

# Where QEMU reads the boot medium from.
QEMU_MEDIUM := -cdrom $(ISO)

# Serial output goes to the terminal.
QEMU_SERIAL := -serial stdio

# Halt instead of reboot on triple fault.
QEMU_NO_REBOOT := -no-reboot

# Suppress the graphical window.
QEMU_NO_DISPLAY := -display none

# Write debug-port (0xE9) output to a file.
QEMU_DEBUG_FILE := debugcon.log
QEMU_DEBUG := -debugcon file:$(QEMU_DEBUG_FILE)

# Complete QEMU invocation for `make run`.
QEMU_RUN_ARGS := \
	$(QEMU_MEDIUM) \
	$(QEMU_SERIAL) \
	$(QEMU_NO_REBOOT)

# Complete QEMU invocation for `make debug`.
QEMU_DEBUG_ARGS := \
	$(QEMU_MEDIUM) \
	$(QEMU_SERIAL) \
	$(QEMU_NO_DISPLAY) \
	$(QEMU_NO_REBOOT) \
	$(QEMU_DEBUG)

# ============================================================
# Targets
# ============================================================

.PHONY: all kernel iso run debug clean

all: $(ISO)

# ---------- Kernel ----------

# Build the kernel ELF from the Rust crate.
#
# The kernel crate uses a custom JSON target spec (`i686-none.json`)
# and builds its own copy of `core` and `alloc` via `-Z build-std`.
# Both are nightly-only features.
kernel:
	cd $(KERNEL_DIR) && \
	$(CARGO) $(CARGO_NIGHTLY) build $(CARGO_FLAGS)
	@$(TEST) -f $(KERNEL_ELF) || \
		(echo "kernel ELF not found at $(KERNEL_ELF)" && exit 1)

# ---------- ISO ----------

# Assemble the ISO from the kernel ELF and GRUB config.
$(ISO): kernel
	$(MKDIR) $(ISO_GRUB_DIR)
	$(CP) $(KERNEL_ELF) $(ISO_KERNEL)
	$(CP) $(GRUB_CONFIG_SRC) $(GRUB_CONFIG_DST)
	$(GRUB_MKRESCUE) -o $@ $(BUILD)/iso

iso: $(ISO)

# ---------- Run ----------

# Run the ISO under QEMU, with the kernel's serial output
# connected to the terminal.
run: $(ISO)
	$(QEMU) $(QEMU_RUN_ARGS)

# Run the ISO under QEMU with a debug log file and no window.
#
# The debug log records writes to port 0xE9. The kernel writes
# diagnostic bytes there from a few places during boot; the file
# can be inspected with `cat $(QEMU_DEBUG_FILE)`.
debug: $(ISO)
	$(QEMU) $(QEMU_DEBUG_ARGS)

# ---------- Clean ----------

clean:
	$(RM) $(BUILD)
	cd $(KERNEL_DIR) && $(CARGO) clean