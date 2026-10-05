.code16

.section .text
.global _start

.equ BOOT_INFO,        0x5000
.equ E820_BUFFER,      0x5020
.equ E820_MAX_ENTRIES, 64

.equ BOOT_INFO_MAGIC,  0x4B42494F

.equ KERNEL_START,     0x8000
.equ KERNEL_END,       KERNEL_START + (KERNEL_SECTORS * 512)

_start:
    cli
    cld

    xorw %ax, %ax
    movw %ax, %ds
    movw %ax, %es
    movw %ax, %ss

    movw $0x7c00, %sp

    movb %dl, boot_drive

    #
    # Collect E820 memory map.
    #
    call detect_memory

    #
    # Load kernel:
    #
    # LBA 1 -> physical 0x8000
    #
    movw $0x0800, load_segment
    movw $1, current_lba
    movw $0, current_sector

load_kernel:

    #
    # Build Disk Address Packet.
    #
    movw $disk_address_packet, %si

    movb $0x10, 0(%si)
    movb $0x00, 1(%si)

    movw $1, 2(%si)

    movw $0x0000, 4(%si)

    movw load_segment, %ax
    movw %ax, 6(%si)

    movw current_lba, %ax
    movw %ax, 8(%si)

    movw $0, 10(%si)
    movw $0, 12(%si)
    movw $0, 14(%si)

    #
    # BIOS extended disk read.
    #
    movb $0x42, %ah
    movb boot_drive, %dl

    int $0x13

    jc disk_error

    #
    # Next sector.
    #
    incw current_lba

    #
    # 512 bytes = 0x20 paragraphs.
    #
    addw $0x20, load_segment

    incw current_sector

    cmpw $KERNEL_SECTORS, current_sector
    jb load_kernel

    #
    # Enable A20 using port 0x92.
    #
    inb $0x92, %al
    orb $0x02, %al
    outb %al, $0x92

    #
    # Load temporary GDT.
    #
    lgdt gdt_descriptor

    #
    # Enter protected mode.
    #
    movl %cr0, %eax
    orl $0x00000001, %eax
    movl %eax, %cr0

    #
    # Far jump reloads CS and flushes the prefetch queue.
    #
    ljmp $0x08, $protected_mode


.code32

protected_mode:

    movw $0x10, %ax

    movw %ax, %ds
    movw %ax, %es
    movw %ax, %ss
    movw %ax, %fs
    movw %ax, %gs

    #
    # Protected-mode stack.
    #
    movl $0x9FC00, %esp  

    cld

    #
    # Jump directly to Rust _start.
    #
    movl $KERNEL_ENTRY, %eax
    jmp *%eax


.code16

disk_error:

    movb $0x0e, %ah
    movb $'D', %al
    movb $0x00, %bh
    movb $0x07, %bl
    int $0x10

disk_error_hang:
    cli
    hlt
    jmp disk_error_hang


#
# BIOS Disk Address Packet
#
.align 4
disk_address_packet:

    .byte 0x10
    .byte 0x00
    .word 1

    .word 0x0000
    .word 0x0800

    .quad 1


boot_drive:
    .byte 0


.align 2

load_segment:
    .word 0x0800

current_lba:
    .word 1

current_sector:
    .word 0


#
# Temporary GDT
#
.align 8

gdt:

    #
    # Null descriptor
    #
    .quad 0x0000000000000000

    #
    # Code: 0x08
    #
    .word 0xFFFF
    .word 0x0000
    .byte 0x00
    .byte 0x9A
    .byte 0xCF
    .byte 0x00

    #
    # Data: 0x10
    #
    .word 0xFFFF
    .word 0x0000
    .byte 0x00
    .byte 0x92
    .byte 0xCF
    .byte 0x00

gdt_end:

gdt_descriptor:

    .word gdt_end - gdt - 1
    .long gdt


#
# E820 memory detection
#
detect_memory:

    movl $BOOT_INFO_MAGIC, %eax
    movl %eax, BOOT_INFO

    xorl %eax, %eax
    movl %eax, BOOT_INFO + 4

    movl $E820_BUFFER, %eax
    movl %eax, BOOT_INFO + 8

    movl $KERNEL_START, %eax
    movl %eax, BOOT_INFO + 12

    movl $KERNEL_END, %eax
    movl %eax, BOOT_INFO + 16

    xorl %ebx, %ebx

    xorw %ax, %ax
    movw %ax, %es

    movw $E820_BUFFER, %di


e820_next:

    movl $0xE820, %eax
    movl $0x534D4150, %edx
    movl $24, %ecx

    int $0x15

    jc e820_done

    cmpl $0x534D4150, %eax
    jne e820_done

    cmpl $20, %ecx
    jb e820_done

    incl BOOT_INFO + 4

    addw $24, %di

    cmpl $E820_MAX_ENTRIES, BOOT_INFO + 4
    jae e820_done

    testl %ebx, %ebx
    jnz e820_next


e820_done:

    ret