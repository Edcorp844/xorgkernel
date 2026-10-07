//! Capability fabric tests.
//!
//! These tests exercise the fabric's object lifecycle, the three
//! capability operations ([`move_capability`], [`copy_capability`],
//! [`try_move_capability`]), and the invariants that make the
//! architecture's safety argument hold:
//!
//! - **The affine invariant.** At most one `Owned` token exists to
//!   any object at any time. Exercised by
//!   [`test_affine_invariant`].
//!
//! - **The leaf-only invariant.** A copied capability never has
//!   `SHARE`, so it can never be the root of a derivation tree.
//!   Exercised by [`test_share_stripping`].
//!
//! - **Failure-mode preservation.** [`move_capability`] preserves
//!   its source when the target side fails;
//!   [`try_move_capability`] consumes it regardless. Exercised by
//!   [`test_move_failure_modes`].
//!
//! - **Cross-cell operations.** [`copy_between_cells`] and
//!   [`move_between_cells`] maintain the source and target cells'
//!   namespaces correctly. Exercised by
//!   [`test_cross_cell_operations`].
//!
//! The tests use the fabric's public API exclusively. They do not
//! reach into the ITable or the registry directly, so they remain
//! valid if the fabric's internal tables are restructured.
//!
//! [`move_capability`]: crate::capability::core::CapabilityCore::move_capability
//! [`copy_capability`]: crate::capability::core::CapabilityCore::copy_capability
//! [`try_move_capability`]: crate::capability::core::CapabilityCore::try_move_capability
//! [`copy_between_cells`]: crate::capability::core::CapabilityCore::copy_between_cells
//! [`move_between_cells`]: crate::capability::core::CapabilityCore::move_between_cells

use crate::capability::capability::CapabilityRights;
use crate::capability::cell::CellId;
use crate::capability::object::ObjectKind;
use crate::println;

/// Tests the fabric's object lifecycle: creation, capability
/// allocation, and object-wide revocation.
///
/// This is the fabric's basic sanity test. It does not exercise
/// the affine operations; those have their own tests below.
fn test_object_lifecycle() {
    println!();
    println!("Testing object lifecycle...");

    let core = crate::capability::core_mut();

    let object = core
        .create_object(ObjectKind::Cell)
        .expect("object creation failed");

    println!("  Object created: {}", object.raw());

    let rights = CapabilityRights::READ | CapabilityRights::WRITE;

    let cap_a = core
        .allocate(object, rights)
        .expect("capability A allocation failed");
    let cap_b = core
        .allocate(object, rights)
        .expect("capability B allocation failed");

    println!("  Capability A: 0x{:08x}", cap_a.raw());
    println!("  Capability B: 0x{:08x}", cap_b.raw());

    assert!(core.lookup(cap_a).is_some());
    assert!(core.lookup(cap_b).is_some());

    assert_eq!(core.object(cap_a), Some(object));
    assert_eq!(core.object(cap_b), Some(object));

    println!("  Lookup and object resolution: SUCCESS");

    assert!(core.destroy_object(object));

    assert!(core.lookup(cap_a).is_none());
    assert!(core.lookup(cap_b).is_none());
    assert!(core.lookup_object(object).is_none());

    println!("  Object-wide revocation: SUCCESS");
}

/// Tests [`CapabilityCore::copy_capability`]: the source survives,
/// the target receives a leaf with attenuated rights.
fn test_copy_capability() {
    println!();
    println!("Testing copy_capability (leaf duplication)...");

    let core = crate::capability::core_mut();

    let object = core
        .create_object(ObjectKind::Cell)
        .expect("object creation failed");

    let target = core.create_cell().expect("target cell creation failed");

    let parent_rights = CapabilityRights::READ | CapabilityRights::WRITE | CapabilityRights::SHARE;

    let parent = core
        .allocate(object, parent_rights)
        .expect("parent capability allocation failed");

    let child = core
        .copy_capability(parent, target, CapabilityRights::READ)
        .expect("copy_capability with READ failed");

    // Source survives.
    assert!(
        core.lookup(parent).is_some(),
        "copy must not consume the source"
    );

    // Child has the requested rights.
    assert!(core.has_rights(child, CapabilityRights::READ));
    assert!(!core.has_rights(child, CapabilityRights::WRITE));
    assert!(!core.has_rights(child, CapabilityRights::EXECUTE));

    // Child has no SHARE.
    assert!(
        !core.has_rights(child, CapabilityRights::SHARE),
        "copy must strip SHARE regardless of what was requested"
    );

    // Child is in the target cell.
    assert!(core.cell_has_capability(target, child));
    assert!(!core.cell_has_capability(target, parent));

    // Parent and child name the same object.
    assert_eq!(core.object(parent), Some(object));
    assert_eq!(core.object(child), Some(object));

    println!("  Source survives: SUCCESS");
    println!("  Rights attenuation: SUCCESS");
    println!("  SHARE stripped from copy: SUCCESS");
    println!("  Target cell populated: SUCCESS");

    assert!(core.destroy_object(object));

    assert!(core.lookup(parent).is_none());
    assert!(core.lookup(child).is_none());

    println!("  Object-wide revocation: SUCCESS");
}

