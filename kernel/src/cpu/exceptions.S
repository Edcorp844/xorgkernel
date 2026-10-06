.code32

.section .text

.extern exception_dispatch


/*
 * Exceptions which do not push an error code.
 *
 * CPU stack on entry:
 *
 *     EIP
 *     CS
 *     EFLAGS
 *
 * We save registers and construct:
 *
 *     error_code
 *     EIP
 *     CS
 *     EFLAGS
 */
.macro EXCEPTION_NO_ERROR vector

.global exception_entry_\vector
.type exception_entry_\vector, @function

exception_entry_\vector:

    pushal

    /*
     * Stack after pushal:
     *
     * +0   EDI
     * +4   ESI
     * +8   EBP
     * +12  original ESP
     * +16  EBX
     * +20  EDX
     * +24  ECX
     * +28  EAX
     *
     * +32  EIP
     * +36  CS
     * +40  EFLAGS
     */

    /*
     * Save the original exception frame values in registers
     * before changing the stack.
     */
    movl 32(%esp), %eax       /* EIP */
    movl 36(%esp), %ecx       /* CS */
    movl 40(%esp), %edx       /* EFLAGS */

    /*
     * Construct:
     *
     * +0   error_code
     * +4   EIP
     * +8   CS
     * +12  EFLAGS
     *
     * +16  registers
     */

    pushl %edx
    pushl %ecx
    pushl %eax
    pushl $0

    /*
     * Current stack:
     *
     * +0   error_code
     * +4   EIP
     * +8   CS
     * +12  EFLAGS
     *
     * +16  EDI
     * +20  ESI
     * +24  EBP
     * +28  original ESP
     * +32  EBX
     * +36  EDX
     * +40  ECX
     * +44  EAX
     */

    leal 16(%esp), %eax
    movl %esp, %edx

    /*
     * C ABI:
     *
     * exception_dispatch(
     *     vector,
     *     registers,
     *     frame
     * )
     */

    pushl %edx
    pushl %eax
    pushl $\vector

    call exception_dispatch

1:
    cli
    hlt
    jmp 1b

.endm

/*
 * EXCEPTION_ERROR - exception entry stub for vectors that push a
 * hardware error code.
 *
 * Used for vectors: 8, 10, 11, 12, 13, 14, 17, 21, 30.
 *
 * CPU stack on entry (pushed by the processor itself):
 *
 *     +0   error_code   <- ESP points here when the stub starts
 *     +4   EIP
 *     +8   CS
 *     +12  EFLAGS
 *
 * The stub's job is to:
 *   1. Save the general-purpose registers (pushal).
 *   2. Build a pointer to the saved registers (Registers struct).
 *   3. Build a pointer to the CPU-pushed exception frame
 *      (ExceptionFrame struct).
 *   4. Call the Rust handler exception_dispatch(vector, regs, frame).
 *   5. If the handler ever returns (it shouldn't; it's `-> !`),
 *      halt the CPU so we don't execute garbage.
 */
.macro EXCEPTION_ERROR vector

