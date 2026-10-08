/*
 * Syscall entry stub.
 *
 * `int 0x80` is the kernel's syscall instruction. The gate is
 * installed at DPL 3 in the IDT, so user code at CPL 3 can invoke
 * it without a #GP. From CPL 3 the CPU loads `SS:ESP` from the
 * TSS's `ss0`/`esp0`, which the scheduler keeps pointed at the
 * running task's kernel stack top.
 *
 * # Frame shape varies by origin
 *
 * The CPU pushes a different frame depending on the privilege
 * level the trap came from:
 *
 *   From CPL 0:  [EFLAGS][CS][EIP]
 *   From CPL 3:  [SS][ESP][EFLAGS][CS][EIP]
 *
 * The stub does **not** normalize. It saves the general-purpose
 * registers and passes the current ESP to the Rust handler. The
 * handler inspects the saved CS to determine which shape it is
 * looking at, and reads the appropriate fields.
 *
 * This is simpler than normalizing in assembly. The handler has to
 * know the shape anyway to build the return, and having the
 * decision in Rust means it can be tested and audited in a
 * language that is easier to reason about than assembly.
 *
 * # Segment reload
 *
 * When the trap arrives from CPL 3, the CPU does not reload
 * DS/ES/FS/GS from the kernel's data descriptor. They remain at
 * whatever the user task had them set to, or (after the initial
 * `iret` into user mode, which zeroes them by architectural rule)
 * at a null selector. Any kernel data access via DS would #GP.
 *
 * The first thing the stub does, therefore, is reload DS/ES/FS/GS
 * with the kernel's data selector (`0x10`). This must happen
 * before any other code in the stub accesses memory via DS, and
 * it must not clobber a register whose value the caller expects.
 * `%ax` is used as scratch, with `%eax` saved and restored around
 * the reload.
 *
 * # Register layout after pushal
 *
 * From lowest address (current ESP) to highest:
 *
 *   +0   EDI
 *   +4   ESI
 *   +8   EBP
 *   +12  original ESP
 *   +16  EBX
 *   +20  EDX
 *   +24  ECX
 *   +28  EAX        <- the handler writes its return value here
 *   +32  EIP
 *   +36  CS
 *   +40  EFLAGS
 *   +44  ESP        (only if from CPL 3)
 *   +48  SS         (only if from CPL 3)
 *
 * The Rust `SyscallFrame` struct describes this layout. See
 * `cpu/syscall.rs` for how the handler reads it.
 *
 * # Return value
 *
 * The handler writes its return value into the saved-EAX slot at
 * `esp + 28` before returning. The stub then does `popal`, which
 * restores EAX from that slot, so the caller sees the return value
 * in EAX after `iret`.
 *
 * # Trap gate vs. interrupt gate
 *
 * The IDT entry uses type 0xE (32-bit interrupt gate), so IF is
 * cleared on entry. The handler runs with interrupts disabled.
 * This is the safe choice: a syscall cannot be preempted mid-
 * execution by a timer tick, which would corrupt the handler's
 * view of the frame.
 *
 * # Diagnostic markers
 *
 * The stub writes two single-byte markers to the QEMU debug port
 * (0xE9) so that the return path can be traced from the debugcon
 * log:
 *
 *   'A'  just before `popal`           — reached if the call into
 *                                        the handler returned
 *   'B'  just before `iret`            — reached if `popal`
 *                                        completed
 *
 * If the log ends at 'A', the fault is in `popal`. If it ends at
 * 'B', the fault is in `iret`. If neither appears, the fault is
 * in the call to the handler or in the handler itself.
 *
 * Both markers preserve EAX across the `outb` by saving and
 * restoring it. Removing the markers when the return path is
 * confirmed working is a one-line edit each.
 */

.code32

.section .text

.extern syscall_dispatch

.global syscall_entry
.type syscall_entry, @function

syscall_entry:
    /*
     * Reload DS/ES/FS/GS with the kernel data selector.
     *
     * See the file header for why this is the first thing the
     * stub does. EAX is preserved across the reload.
     */
    pushl %eax
    movw $0x10, %ax
    movw %ax, %ds
    movw %ax, %es
    movw %ax, %fs
    movw %ax, %gs
    popl %eax

    /*
     * Save the general-purpose registers. After this, the trap
     * frame the CPU pushed is at `ESP + 32`, and the register
     * block begins at `ESP + 0`.
     */
    pushal

    /*
     * Pass the address of the frame to the Rust handler.
     *
     * The frame starts at `ESP + 32`, which points at the EIP
     * the CPU pushed. The register block at `ESP + 0` is part of
     * the same structure; `SyscallFrame` in `cpu/syscall.rs`
     * describes the whole thing.
     */
    movl %esp, %eax
    pushl %eax
    call syscall_dispatch
    addl $4, %esp

    /*
     * Marker 'A' — reached if the handler returned normally.
     *
     * EAX is preserved across the marker write. At this point
     * EAX still holds whatever the handler left in it; the real
     * return value is in the register block at `ESP + 28`, which
     * `popal` will restore shortly.
     */
    pushl %eax
    movb $'A', %al
    outb %al, $0xE9
    popl %eax

    /*
     * Restore the general-purpose registers. The handler has
     * already written the return value into the saved-EAX slot
     * (`ESP + 28`), so `popal` restores it into EAX.
     */
    popal

    /*
     * Marker 'B' — reached if `popal` completed and the stack is
     * now pointing at the trap frame's EIP.
     *
     * EAX is preserved. At this point EAX holds the handler's
     * return value; the marker write must not clobber it, or the
     * caller would see a corrupt return value after `iret`.
     */
    pushl %eax
    movb $'B', %al
    outb %al, $0xE9
    popl %eax

    /*
     * Return to the caller. `iret` pops the trap frame:
     *
     *   EIP, CS, EFLAGS       (both origins)
     *   ESP, SS               (CPL 3 origin only)
     *
     * From CPL 3, five dwords are popped and the CPU returns to
     * user mode. From CPL 0, three dwords are popped and the CPU
     * returns to the interrupted kernel code.
     */
    iret