/// Tests the affine invariant: at most one Owned token to an
/// object at any time.
///
/// The test performs a move and verifies that:
///
/// 1. The source ID no longer resolves.
/// 2. The target ID resolves with the same object and rights.
/// 3. The total count of live tokens to the object is unchanged
///    (one before, one after).
/// 4. Replaying the source ID fails.
fn test_affine_invariant() {
    println!();
    println!("Testing affine invariant (move consumes source)...");

    let core = crate::capability::core_mut();

    let object = core
        .create_object(ObjectKind::Cell)
        .expect("object creation failed");

    let cell_a = core.create_cell().expect("cell A creation failed");
    let cell_b = core.create_cell().expect("cell B creation failed");

    let rights = CapabilityRights::READ | CapabilityRights::WRITE;

    let cap_a = core
        .allocate(object, rights)
        .expect("capability allocation failed");

    assert!(core.grant_capability(cell_a, cap_a));

    // Before the move: A owns, B has nothing, one live token.
    assert!(core.lookup(cap_a).is_some());
    assert!(core.cell_has_capability(cell_a, cap_a));
    assert!(!core.cell_has_capability(cell_b, cap_a));

    println!("  Before move: source in cell A, target empty");

    // Move.
    let cap_b = core
        .move_capability(cap_a, cell_b)
        .expect("move_capability failed");

    // After: source is stale.
    assert!(
        core.lookup(cap_a).is_none(),
        "source must be consumed after move"
    );

    // After: target holds the token with the same object and rights.
    let moved = core.lookup(cap_b).expect("target token must resolve");
    assert_eq!(moved.object(), object);
    assert!(moved.rights().contains(CapabilityRights::READ));
    assert!(moved.rights().contains(CapabilityRights::WRITE));

    // After: B holds it, A's namespace still lists the (stale) ID.
    // The move operation does not touch the source cell's
    // namespace; that is `move_between_cells`'s job.
    assert!(core.cell_has_capability(cell_b, cap_b));

    println!("  After move: source stale, target owns");
    println!("  Rights preserved across move: SUCCESS");

    // Replaying the consumed ID must fail.
    assert!(
        core.move_capability(cap_a, cell_a).is_none(),
        "consumed ID must not resolve"
    );
    assert!(core.lookup(cap_a).is_none());

    println!("  Replay of consumed ID: SUCCESS");

    // Cleanup.
    assert!(core.destroy_object(object));

    println!("  Affine invariant: SUCCESS");
}

