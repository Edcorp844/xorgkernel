//! Speculation barriers.
//!
//! Modern x86 processors execute instructions out of order and
//! speculate past branches and bounds checks. Speculation is
//! architecturally invisible: a speculatively executed instruction
//! either commits (if the speculation was correct) or is squashed
//! (if not), with no architectural state change on the squash
//! path.
//!
//! But speculation is not *microarchitecturally* invisible. A
//! speculatively executed load can pull data into the cache even
//! if the load is later squashed, and the cache state persists
//! after the squash. An attacker who can measure the cache can
//! recover information about the speculative load's target, even
//! though the load was architecturally forbidden. This is the
//! class of attack that includes Meltdown and Spectre.
//!
//! # What this module provides
//!
//! [`capability_barrier`] is a serializing instruction that
//! prevents the processor from speculating past a capability
//! check. Placed after the check and before the operation that
//! depends on it, it ensures that a capability failure is
//! architecturally committed before any subsequent instruction is
//! speculatively executed, so no speculative load can leak data
//! the check was supposed to protect.
//!
//! # Where barriers are needed
//!
//! The fabric inserts a barrier after every public capability
//! check:
//!
//! - [`CapabilityCore::lookup`] — resolves a capability ID to a
//!   capability.
//! - [`CapabilityCore::has_rights`] — tests whether a capability
//!   carries a given right.
//! - [`CapabilityCore::object`] — resolves a capability to its
//!   object.
//!
//! Those are the operations whose result a subsequent load or
//! store might depend on. Callers of the fabric do not need to
//! place barriers themselves; the fabric's own methods do it.
//!
//! The barrier is placed only after checks that *succeed* — a
//! failed check returns immediately and the caller's error path
//! is not speculative-attack-relevant, because no data was going
//! to be accessed anyway.
//!
//! # Cost
//!
//! On modern Intel and AMD processors, `lfence` is a
//! dispatch-serializing instruction with a cost of a few cycles.
//! It is not free, but it is small relative to a syscall boundary
//! or a context switch, which is where capability checks are
//! likely to appear.
//!
//! The cost is paid per check, not per operation. A syscall that
//! performs three capability checks pays three barriers; a
//! syscall that performs none pays none. The barrier is on the
//! check path, not on the fabric's internal paths, so the
//! fabric's own bookkeeping (cell lookups, ITable scans,
//! registry lookups) does not pay for it.
//!
//! # Portability
//!
//! The barrier is `lfence` on x86 and x86_64. On other
//! architectures, it falls back to a sequentially-consistent
//! atomic fence, which is the strongest barrier the language
//! offers and is a correct (if conservative) substitute for an
//! architecture-specific serializing instruction.
//!
//! The kernel currently targets x86 only. The fallback exists so
//! that the module compiles on any target, which makes
//! cross-compilation and static analysis easier.
//!
//! # Placement discipline
//!
//! Adding a barrier is not free, and adding one in the wrong
//! place is worse than useless: it can mislead a reader into
//! believing a check is protected when it is not. A barrier
//! belongs at the exit of a public method whose result the caller
//! will use to decide whether to access an object. It does not
//! belong inside the fabric's internal helper methods, because
//! those are called by the public methods and the barrier at the
//! public method's exit covers them.
//!
//! The rule of thumb: if a method returns something the caller
//! will use as a precondition for a memory access, and that
//! method can fail to resolve a capability, put a barrier at its
//! exit.
//!
//! [`CapabilityCore::lookup`]:
//!     crate::capability::core::CapabilityCore::lookup
//! [`CapabilityCore::has_rights`]:
//!     crate::capability::core::CapabilityCore::has_rights
//! [`CapabilityCore::object`]:
//!     crate::capability::core::CapabilityCore::object

use core::sync::atomic::{fence, Ordering};

/// Prevents the processor from speculating past a capability
/// check.
///
/// Placed after a check that has succeeded and before the
/// operation the check protects. See the module documentation for
/// the rationale and the placement rules.
///
/// The function is `#[inline(always)]` so that every call site
/// compiles to a single `lfence` instruction (on x86) or a single
/// fence instruction (on other targets). No function call
/// overhead is paid on the hot path.
///
/// # Ordering guarantee
///
/// On x86, `lfence` is dispatch-serializing: no instruction
/// after the `lfence` is dispatched until every instruction
/// before the `lfence` has been dispatched. The load of a
/// capability's contents (which happens during the check) and
/// the load of the object's data (which happens after the check)
/// are therefore ordered, and the second cannot speculatively
/// execute before the first commits.
///
/// This is stronger than a compiler fence and stronger than an
/// acquire fence. It is the specific guarantee the paper's §4.2
/// requires.
#[inline(always)]
pub fn capability_barrier() {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        // SAFETY: `lfence` is architecturally defined on every
        // x86 processor since the Pentium 4. It is a
        // dispatch-serializing instruction that prevents
        // speculative loads from being reordered across it. It
        // does not touch memory or flags, and it cannot fault.
        //
        // The `nostack` option tells the compiler the instruction
        // does not use the stack, so it does not need to preserve
        // the red zone. The `preserves_flags` option tells the
        // compiler the instruction does not modify EFLAGS, so it
        // does not need to save and restore them.
        unsafe {
            core::arch::asm!("lfence", options(nostack, preserves_flags));
        }
    }

    #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
    {
        // Fall back to a sequentially-consistent atomic fence.
        //
        // This is the strongest barrier the Rust memory model
        // offers, and it is a correct substitute for an
        // architecture-specific serializing instruction, though
        // it may be stronger (and therefore slower) than
        // necessary for the specific attack this barrier is
        // meant to prevent.
        //
        // The kernel currently targets x86 only, so this branch
        // is not exercised. It exists so that the module compiles
        // on any target.
        fence(Ordering::SeqCst);
    }
}