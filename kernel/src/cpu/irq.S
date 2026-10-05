.code32

.section .text

.extern irq_dispatch

/*
 * IRQ entry stubs.
 *
 * Hardware interrupts from the 8259 PIC arrive on vectors 32-47
 * (0x20-0x2F) after the PIC has been remapped. Unlike CPU
 * exceptions, hardware interrupts do not push an error code, and
 * they must be acknowledged and returned from with `iret` rather
 * than halting the CPU.
 *
 * The stub for each vector:
 *
 *   1. Pushes the IRQ number onto the stack.
 *   2. Calls the Rust dispatcher, which invokes the registered
 *      handler and sends an EOI to the PIC.
 *   3. Pops the IRQ number.
 *   4. Returns with `iret`, restoring the interrupted context.
 *
 * # Register saving
 *
 * The dispatcher is a normal C function. The C ABI says that
 * EAX, ECX, and EDX are caller-saved, so the callee is free to
 * clobber them. But an interrupt can happen at any time, including
 * in the middle of code that was relying on those registers. We
 * therefore save them in the stub.
 *
 * EBP, EBX, ESI, and EDI are callee-saved by the C ABI: the
 * dispatcher promises to preserve them, so the stub does not need
 * to save them explicitly. However, the compiler is free to use
 * EBP as a frame pointer, and we want the stack frame that `iret`
 * sees to be exactly what the CPU pushed, so we push a full
 * `pushal` anyway. The cost is negligible and the resulting code
 * is simpler to reason about.
 *
 * # Stack layout during dispatch
 *
 *   +0   EDI          ^
 *   +4   ESI          |
 *   +8   EBP          |  pushal
 *   +12  ESP_before   |
 *   +16  EBX          |
 *   +20  EDX          |
 *   +24  ECX          |
 *   +28  EAX          v
 *   +32  IRQ number
 *   +36  EIP          ^
 *   +40  CS           |  pushed by CPU on interrupt
 *   +44  EFLAGS       v
 */

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


/*
 * Vectors 32-47 correspond to IRQ0-15 after the PIC remap.
 */

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