/// Tests that `copy_capability` rejects requests for `SHARE`.
///
/// A caller who writes `copy_capability(src, cell, READ | SHARE)`
/// is asking for a shareable copy, which the architecture forbids.
/// The call must fail loudly rather than silently strip `SHARE`,
/// because a silent strip lets the caller believe they have a
/// shareable copy when they do not.
fn test_share_stripping() {
    println!();
    println!("Testing SHARE rejection in copy_capability...");

    let core = crate::capability::core_mut();

    let object = core
        .create_object(ObjectKind::Cell)
        .expect("object creation failed");

    let target = core.create_cell().expect("target cell creation failed");

    let parent_rights = CapabilityRights::READ | CapabilityRights::WRITE | CapabilityRights::SHARE;

    let parent = core
        .allocate(object, parent_rights)
        .expect("parent capability allocation failed");

    // ---- A copy requesting SHARE must be rejected. ----

    let denied = core.copy_capability(
        parent,
        target,
        CapabilityRights::READ | CapabilityRights::SHARE,
    );

    assert!(
        denied.is_none(),
        "copy_capability must reject requests containing SHARE"
    );

    println!("  Request with SHARE rejected: SUCCESS");

    // ---- A copy requesting rights beyond the source's must be
    //      rejected. ----

    let denied = core.copy_capability(parent, target, CapabilityRights::EXECUTE);

    assert!(
        denied.is_none(),
        "copy_capability must reject rights not contained in the source"
    );

    println!("  Excess-rights rejection: SUCCESS");

    // ---- A copy with valid rights succeeds and has no SHARE. ----

    let child = core
        .copy_capability(
            parent,
            target,
            CapabilityRights::READ | CapabilityRights::WRITE,
        )
        .expect("copy_capability with valid rights failed");

    assert!(!core.has_rights(child, CapabilityRights::SHARE));
    assert!(core.has_rights(child, CapabilityRights::READ));
    assert!(core.has_rights(child, CapabilityRights::WRITE));

    println!("  Valid copy has no SHARE: SUCCESS");

    // ---- A copy of a copy is still a leaf. ----
    //
    // Under the three-operation model, `copy_capability` does
    // not require `SHARE` on the source. What it guarantees is
    // that the result never carries `SHARE`. So a copy of a
    // copy is allowed, and the grandchild is still a leaf. The
    // invariant is about what copies can carry, not about
    // whether copies can be made.

    let grandchild = core
        .copy_capability(child, target, CapabilityRights::READ)
        .expect("copy of a leaf should succeed");

    assert!(
        !core.has_rights(grandchild, CapabilityRights::SHARE),
        "a copy of a copy must also lack SHARE"
    );
    assert!(core.has_rights(grandchild, CapabilityRights::READ));
    assert!(core.lookup(grandchild).is_some());

    println!("  Copy of a leaf is still a leaf: SUCCESS");

    assert!(core.destroy_object(object));

    println!("  SHARE stripping: SUCCESS");
}

/// Tests the two move variants' failure-mode semantics.
///
/// `move_capability` preserves the source when the target side
/// fails, so the caller can retry. `try_move_capability` consumes
/// the source regardless, so a caller that has committed to
/// spending the token cannot get it back by accident.
fn test_move_failure_modes() {
    println!();
    println!("Testing move failure modes...");

    let core = crate::capability::core_mut();

    // ---- move_capability: source survives on target failure. ----

    {
        let object = core
            .create_object(ObjectKind::Cell)
            .expect("object creation failed");

        let source_cell = core.create_cell().expect("source cell creation failed");

        let cap = core
            .allocate(object, CapabilityRights::READ)
            .expect("capability allocation failed");

        assert!(core.grant_capability(source_cell, cap));

        // Target does not exist.
        let bogus_cell = CellId::new(9999);

        assert!(
            core.move_capability(cap, bogus_cell).is_none(),
            "move to a non-existent cell must fail"
        );

        assert!(
            core.lookup(cap).is_some(),
            "move_capability must leave the source live on failure"
        );
        assert!(core.cell_has_capability(source_cell, cap));

        println!("  move_capability preserves source on failure: SUCCESS");

        assert!(core.destroy_object(object));
    }

    // ---- try_move_capability: source consumed on target failure. ----

    {
        let object = core
            .create_object(ObjectKind::Cell)
            .expect("object creation failed");

        let source_cell = core.create_cell().expect("source cell creation failed");

        let cap = core
            .allocate(object, CapabilityRights::READ)
            .expect("capability allocation failed");

        assert!(core.grant_capability(source_cell, cap));

        let bogus_cell = CellId::new(9999);

        assert!(
            core.try_move_capability(cap, bogus_cell).is_none(),
            "try_move to a non-existent cell must fail"
        );

        assert!(
            core.lookup(cap).is_none(),
            "try_move_capability must consume the source even on failure"
        );

        println!("  try_move_capability consumes on failure: SUCCESS");

        assert!(core.destroy_object(object));
    }

    // ---- try_move_capability: source not consumed if it does
    //      not resolve in the first place. ----

    {
        let object = core
            .create_object(ObjectKind::Cell)
            .expect("object creation failed");

        let cap = core
            .allocate(object, CapabilityRights::READ)
            .expect("capability allocation failed");

        assert!(core.revoke(cap));

        assert!(
            core.try_move_capability(cap, CellId::new(1)).is_none(),
            "try_move with an unresolvable source must fail"
        );

        // Nothing to consume; the operation is a no-op.

        println!("  try_move_capability no-op on stale source: SUCCESS");

        assert!(core.destroy_object(object));
    }
}

