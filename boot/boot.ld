/*
 * Bootloader linker script.
 *
 * The BIOS loads the boot sector at physical address 0x7C00 and
 * jumps to it in 16-bit real mode. This script produces a single
 * 512-byte sector ending with the standard 0xAA55 boot signature.
 *
 * Only one object file (boot.S) contributes to this image, so the
 * layout is straightforward:
 *
 *   0x7C00   .text + .rodata     code and constants
 *            .data               initialized data
 *            .bss                uninitialized data (not loaded)
 *   0x7DFE   .signature          0xAA55
 *
 * The .bss section is declared NOBITS by convention but is listed
 * here explicitly so that the linker does not merge it into other
 * sections. Since the BIOS jumps directly to the start of the
 * sector without zeroing anything, the bootloader itself must not
 * rely on .bss being zero-initialized.
 */

OUTPUT_FORMAT(elf32-i386)
OUTPUT_ARCH(i386)

ENTRY(_start)

SECTIONS
{
    /*
     * BIOS loads the boot sector at physical address 0x7C00.
     */
    . = 0x7C00;

    /*
     * Code and read-only data.
     *
     * Boot code is tiny and does not distinguish between
     * executable and constant sections, so they share one region.
     */
    .text : ALIGN(1)
    {
        *(.text*)
        *(.rodata*)
    }

    /*
     * Initialized data.
     *
     * Contains boot_drive, load_segment, current_lba,
     * current_sector, the GDT, the disk address packet, and the
     * GDT descriptor. All of these are written before use, so
     * their initial values are not significant.
     */
    .data : ALIGN(1)
    {
        *(.data*)
    }

    /*
     * Uninitialized data.
     *
     * The boot sector does not include a zeroing pass, so anything
     * the bootloader needs zero-initialized must be initialized
     * explicitly by code.
     */
    .bss : ALIGN(1)
    {
        *(.bss*)
    }

    /*
     * The boot signature must occupy bytes 510-511 of the
     * 512-byte sector.
     *
     * 0x7C00 + 510 = 0x7DFE
     */
    . = 0x7DFE;

    .signature :
    {
        SHORT(0xAA55);
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
}/*
 * Bootloader linker script.
 *
 * The BIOS loads the boot sector at physical address 0x7C00 and
 * jumps to it in 16-bit real mode. This script produces a single
 * 512-byte sector ending with the standard 0xAA55 boot signature.
 *
 * Only one object file (boot.S) contributes to this image, so the
 * layout is straightforward:
 *
 *   0x7C00   .text + .rodata     code and constants
 *            .data               initialized data
 *            .bss                uninitialized data (not loaded)
 *   0x7DFE   .signature          0xAA55
 *
 * The .bss section is declared NOBITS by convention but is listed
 * here explicitly so that the linker does not merge it into other
 * sections. Since the BIOS jumps directly to the start of the
 * sector without zeroing anything, the bootloader itself must not
 * rely on .bss being zero-initialized.
 */

OUTPUT_FORMAT(elf32-i386)
OUTPUT_ARCH(i386)

ENTRY(_start)

SECTIONS
{
    /*
     * BIOS loads the boot sector at physical address 0x7C00.
     */
    . = 0x7C00;

    /*
     * Code and read-only data.
     *
     * Boot code is tiny and does not distinguish between
     * executable and constant sections, so they share one region.
     */
    .text : ALIGN(1)
    {
        *(.text*)
        *(.rodata*)
    }

    /*
     * Initialized data.
     *
     * Contains boot_drive, load_segment, current_lba,
     * current_sector, the GDT, the disk address packet, and the
     * GDT descriptor. All of these are written before use, so
     * their initial values are not significant.
     */
    .data : ALIGN(1)
    {
        *(.data*)
    }

    /*
     * Uninitialized data.
     *
     * The boot sector does not include a zeroing pass, so anything
     * the bootloader needs zero-initialized must be initialized
     * explicitly by code.
     */
    .bss : ALIGN(1)
    {
        *(.bss*)
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