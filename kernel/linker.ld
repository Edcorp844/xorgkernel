/*
 * Kernel linker script for GRUB / Multiboot2.
 *
 * Layout:
 *
 *   0x100000   .multiboot2_header
 *              .text
 *              .rodata
 *              .data
 *              .got
 *   __bootstrap_start
 *              .page_directory     (NOLOAD)
 *              .page_table         (NOLOAD)
 *              .idt                (NOLOAD)
 *              .gdt                (NOLOAD)
 *              .frame_bitmap       (NOLOAD)
 *              .bss
 *   __kernel_end
 *              .bootstrap_stack    (NOLOAD)
 *   __bootstrap_stack_top
 *
 * See the previous linker script for the rationale on the
 * bootstrap regions and why the GOT and stack are placed where
 * they are.
 */

ENTRY(_start)

SECTIONS
{
    . = 0x100000;
    __kernel_start = .;

    .multiboot2_header : ALIGN(8)
    {
        KEEP(*(.multiboot2_header))
    }

    .text : ALIGN(16)
    {
        KEEP(*(.text._start))
        *(.text*)
    }

    .rodata : ALIGN(16)
    {
        *(.rodata*)
    }

    .data : ALIGN(16)
    {
        *(.data*)
    }

    .got : ALIGN(16)
    {
        *(.got*)
    }

    __bootstrap_start = .;

    .page_directory (NOLOAD) : ALIGN(0x1000)
    {
        __page_directory_start = .;
        . += 0x1000;
        __page_directory_end = .;
    }

    .page_table (NOLOAD) : ALIGN(0x1000)
    {
        __page_table_start = .;
        . += 0x1000;
        __page_table_end = .;
    }

    .idt (NOLOAD) : ALIGN(16)
    {
        __idt_start = .;
        . += 2048;
        __idt_end = .;
    }

    .gdt (NOLOAD) : ALIGN(16)
    {
        __gdt_start = .;
        . += 48;
        __gdt_end = .;
    }

    .frame_bitmap (NOLOAD) : ALIGN(0x1000)
    {
        __frame_bitmap_start = .;
        . += 128 * 1024;
        __frame_bitmap_end = .;
    }

    .bss : ALIGN(0x1000)
    {
        __bss_start = .;
        *(.bss*)
        *(COMMON)
        __bss_end = .;
    }

    __kernel_end = .;

    .bootstrap_stack (NOLOAD) : ALIGN(0x1000)
    {
        __bootstrap_stack_bottom = .;
        . += 128 * 1024;
        __bootstrap_stack_top = .;
    }

    /DISCARD/ :
    {
        *(.comment)
        *(.note*)
        *(.eh_frame*)
    }
}