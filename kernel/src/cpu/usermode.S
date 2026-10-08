/*
 * Return to user mode.
 *
 * `iret_to_user` is the tail of the kernel's return path to CPL 3.
 * It executes `iret`, which pops the five-dword user frame that
 * `Task::create_user` laid out on the task's kernel stack (or that
 * the CPU pushed on entry to a syscall from CPL 3).
 *
 * # Why a named label
 *
 * Every return to user mode goes through this label:
 *
 * - A task that has never run before: `Task::create_user` places
 *   this label's address in the return-address slot of the
 *   initial frame, so `switch_context`'s `ret` lands here.
 *
 * - A task that trapped into the kernel (syscall, IRQ, exception):
 *   the trap handler's exit sequence uses the CPU-pushed frame on
 *   the kernel stack and jumps here to return.
 *
 * Having a single label means there is exactly one place where
 * the return-to-user instruction lives. When the kernel is ported
 * to x86_64 and the `syscall`/`sysret` fast path is added, this
 * label becomes a small dispatcher:
 *
 *     - Check the fast-path conditions (RIP == RCX, RFLAGS == R11,
 *       RIP canonical, RF clear, CS/SS valid).
 *     - If they hold, `swapgs` and `sysret`.
 *     - Otherwise, fall through to `iret`.
 *
 * The `iret` below is the slow path in both worlds. On x86_64, it
 * is also the only path used for exceptions and interrupts. The
 * port is a change to this file, not to the ABI.
 *
 * # Why `iret` and not `sysexit` on 32-bit
 *
 * `sysexit` is faster but has a documented race: the flags
 * restored by the `popf` immediately before `sysexit` take effect
 * before the instruction itself does, so a task with the Trap
 * Flag set can cause an exception inside the kernel after the
 * return address has been set up. seL4 fixed this by falling back
 * to `iret` for threads with single-stepping enabled. The kernel
 * here uses `iret` unconditionally for the same reason: the
 * performance difference on 32-bit is small, and the security
 * property is not negotiable.
 *
 * # Frame layout expected by `iret`
 *
 * From lowest to highest address on the kernel stack:
 *
 *     +0   user EIP
 *     +4   user CS   (0x18 | 3 == 0x1B)
 *     +8   user EFLAGS (0x202: IF set, reserved bit 1 set)
 *     +12  user ESP
 *     +16  user SS   (0x20 | 3 == 0x23)
 *
 * The five dwords are either laid out by `Task::create_user` (for
 * a task's first run) or pushed by the CPU on trap entry (for a
 * return from a syscall or interrupt).
 */

.code32

.section .text

.global iret_to_user
.type iret_to_user, @function

iret_to_user:
    /*
     * On x86_64, this is where the `swapgs` and the fast-path
     * check will live. On i686, there is nothing to do here: the
     * frame on the stack is exactly what `iret` expects.
     */
    iret