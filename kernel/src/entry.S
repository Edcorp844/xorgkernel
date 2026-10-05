.code32
.section .text._start, "ax"

.global _start
.extern kernel_main
.extern __bootstrap_stack_top

/*
 * Kernel entry point.
 *
 * The bootloader jumps here after loading the kernel and entering
 * protected mode. At this point:
 *
 *   - paging is disabled
 *   - the GDT is the bootloader's temporary GDT
 *   - ESP points into the bootloader's stack, which is not the
 *     kernel's own stack and must not be relied upon
 *
 * We switch to the linker-provided bootstrap stack and then call
 * the Rust entry point.
 */
_start:
    movl $__bootstrap_stack_top, %esp
    xorl %ebp, %ebp
    call kernel_main

    /* kernel_main is `-> !`. If we get here, halt. */
1:
    cli
    hlt
    jmp 1b