/* boot/grub_header.S */
/*
 * Multiboot2 header and entry point for GRUB.
 *
 * See the accompanying linker script for the placement of the
 * kernel stack. The stack is defined by the linker (as the
 * `.bootstrap_stack` region), so it is positioned outside the
 * kernel's `.bss` and cannot be corrupted by stack usage from
 * Rust code.
 */

.section .multiboot2_header, "a"
.align 8
header_start:
    /* ---- Fixed header (16 bytes). ---- */
    .long   0xe85250d6
    .long   0
    .long   header_end - header_start
    .long   -(0xe85250d6 + 0 + (header_end - header_start))

    /* ---- Memory map request (type 6, required). ---- */
    .short  6
    .short  0
    .long   16
    .long   0
    .long   0

    /* ---- Framebuffer request (type 5, optional). ---- */
    .short  5
    .short  1
    .long   20
    .long   0
    .long   0
    .long   0
    .long   0

    /* ---- End tag (type 0). ---- */
    .short  0
    .short  0
    .long   8
header_end:

.section .text
.global _start
.extern kernel_main
.extern __bootstrap_stack_top

_start:
    /* Diagnostic marker 'S'. */
    xorw %dx, %dx
    movb $0xE9, %dl
    movb $'S', %al
    outb %al, %dx

    /* GRUB passes the Multiboot2 info pointer in EBX.
     * Set up a stack from the linker-provided symbol, then call
     * kernel_main with EBX as its first argument. */
    movl $__bootstrap_stack_top, %esp
    andl $0xFFFFFFF0, %esp

    pushl %ebx

    movb $'T', %al
    outb %al, %dx

    call kernel_main

    cli
    hlt
    jmp .