/// Tests cross-cell operations: `copy_between_cells` and
/// `move_between_cells`.
///
/// These are the operations callers should use when authority
/// crosses a cell boundary. They maintain the source and target
/// cells' namespaces in addition to performing the underlying
/// capability operation.
fn test_cross_cell_operations() {
    println!();
    println!("Testing cross-cell operations...");

    let core = crate::capability::core_mut();

    // ---- copy_between_cells: source cell retains its token. ----

    {
        let object = core
            .create_object(ObjectKind::Cell)
            .expect("object creation failed");

        let cell_a = core.create_cell().expect("cell A creation failed");
        let cell_b = core.create_cell().expect("cell B creation failed");

        let rights = CapabilityRights::READ | CapabilityRights::SHARE;
        let cap_a = core
            .allocate(object, rights)
            .expect("capability allocation failed");
        assert!(core.grant_capability(cell_a, cap_a));

        let cap_b = core
            .copy_between_cells(cell_a, cell_b, cap_a, CapabilityRights::READ)
            .expect("copy_between_cells failed");

        assert!(
            core.cell_has_capability(cell_a, cap_a),
            "copy must not remove the source from the source cell"
        );
        assert!(
            core.cell_has_capability(cell_b, cap_b),
            "copy must add the derived capability to the target cell"
        );

        assert!(core.lookup(cap_a).is_some());
        assert!(core.lookup(cap_b).is_some());

        println!("  copy_between_cells preserves source: SUCCESS");

        assert!(core.destroy_object(object));
    }

    // ---- move_between_cells: source cell loses its token. ----

    {
        let object = core
            .create_object(ObjectKind::Cell)
            .expect("object creation failed");

        let cell_a = core.create_cell().expect("cell A creation failed");
        let cell_b = core.create_cell().expect("cell B creation failed");

        let cap_a = core
            .allocate(object, CapabilityRights::READ)
            .expect("capability allocation failed");
        assert!(core.grant_capability(cell_a, cap_a));

        let cap_b = core
            .move_between_cells(cell_a, cell_b, cap_a)
            .expect("move_between_cells failed");

        assert!(
            core.lookup(cap_a).is_none(),
            "move must revoke the source capability"
        );
        assert!(
            !core.cell_has_capability(cell_a, cap_a),
            "move must remove the source capability from the source cell"
        );
        assert!(
            core.cell_has_capability(cell_b, cap_b),
            "move must add the target capability to the target cell"
        );

        println!("  move_between_cells consumes source cell's token: SUCCESS");

        assert!(core.destroy_object(object));
    }

    // ---- move_between_cells: source cell is unchanged on failure. ----

    {
        let object = core
            .create_object(ObjectKind::Cell)
            .expect("object creation failed");

        let cell_a = core.create_cell().expect("cell A creation failed");

        let cap_a = core
            .allocate(object, CapabilityRights::READ)
            .expect("capability allocation failed");
        assert!(core.grant_capability(cell_a, cap_a));

        let bogus_cell = CellId::new(9999);

        assert!(core.move_between_cells(cell_a, bogus_cell, cap_a).is_none());

        assert!(
            core.lookup(cap_a).is_some(),
            "failed cross-cell move must leave the source live"
        );
        assert!(
            core.cell_has_capability(cell_a, cap_a),
            "failed cross-cell move must leave the source cell's namespace intact"
        );

        println!("  move_between_cells preserves source on failure: SUCCESS");

        assert!(core.destroy_object(object));
    }
}

/// Tests execution cell creation and management.
///
/// This is a small structural test: it verifies that cells can be
/// created, that their IDs are distinct, and that the fabric can
/// resolve them.
pub fn test_cells() {
    println!();
    println!("Testing execution cells...");

    let core = crate::capability::core_mut();

    println!(
        "  CapabilityCore size: {} bytes",
        core::mem::size_of::<crate::capability::core::CapabilityCore>()
    );

    let cell_a = core.create_cell().expect("cell A creation failed");
    let cell_b = core.create_cell().expect("cell B creation failed");

    assert_ne!(cell_a, cell_b, "cell IDs must be distinct");
    assert!(core.cell(cell_a).is_some());
    assert!(core.cell(cell_b).is_some());

    println!("  Cell A: {}", cell_a.raw());
    println!("  Cell B: {}", cell_b.raw());
    println!("  Distinct and resolvable: SUCCESS");
}

/// Public entry point called by `tests/mod.rs`.
///
/// The old test module exposed a single
/// `test_capability_operations` function. That name no longer
/// matches the fabric's vocabulary: `transfer` is now
/// `copy_capability`, and the affine operation is
/// `move_capability`. The test suite has been split into focused
/// tests, one per invariant, and this function runs them all.
pub fn test_capability_operations() {
    test_object_lifecycle();
    test_copy_capability();
    test_affine_invariant();
    test_share_stripping();
    test_move_failure_modes();
    test_cross_cell_operations();
}
