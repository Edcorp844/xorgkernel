/*
 * Context switching.
 *
 * The kernel runs one task at a time. A task is a kernel
 * execution context: a saved register state, a kernel stack, and
 * the bookkeeping the scheduler needs to decide when the task
 * runs next.
 *
 * `switch_context` performs the register save and stack switch
 * that moves execution from one task to another. It is called
 * only by the scheduler, never directly by tasks.
 *
 * # Calling convention
 *
 *     void switch_context(Task *from, const Task *to);
 *
 * `from` is the task that is currently running. Its `esp` field
 * is where the routine saves the current register state.
 *
 * `to` is the task that should run next. Its `esp` field points
 * at the saved register state to restore.
 *
 * The function does not return to the caller. It returns into
 * whichever task `to` refers to, and execution continues from
 * that task's saved instruction pointer.
 *
 * # Register set
 *
 * Only the callee-saved registers (EBX, ESI, EDI, EBP) are saved
 * and restored. Caller-saved registers are already dead at the
 * call site; saving them would be wasted work.
 *
 * EAX and EDX are used as scratch and are not saved: EAX holds
 * `from`, EDX holds `to`, and both are consumed before the
 * switch.
 *
 * # Stack layout on entry
 *
 *     ESP + 0   return address (pushed by `call`)
 *     ESP + 4   from
 *     ESP + 8   to
 *
 * # Stack layout of a saved task
 *
 * After `switch_context` saves a task, the task's `esp` points
 * at:
 *
 *     ESP + 0   saved EBX
 *     ESP + 4   saved ESI
 *     ESP + 8   saved EDI
 *     ESP + 12  saved EBP
 *     ESP + 16  return address of the call that triggered the
 *               switch (or the task's entry point on first run)
 *
 * # First run of a task
 *
 * When `Task::create` builds a task, it lays out an initial
 * frame that matches the layout above:
 *
 *     ESP + 0   EBX = 0
 *     ESP + 4   ESI = 0
 *     ESP + 8   EDI = 0
 *     ESP + 12  EBP = 0
 *     ESP + 16  entry point
 *
 * The first time `switch_context` restores this task, it pops
 * four zeros and executes `ret`, which jumps to the entry point.
 * The task starts running with a clean stack.
 */

.code32

.section .text

.global switch_context

switch_context:
    /*
     * Load the two task pointers into scratch registers.
     */
    movl 4(%esp), %eax          /* from */
    movl 8(%esp), %edx          /* to */

    /*
     * Save the current task's callee-saved registers on its
     * kernel stack.
     */
    pushl %ebp
    pushl %edi
    pushl %esi
    pushl %ebx

    /*
     * Save the resulting stack pointer into from->esp.
     *
     * `esp` is at offset 0 of `Task`.
     */
    movl %esp, 0(%eax)

    /*
     * Load the next task's saved stack pointer into ESP.
     */
    movl 0(%edx), %esp

    /*
     * Restore the next task's callee-saved registers.
     */
    popl %ebx
    popl %esi
    popl %edi
    popl %ebp

    /*
     * Return into the next task. The return address was pushed
     * onto the new stack when the task was saved (or, on first
     * run, when `Task::create` laid out the initial frame).
     */
    ret