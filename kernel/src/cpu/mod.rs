//! CPU-level infrastructure.
//!
//! Modules here deal with the processor itself: descriptor tables,
//! control registers, interrupt controllers, timers, and the
//! mechanisms that let the kernel run on the hardware.

pub mod control;
pub mod exception;
pub mod gdt;
pub mod idt;
pub mod interrupts;
pub mod msr;
pub mod pic;
pub mod pit;