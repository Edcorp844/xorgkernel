//! CPU-level infrastructure.
//!
//! Modules here deal with the processor itself: descriptor tables,
//! control registers, interrupt controllers, timers, and the
//! mechanisms that let the kernel run on the hardware.
//!
//! # Speculation barriers
//!
//! The [`speculate`] module provides
//! [`speculate::capability_barrier`], a serializing instruction
//! that prevents the processor from speculating past a capability
//! check. It is called by the fabric's public check methods; see
//! the module documentation for the placement rules.

pub mod control;
pub mod exception;
pub mod gdt;
pub mod idt;
pub mod interrupts;
pub mod msr;
pub mod pic;
pub mod pit;
pub mod speculate;