/* Export the symbol so the IDT can reference it by name. */
.global exception_entry_\vector
.type exception_entry_\vector, @function
exception_entry_\vector:

    /*
     * Diagnostic: write 'E' to QEMU's debug port (0xE9).
     *
     * This goes straight to the emulator's stderr and needs no UART
     * handshake, so it works even if the serial port is wedged.
     *
     * If we see 'E' but nothing else, the fault happened after this
     * point but before the next marker.
     */
    movb $'X', %al
    outb %al, $0xE9

    /*
     * Save EAX, ECX, EDX, EBX, original ESP, EBP, ESI, EDI.
     *
     * The pushes happen in that order, so the stack grows downward
     * and ends up with the LOWEST address holding EDI:
     *
     *     +0   EDI
     *     +4   ESI
     *     +8   EBP
     *     +12  original ESP
     *     +16  EBX
     *     +20  EDX
     *     +24  ECX
     *     +28  EAX
     *     +32  error_code   <- CPU-pushed frame starts here
     *     +36  EIP
     *     +40  CS
     *     +44  EFLAGS
     *
     * This layout matches the Rust `Registers` struct, whose fields
     * are declared in exactly that order.
     */
    pushal

    /*
     * Diagnostic: 'P' marks the point after pushal succeeded.
     */
    movb $'P', %al
    outb %al, $0xE9

    /*
     * Build a pointer to the ExceptionFrame.
     *
     * The CPU pushed {error_code, EIP, CS, EFLAGS} before entering
     * this stub. After pushal, that block starts at ESP + 32.
     */
    leal 32(%esp), %eax

    /*
     * Build a pointer to the Registers struct.
     *
     * The Registers struct is exactly the pushal block, so its
     * address is the current ESP.
     */
    movl %esp, %edx

    /*
     * Push the three arguments for exception_dispatch in reverse
     * order (cdecl: last argument pushed first).
     *
     * After these three pushes, the callee will see:
     *
     *     [esp+4]  = vector    (u32)
     *     [esp+8]  = &Registers
     *     [esp+12] = &ExceptionFrame
     */
    pushl %eax            /* &ExceptionFrame */
    pushl %edx            /* &Registers      */
    pushl $\vector        /* vector number   */

    /*
     * Diagnostic: 'C' marks that argument setup succeeded and we are
     * about to branch into Rust code.
     */
    movb $'C', %al
    outb %al, $0xE9

    /*
     * Enter the Rust exception dispatcher.
     *
     * exception_dispatch is declared `-> !`, so it must never return.
     * If it somehow does, execution falls into the trap below.
     */
    call exception_dispatch

    /*
     * Safety net. Reaching this label means exception_dispatch
     * returned, which violates its contract. Halt the CPU.
     */
1:
    cli
    hlt
    jmp 1b

.endm


/*
 * CPU exception vectors 0-31.
 *
 * Exceptions with hardware error codes:
 *
 *     8   Double Fault
 *     10  Invalid TSS
 *     11  Segment Not Present
 *     12  Stack-Segment Fault
 *     13  General Protection Fault
 *     14  Page Fault
 *     17  Alignment Check
 *     21  Control Protection
 *     30  Security Exception
 */


/* 0-7: no hardware error code */

EXCEPTION_NO_ERROR 0
EXCEPTION_NO_ERROR 1
EXCEPTION_NO_ERROR 2
EXCEPTION_NO_ERROR 3
EXCEPTION_NO_ERROR 4
EXCEPTION_NO_ERROR 5
EXCEPTION_NO_ERROR 6
EXCEPTION_NO_ERROR 7


/* 8: hardware error code */

EXCEPTION_ERROR 8


/* 9: no hardware error code */

EXCEPTION_NO_ERROR 9


/* 10-14: hardware error code */

EXCEPTION_ERROR 10
EXCEPTION_ERROR 11
EXCEPTION_ERROR 12
EXCEPTION_ERROR 13
EXCEPTION_ERROR 14


/* 15-16: no hardware error code */

EXCEPTION_NO_ERROR 15
EXCEPTION_NO_ERROR 16


/* 17: hardware error code */

EXCEPTION_ERROR 17


/* 18-20: no hardware error code */

EXCEPTION_NO_ERROR 18
EXCEPTION_NO_ERROR 19
EXCEPTION_NO_ERROR 20


/* 21: hardware error code */

EXCEPTION_ERROR 21


/* 22-29: no hardware error code */

EXCEPTION_NO_ERROR 22
EXCEPTION_NO_ERROR 23
EXCEPTION_NO_ERROR 24
EXCEPTION_NO_ERROR 25
EXCEPTION_NO_ERROR 26
EXCEPTION_NO_ERROR 27
EXCEPTION_NO_ERROR 28
EXCEPTION_NO_ERROR 29


/* 30: hardware error code */

EXCEPTION_ERROR 30


/* 31: no hardware error code */

EXCEPTION_NO_ERROR 31

