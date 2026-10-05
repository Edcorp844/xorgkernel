BUILD := build
KERNEL := kernel

CLANG := clang
LLD := ld.lld
OBJCOPY := llvm-objcopy
NM := llvm-nm
SIZE := llvm-size
OBJDUMP := llvm-objdump
READELF := llvm-readelf

QEMU := qemu-system-i386

KERNEL_ELF := $(KERNEL)/target/i686-none/debug/kernel
KERNEL_BIN := $(KERNEL)/target/i686-none/debug/kernel.bin
DISK_IMAGE := $(BUILD)/disk.img

# Deferred evaluation is important:
# the kernel ELF may not exist when Make first reads this file.
KERNEL_ENTRY = $(shell \
	$(NM) $(KERNEL_ELF) 2>/dev/null | \
	awk '$$3 == "_start" { print "0x" $$1 }')

CFLAGS := \
	-target i386-unknown-none \
	-m16 \
	-ffreestanding \
	-fno-pic \
	-fno-stack-protector \
	-nostdlib

.PHONY: all clean run debug inspect boot kernel disk

# ============================================================
# Default target
# ============================================================

all: $(DISK_IMAGE)


# ============================================================
# Build directory
# ============================================================

$(BUILD):
	mkdir -p $(BUILD)


# ============================================================
# Rust kernel
# ============================================================

kernel: $(KERNEL_BIN)

$(KERNEL_ELF):
	cd $(KERNEL) && \
	cargo +nightly build \
    	-Z build-std=core,alloc \
    	-Z json-target-spec

$(KERNEL_BIN): $(KERNEL_ELF)
	$(OBJCOPY) \
		-O binary \
		$< \
		$@


# ============================================================
# Stage 1 bootloader
# ============================================================

$(BUILD)/boot.o: boot/boot.S $(KERNEL_BIN) | $(BUILD)
	@KERNEL_SIZE=$$(wc -c < $(KERNEL_BIN)); \
	KERNEL_SECTORS=$$(( (KERNEL_SIZE + 511) / 512 )); \
	echo "Building bootloader"; \
	echo "Kernel size:    $$KERNEL_SIZE bytes"; \
	echo "Kernel sectors: $$KERNEL_SECTORS"; \
	echo "Kernel entry:   $(KERNEL_ENTRY)"; \
	$(CLANG) \
		$(CFLAGS) \
		-DKERNEL_SECTORS=$$KERNEL_SECTORS \
		-DKERNEL_ENTRY=$(KERNEL_ENTRY) \
		-c boot/boot.S \
		-o $@

$(BUILD)/boot.elf: $(BUILD)/boot.o boot/boot.ld
	$(LLD) \
		-m elf_i386 \
		-T boot/boot.ld \
		-o $@ \
		$(BUILD)/boot.o

$(BUILD)/boot.bin: $(BUILD)/boot.elf
	$(OBJCOPY) \
		-O binary \
		$< \
		$@

	@test "$$(wc -c < $@)" -eq 512

boot: $(BUILD)/boot.bin


# ============================================================
# Disk image
# ============================================================

$(DISK_IMAGE): $(BUILD)/boot.bin $(KERNEL_BIN) | $(BUILD)
	@KERNEL_SIZE=$$(wc -c < $(KERNEL_BIN)); \
	KERNEL_SECTORS=$$(( (KERNEL_SIZE + 511) / 512 )); \
	TOTAL_SECTORS=$$(( 1 + KERNEL_SECTORS )); \
	echo "Creating disk image"; \
	echo "Kernel size:    $$KERNEL_SIZE bytes"; \
	echo "Kernel sectors: $$KERNEL_SECTORS"; \
	echo "Disk sectors:   $$TOTAL_SECTORS"; \
	dd if=/dev/zero \
		of=$@ \
		bs=512 \
		count=$$TOTAL_SECTORS \
		status=none; \
	dd if=$(BUILD)/boot.bin \
		of=$@ \
		bs=512 \
		seek=0 \
		conv=notrunc \
		status=none; \
	dd if=$(KERNEL_BIN) \
		of=$@ \
		bs=512 \
		seek=1 \
		conv=notrunc \
		status=none

disk: $(DISK_IMAGE)


# ============================================================
# Run
# ============================================================

run: $(DISK_IMAGE)
	$(QEMU) \
		-drive format=raw,file=$(DISK_IMAGE) \
		-serial stdio 


# ============================================================
# Debug
# ============================================================

debug: $(DISK_IMAGE)
	$(QEMU) \
		-drive format=raw,file=$(DISK_IMAGE) \
		-serial stdio \
		-S \
		-s


# ============================================================
# Inspect everything
# ============================================================

inspect: $(BUILD)/boot.elf $(BUILD)/boot.bin $(KERNEL_ELF) $(KERNEL_BIN) $(DISK_IMAGE)

	@echo
	@echo "============================================================"
	@echo "BOOT SECTOR"
	@echo "============================================================"

	@echo "Size:"
	wc -c $(BUILD)/boot.bin

	@echo
	@echo "Boot signature:"
	xxd -s 510 -l 2 $(BUILD)/boot.bin

	@echo
	@echo "============================================================"
	@echo "RUST KERNEL"
	@echo "============================================================"

	@echo "Kernel size:"
	wc -c $(KERNEL_BIN)

	@echo
	@echo "Rust entry point:"
	$(NM) $(KERNEL_ELF) | grep '_start'

	@echo
	@echo "ELF header:"
	$(READELF) -h $(KERNEL_ELF) | \
		grep -E 'Class:|Machine:|Entry point'

	@echo
	@echo "============================================================"
	@echo "DISK IMAGE"
	@echo "============================================================"

	@echo "Disk size:"
	wc -c $(DISK_IMAGE)

	@echo
	@echo "Boot sector inside disk:"
	xxd -s 510 -l 2 $(DISK_IMAGE)

	@echo
	@echo "Kernel beginning at LBA 1:"
	xxd -s 512 -l 32 $(DISK_IMAGE)

	@echo
	@echo "============================================================"
	@echo "BOOT ELF"
	@echo "============================================================"

	@echo "Size:"
	$(SIZE) $(BUILD)/boot.elf

	@echo
	@echo "Disassembly:"
	$(OBJDUMP) -d $(BUILD)/boot.elf


# ============================================================
# Clean
# ============================================================

clean:
	rm -rf $(BUILD)
	cd $(KERNEL) && cargo clean