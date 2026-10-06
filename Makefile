BUILD := build
KERNEL_DIR := kernel
KERNEL_ELF := $(KERNEL_DIR)/target/i686-none/debug/kernel
ISO := $(BUILD)/xorg.iso

.PHONY: all kernel iso run clean

all: $(ISO)

kernel:
	cd $(KERNEL_DIR) && cargo +nightly build -Z build-std=core,alloc -Z json-target-spec
	@test -f $(KERNEL_ELF) || (echo "kernel ELF not found at $(KERNEL_ELF)" && exit 1)

$(ISO): kernel
	mkdir -p $(BUILD)/iso/boot/grub
	cp $(KERNEL_ELF) $(BUILD)/iso/boot/kernel.elf
	cp grub.cfg $(BUILD)/iso/boot/grub/grub.cfg
	grub-mkrescue -o $@ $(BUILD)/iso

run: $(ISO)
	qemu-system-i386 -cdrom $(ISO) -serial stdio -no-reboot

clean:
	rm -rf $(BUILD)
	cd $(KERNEL_DIR) && cargo clean