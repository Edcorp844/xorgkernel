use crate::cpu::control;

/// Register state saved by the x86 `pushal` instruction.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Registers {
    pub edi: u32,
    pub esi: u32,
    pub ebp: u32,
    pub original_esp: u32,
    pub ebx: u32,
    pub edx: u32,
    pub ecx: u32,
    pub eax: u32,
}

/// Processor state supplied by an exception entry stub.
///
/// The assembly entry points normalize exceptions so that this structure
/// always contains an explicit error code. Exceptions which do not generate
/// a hardware error code receive an error code of zero.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ExceptionFrame {
    pub error_code: u32,
    pub eip: u32,
    pub cs: u32,
    pub eflags: u32,
}

/// Dispatches a processor exception to the kernel exception reporter.
///
/// The current kernel treats every exception as fatal. The processor is
/// halted after the diagnostic information has been printed.
#[unsafe(no_mangle)]
pub extern "C" fn exception_dispatch(
    vector: u32,
    registers: &Registers,
    frame: &ExceptionFrame,
) -> ! {
    unsafe {
        core::arch::asm!(
            "mov al, 'D'",
            "out 0xE9, al",
            options(nostack, preserves_flags),
        );
    }

    println!();
    println!("========================================");
    println!("           KERNEL EXCEPTION");
    println!("========================================");

    println!("Exception: {}", exception_name(vector));
    println!("Vector:    {}", vector);
    println!("Error:     0x{:08x}", frame.error_code);

    println!();
    println!("Execution state:");
    println!("  EIP:     0x{:08x}", frame.eip);
    println!("  CS:      0x{:08x}", frame.cs);
    println!("  EFLAGS:  0x{:08x}", frame.eflags);

    if vector == 14 {
        println!();
        println!("Page fault:");
        println!("  Reading CR2...");
        let cr2 = control::read_cr2();
        println!("  CR2: 0x{:08x}", cr2);
        print_page_fault_reason(frame.error_code);
    }

    println!();
    println!("Registers:");
    println!("  EAX:     0x{:08x}", registers.eax);
    println!("  EBX:     0x{:08x}", registers.ebx);
    println!("  ECX:     0x{:08x}", registers.ecx);
    println!("  EDX:     0x{:08x}", registers.edx);
    println!("  ESI:     0x{:08x}", registers.esi);
    println!("  EDI:     0x{:08x}", registers.edi);
    println!("  EBP:     0x{:08x}", registers.ebp);
    println!("  ESP:     0x{:08x}", registers.original_esp);

    println!();
    println!("Kernel halted.");

    halt();
}

/// Returns the architectural name of an x86 exception.
fn exception_name(vector: u32) -> &'static str {
    match vector {
        0 => "Divide Error (#DE)",
        1 => "Debug (#DB)",
        2 => "Non-Maskable Interrupt (#NMI)",
        3 => "Breakpoint (#BP)",
        4 => "Overflow (#OF)",
        5 => "Bound Range Exceeded (#BR)",
        6 => "Invalid Opcode (#UD)",
        7 => "Device Not Available (#NM)",
        8 => "Double Fault (#DF)",
        9 => "Coprocessor Segment Overrun",
        10 => "Invalid TSS (#TS)",
        11 => "Segment Not Present (#NP)",
        12 => "Stack-Segment Fault (#SS)",
        13 => "General Protection Fault (#GP)",
        14 => "Page Fault (#PF)",
        15 => "Reserved",
        16 => "x87 Floating-Point (#MF)",
        17 => "Alignment Check (#AC)",
        18 => "Machine Check (#MC)",
        19 => "SIMD Floating-Point (#XM)",
        20 => "Virtualization Exception (#VE)",
        21 => "Control Protection (#CP)",
        22..=29 => "Reserved",
        30 => "Security Exception",
        31 => "Reserved",
        _ => "Unknown Exception",
    }
}

/// Prints the meaning of the page-fault error-code bits.
fn print_page_fault_reason(error: u32) {
    println!();
    println!("Page fault reason:");

    if error_code_has_bit(error, 0) {
        println!("  - Protection violation");
    } else {
        println!("  - Non-present page");
    }

    if error_code_has_bit(error, 1) {
        println!("  - Write access");
    } else {
        println!("  - Read access");
    }

    if error_code_has_bit(error, 2) {
        println!("  - User-mode access");
    } else {
        println!("  - Kernel-mode access");
    }

    if error_code_has_bit(error, 3) {
        println!("  - Reserved bit violation");
    }

    if error_code_has_bit(error, 4) {
        println!("  - Instruction fetch");
    }
}

/// Tests whether an exception error-code bit is set.
const fn error_code_has_bit(error: u32, bit: u32) -> bool {
    (error & (1 << bit)) != 0
}

/// Halts the processor permanently.
fn halt() -> ! {
    unsafe {
        core::arch::asm!("cli", options(nomem, nostack, preserves_flags));
    }

    loop {
        unsafe {
            core::arch::asm!("hlt", options(nomem, nostack, preserves_flags));
        }
    }
}

unsafe extern "C" {
    pub fn exception_entry_0();
    pub fn exception_entry_1();
    pub fn exception_entry_2();
    pub fn exception_entry_3();
    pub fn exception_entry_4();
    pub fn exception_entry_5();
    pub fn exception_entry_6();
    pub fn exception_entry_7();
    pub fn exception_entry_8();
    pub fn exception_entry_9();
    pub fn exception_entry_10();
    pub fn exception_entry_11();
    pub fn exception_entry_12();
    pub fn exception_entry_13();
    pub fn exception_entry_14();
    pub fn exception_entry_15();
    pub fn exception_entry_16();
    pub fn exception_entry_17();
    pub fn exception_entry_18();
    pub fn exception_entry_19();
    pub fn exception_entry_20();
    pub fn exception_entry_21();
    pub fn exception_entry_22();
    pub fn exception_entry_23();
    pub fn exception_entry_24();
    pub fn exception_entry_25();
    pub fn exception_entry_26();
    pub fn exception_entry_27();
    pub fn exception_entry_28();
    pub fn exception_entry_29();
    pub fn exception_entry_30();
    pub fn exception_entry_31();
}
