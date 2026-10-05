/*
 * Kernel linker script.
 *
 * The kernel is loaded by the bootloader at physical address 0x8000
 * and executed in place. Paging is initially disabled, so the
 * addresses in this script are both virtual and physical.
 *
 * # Layout
 *
 *   __kernel_start
 *     .text                     executable code
 *     .rodata                   read-only data
 *     .data                     initialized data
 *     .got                      Global Offset Table (see note below)
 *   __bootstrap_start
 *     .page_directory           bootstrap page directory
 *     .page_table               bootstrap page table
 *     .idt                      Interrupt Descriptor Table
 *     .gdt                      Global Descriptor Table
 *     .frame_bitmap             physical frame allocator bitmap
 *     .bss                      zero-initialized statics
 *   __kernel_end
 *     .bootstrap_stack          kernel bootstrap stack
 *   __bootstrap_stack_top
 *
 * # Bootstrap regions
 *
 * Everything between __bootstrap_start and __kernel_end is
 * declared NOLOAD: it occupies address space but is not part of
 * the disk image that the bootloader reads from the drive. The
 * kernel zeroes the whole range at boot (see
 * `clear_bootstrap_regions` in main.rs) before first use.
 *
 * # Why the GOT is outside the bootstrap range
 *
 * The kernel is linked as position-independent code, so globals
 * are reached through the Global Offset Table. The GOT is emitted
 * by the linker after .data. If it fell inside
 * [__bootstrap_start, __kernel_end), zeroing that range at boot
 * would wipe the GOT, and every later access to a static would
 * read zero. This is why the GOT is given its own region and
 * __bootstrap_start is placed after it.
 *
 * # Why the stack is above __kernel_end
 *
 * The bootstrap stack must not be zeroed by clear_bootstrap_regions,
 * because that function runs *on* the stack. Placing the stack
 * above __kernel_end keeps it outside the zeroed range.
 */

ENTRY(_start)

SECTIONS
{
    . = 0x8000;

    __kernel_start = .;

    /*
     * Executable code.
     *
     * The entry shim (from entry.S / global_asm!) is placed first
     * so that its symbol address is the lowest in the image, which
     * matches what a bootloader typically expects for a kernel
     * entry point.
     */
    .text : ALIGN(16)
    {
        KEEP(*(.text._start))
        *(.text*)
    }

    /*
     * Read-only data: string literals, constant tables, etc.
     */
    .rodata : ALIGN(16)
    {
        *(.rodata*)
    }

    /*
     * Initialized data: statics whose initial value is nonzero.
     *
     * This is where Rust places statics with a non-zero `const`
     * initializer, such as CAPABILITY_CORE.
     */
    .data : ALIGN(16)
    {
        *(.data*)
    }

    /*
     * Global Offset Table.
     *
     * Populated by the linker at link time. Read-only after that.
     * Must not be zeroed at runtime, so it sits between .data and
     * __bootstrap_start.
     *
     * `*(.got.plt)` is included defensively; without a dynamic
     * linker there should be no PLT, but a stray section would
     * otherwise land in the bootstrap range.
     */
    .got : ALIGN(16)
    {
        *(.got*)
    }

    /*
     * Everything from here to __kernel_end is statically placed
     * bootstrap state. It is zeroed at boot.
     */
    __bootstrap_start = .;

    /*
     * Bootstrap page directory.
     *
     * Used during the first phase of paging setup. Must be page
     * aligned: the CPU's page-walker requires the low 12 bits of
     * the CR3 value to be zero.
     */
    .page_directory (NOLOAD) : ALIGN(0x1000)
    {
        __page_directory_start = .;
        . += 0x1000;
        __page_directory_end = .;
    }

    /*
     * Bootstrap page table.
     *
     * The initial identity mapping lives here: 1024 entries
     * covering the first 4 MiB of physical memory. Must be page
     * aligned for the same reason as the page directory.
     */
    .page_table (NOLOAD) : ALIGN(0x1000)
    {
        __page_table_start = .;
        . += 0x1000;
        __page_table_end = .;
    }

    /*
     * Interrupt Descriptor Table.
     *
     * 256 entries, 8 bytes each = 2048 bytes. Aligned to 16 so
     * that entries do not straddle cache lines unnecessarily.
     */
    .idt (NOLOAD) : ALIGN(16)
    {
        __idt_start = .;
        . += 2048;
        __idt_end = .;
    }

    /*
     * Global Descriptor Table.
     *
     * Three entries: null, kernel code, kernel data. 24 bytes total.
     * Aligned to 16 for consistency with the IDT.
     */
    .gdt (NOLOAD) : ALIGN(16)
    {
        __gdt_start = .;
        . += 24;
        __gdt_end = .;
    }

    /*
     * Physical frame allocator bitmap.
     *
     * One bit per 4 KiB frame, covering the 4 GiB 32-bit physical
     * address space:
     *
     *     4 GiB / 4 KiB          = 1 MiB frames
     *     1 MiB frames / 8       = 128 KiB bytes
     *
     * Must be page aligned so that range reservations map cleanly
     * onto frames.
     */
    .frame_bitmap (NOLOAD) : ALIGN(0x1000)
    {
        __frame_bitmap_start = .;
        . += 128 * 1024;
        __frame_bitmap_end = .;
    }

    /*
     * Zero-initialized statics.
     *
     * Rust statics whose initial value is all zeros land here.
     * This includes things like CONSOLE and FRAME_ALLOCATOR that
     * are conceptually bootstrap state but are written in Rust
     * rather than declared as linker regions.
     */
    .bss : ALIGN(0x1000)
    {
        __bss_start = .;
        *(.bss*)
        *(COMMON)
        __bss_end = .;
    }

    /*
     * End of the zeroed bootstrap range.
     *
     * Everything from __bootstrap_start to here is wiped at boot
     * by clear_bootstrap_regions().
     */
    __kernel_end = .;

    /*
     * Bootstrap stack.
     *
     * Deliberately placed AFTER __kernel_end so it is not wiped
     * by clear_bootstrap_regions(), which runs on this stack.
     *
     * ESP is set to __bootstrap_stack_top by the assembly entry
     * shim before kernel_main is called.
     */
    .bootstrap_stack (NOLOAD) : ALIGN(0x1000)
    {
        __bootstrap_stack_bottom = .;
        . += 16 * 1024;
        __bootstrap_stack_top = .;
    }

    /*
     * Sections we do not want in the final image.
     */
    /DISCARD/ :
    {
        *(.comment)
        *(.note*)
        *(.eh_frame*)
    }
}