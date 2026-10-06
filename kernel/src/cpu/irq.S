/*
 * IRQ entry stubs.
 *
 * Hardware interrupts from the 8259 PIC arrive on vectors 32-47
 * after the PIC has been remapped. The CPU pushes an interrupt
 * frame (EIP, CS, EFLAGS) onto the current stack and jumps to the
 * stub for the vector.
 *
 * Each stub:
 *
 *   1. Saves all general-purpose registers (pushal).
 *   2. Pushes the IRQ line number as the argument to
 *      irq_dispatch. The line number is `vector - 32`, because
 *      vectors 32-47 correspond to IRQ lines 0-15.
 *   3. Calls irq_dispatch.
 *   4. Cleans up the argument.
 *   5. Restores the general-purpose registers (popal).
 *   6. Returns from the interrupt (iret).
 */

.code32

.section .text

.extern irq_dispatch

.macro IRQ_STUB vector

.global irq_entry_\vector
.type irq_entry_\vector, @function

irq_entry_\vector:
    pushal
    pushl $(\vector - 32)
    call irq_dispatch
    addl $4, %esp
    popal
    iret

.endm

/* Vectors 32-47 correspond to IRQ lines 0-15. */

IRQ_STUB 32
IRQ_STUB 33
IRQ_STUB 34
IRQ_STUB 35
IRQ_STUB 36
IRQ_STUB 37
IRQ_STUB 38
IRQ_STUB 39
IRQ_STUB 40
IRQ_STUB 41
IRQ_STUB 42
IRQ_STUB 43
IRQ_STUB 44
IRQ_STUB 45
IRQ_STUB 46
IRQ_STUB 47