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
use crate::capability::cell::BorrowState;
use crate::capability::cell::CellId;
use crate::capability::channel::MessageKind;
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

/// Tests IPC channel creation, capability send, and message
/// receive.
///
/// Creates two cells, a channel, and a capability. Cell A holds
/// the capability and sends it over the channel. Cell B receives
/// it. After the send, A no longer holds the capability; after
/// the receive, B holds it in the message it dequeued.
fn test_channel_create_send_recv() {
    println!();
    println!("Testing channel create / send / recv...");

    let core = crate::capability::core_mut();

    let cell_a = core.create_cell().expect("cell A creation failed");
    let cell_b = core.create_cell().expect("cell B creation failed");

    // Create a channel with SEND and RECV rights. A caller that
    // distributes the channel to cells would give SEND to the
    // sender and RECV to the receiver; for this test, both
    // rights live on the single channel capability.
    let channel_rights = CapabilityRights::SEND
        | CapabilityRights::RECV
        | CapabilityRights::SHARE
        | CapabilityRights::DESTROY;

    let (channel_obj, channel_cap) = core
        .create_channel(channel_rights)
        .expect("channel creation failed");

    println!("  Channel object: {}", channel_obj.raw());
    println!("  Channel cap:    0x{:08x}", channel_cap.raw());

    // Create an object and a capability to it.
    let object = core
        .create_object(ObjectKind::Cell)
        .expect("object creation failed");

    let cap = core
        .allocate(object, CapabilityRights::READ)
        .expect("capability allocation failed");

    assert!(core.grant_capability(cell_a, cap));

    // Before send: A holds the capability.
    assert!(core.cell_has_capability(cell_a, cap));
    assert!(core.lookup(cap).is_some());

    // Send A's capability over the channel.
    let sent = core.send_capability(channel_cap, cell_a, cap);

    assert!(sent, "send_capability must succeed");
    assert!(
        !core.cell_has_capability(cell_a, cap),
        "after send, A must no longer hold the capability"
    );
    assert!(
        core.lookup(cap).is_some(),
        "the ITable slot remains live; the channel now holds it"
    );

    println!("  Send: source cell released, ITable slot still live: SUCCESS");

    // Receive into B. The message carries the capability ID.
    let message = core
        .recv_message(channel_cap, cell_b)
        .expect("receive must succeed");

    assert_eq!(message.kind, crate::capability::channel::MessageKind::Move);
    assert_eq!(message.capability, cap);

    println!("  Receive: message carries the capability: SUCCESS");

    // Grant the received capability to B.
    assert!(core.grant_capability(cell_b, message.capability));
    assert!(core.cell_has_capability(cell_b, cap));

    println!("  Receiver adopts the capability: SUCCESS");

    // Cleanup.
    assert!(core.destroy_object(object));
    assert!(core.destroy_object(channel_obj));

    println!("  Channel create / send / recv: SUCCESS");
}

/// Tests the borrow and return path.
///
/// Cell A holds a capability. A borrows it out over a channel.
/// A's slot transitions to `BorrowedOut`. B dequeues the borrow
/// message, uses the capability (in this test, just verifies it
/// resolves), and returns it. A's slot reverts to `Owned`.
fn test_borrow_and_return() {
    println!();
    println!("Testing borrow and return...");

    let core = crate::capability::core_mut();

    let cell_a = core.create_cell().expect("cell A creation failed");
    let cell_b = core.create_cell().expect("cell B creation failed");

    let channel_rights = CapabilityRights::SEND
        | CapabilityRights::RECV
        | CapabilityRights::SHARE
        | CapabilityRights::DESTROY;

    let (channel_obj, channel_cap) = core
        .create_channel(channel_rights)
        .expect("channel creation failed");

    let object = core
        .create_object(ObjectKind::Cell)
        .expect("object creation failed");

    let cap = core
        .allocate(object, CapabilityRights::READ)
        .expect("capability allocation failed");

    assert!(core.grant_capability(cell_a, cap));

    // A borrows the capability out.
    let tag = core
        .borrow_capability(channel_cap, cell_a, cap)
        .expect("borrow_capability must succeed");

    println!("  Borrow tag: 0x{:08x}", tag);

    // A's slot is now BorrowedOut.
    use crate::capability::cell::BorrowState;

    assert_eq!(
        core.cell(cell_a).unwrap().borrow_state(cap),
        Some(BorrowState::BorrowedOut)
    );

    // The capability is still in A's namespace.
    assert!(core.cell_has_capability(cell_a, cap));

    // The capability still resolves in the ITable.
    assert!(core.lookup(cap).is_some());

    println!("  Lender's slot is BorrowedOut: SUCCESS");

    // B dequeues the borrow message.
    let message = core
        .recv_message(channel_cap, cell_b)
        .expect("receive must succeed");

    assert_eq!(
        message.kind,
        crate::capability::channel::MessageKind::BorrowIn
    );
    assert_eq!(message.capability, cap);
    assert_eq!(message.tag, tag);

    println!("  Borrower dequeued the borrow message: SUCCESS");

    // B returns the borrow.
    let returned = core.return_capability(channel_cap, cell_b, cap, tag);

    assert!(returned, "return_capability must succeed");

    // A's slot is back to Owned.
    assert_eq!(
        core.cell(cell_a).unwrap().borrow_state(cap),
        Some(BorrowState::Owned)
    );

    println!("  Lender's slot reverted to Owned: SUCCESS");

    // Cleanup.
    assert!(core.destroy_object(object));
    assert!(core.destroy_object(channel_obj));

    println!("  Borrow and return: SUCCESS");
}

/// Verifies that a `BorrowedOut` capability cannot be sent.
///
/// This is decision B2 from the borrow design: a capability that
/// is currently lent cannot be moved, because the lender does not
/// have it in a movable state. Allowing the send would violate the
/// affine invariant: the capability would be in the message ring
/// *and* the borrower's temporary hold, which is two holders.
fn test_borrow_cannot_move() {
    println!();
    println!("Testing that a BorrowedOut capability cannot be moved...");

    let core = crate::capability::core_mut();

    let cell_a = core.create_cell().expect("cell A creation failed");

    let channel_rights = CapabilityRights::SEND
        | CapabilityRights::RECV
        | CapabilityRights::SHARE
        | CapabilityRights::DESTROY;

    let (channel_obj, channel_cap) = core
        .create_channel(channel_rights)
        .expect("channel creation failed");

    let object = core
        .create_object(ObjectKind::Cell)
        .expect("object creation failed");

    let cap = core
        .allocate(object, CapabilityRights::READ)
        .expect("capability allocation failed");

    assert!(core.grant_capability(cell_a, cap));

    // Borrow it out.
    let _tag = core
        .borrow_capability(channel_cap, cell_a, cap)
        .expect("borrow_capability must succeed");

    // Now try to send it from A. This must fail because A's slot
    // is BorrowedOut.
    let sent = core.send_capability(channel_cap, cell_a, cap);

    assert!(!sent, "a BorrowedOut capability must not be sendable");

    println!("  Send of a BorrowedOut capability rejected: SUCCESS");

    // Cleanup.
    assert!(core.destroy_object(object));
    assert!(core.destroy_object(channel_obj));

    println!("  BorrowedOut cannot be moved: SUCCESS");
}

/// Verifies that `break_borrows_for_cell` releases a lender's slot
/// when the lender is the exiting cell.
///
/// Cell A holds a capability and lends it out. Then A is
/// "exited" by calling `break_borrows_for_cell(cell_a)`. This
/// simulates what the scheduler does when a task exits. A's slot
/// must revert to `Owned`, and the borrow message must be removed
/// from the channel.
fn test_break_borrows_lender_exit() {
    println!();
    println!("Testing break_borrows_for_cell (lender exit)...");

    let core = crate::capability::core_mut();

    let cell_a = core.create_cell().expect("cell A creation failed");

    let channel_rights = CapabilityRights::SEND
        | CapabilityRights::RECV
        | CapabilityRights::SHARE
        | CapabilityRights::DESTROY;

    let (channel_obj, channel_cap) = core
        .create_channel(channel_rights)
        .expect("channel creation failed");

    let object = core
        .create_object(ObjectKind::Cell)
        .expect("object creation failed");

    let cap = core
        .allocate(object, CapabilityRights::READ)
        .expect("capability allocation failed");

    assert!(core.grant_capability(cell_a, cap));

    let _tag = core
        .borrow_capability(channel_cap, cell_a, cap)
        .expect("borrow_capability must succeed");

    // The borrow message is in the channel.
    let channel = core.channel(channel_obj).expect("channel lookup failed");
    assert_eq!(channel.len(), 1);

    println!("  Borrow message in channel: SUCCESS");

    // Simulate A's exit.
    let broken = core.break_borrows_for_cell(cell_a);

    assert_eq!(broken, 1, "one borrow must be broken");

    // A's slot is back to Owned.
    use crate::capability::cell::BorrowState;

    assert_eq!(
        core.cell(cell_a).unwrap().borrow_state(cap),
        Some(BorrowState::Owned)
    );

    // The channel is empty.
    let channel = core.channel(channel_obj).expect("channel lookup failed");
    assert_eq!(channel.len(), 0);

    println!("  Lender's slot reverted, message removed: SUCCESS");

    // Cleanup.
    assert!(core.destroy_object(object));
    assert!(core.destroy_object(channel_obj));

    println!("  Break on lender exit: SUCCESS");
}

/// Verifies that `break_borrows_for_cell` releases the lender's
/// slot when the *borrower* is the exiting cell.
///
/// Cell A lends a capability. Cell B dequeues the borrow message.
/// Then B is "exited" by calling `break_borrows_for_cell(cell_b)`.
/// A's slot must revert to `Owned` and the message must be removed,
/// even though B never called `return_capability`.
fn test_break_borrows_borrower_exit() {
    println!();
    println!("Testing break_borrows_for_cell (borrower exit)...");

    let core = crate::capability::core_mut();

    let cell_a = core.create_cell().expect("cell A creation failed");
    let cell_b = core.create_cell().expect("cell B creation failed");

    let channel_rights = CapabilityRights::SEND
        | CapabilityRights::RECV
        | CapabilityRights::SHARE
        | CapabilityRights::DESTROY;

    let (channel_obj, channel_cap) = core
        .create_channel(channel_rights)
        .expect("channel creation failed");

    let object = core
        .create_object(ObjectKind::Cell)
        .expect("object creation failed");

    let cap = core
        .allocate(object, CapabilityRights::READ)
        .expect("capability allocation failed");

    assert!(core.grant_capability(cell_a, cap));

    let _tag = core
        .borrow_capability(channel_cap, cell_a, cap)
        .expect("borrow_capability must succeed");

    // B dequeues the borrow message. This creates an
    // outstanding-borrow record.
    let _message = core
        .recv_message(channel_cap, cell_b)
        .expect("receive must succeed");

    // The channel is now empty (the message was popped).
    let channel = core.channel(channel_obj).expect("channel lookup failed");
    assert_eq!(channel.len(), 0);

    println!("  Borrower dequeued the borrow message: SUCCESS");

    // Simulate B's exit. The outstanding-borrow record is what
    // makes this work: the message is not in the channel's ring
    // anymore, but the fabric remembers that B holds it.
    let broken = core.break_borrows_for_cell(cell_b);

    assert_eq!(broken, 1, "one borrow must be broken");

    // A's slot is back to Owned.
    use crate::capability::cell::BorrowState;

    assert_eq!(
        core.cell(cell_a).unwrap().borrow_state(cap),
        Some(BorrowState::Owned)
    );

    println!("  Lender's slot reverted despite borrower's exit: SUCCESS");

    // Cleanup.
    assert!(core.destroy_object(object));
    assert!(core.destroy_object(channel_obj));

    println!("  Break on borrower exit: SUCCESS");
}

/// Verifies that destroying a channel cleans up its pending
/// messages.
///
/// A channel holds a `Move` message and a `BorrowIn` message when
/// it is destroyed. The `Move` capability must be revoked (it has
/// no owner); the `BorrowIn` lender's slot must revert to `Owned`;
/// and the borrow's record must be removed.
fn test_channel_destroy_breaks_borrows() {
    println!();
    println!("Testing channel destruction with pending messages...");

    let core = crate::capability::core_mut();

    let cell_a = core.create_cell().expect("cell A creation failed");

    let channel_rights = CapabilityRights::SEND
        | CapabilityRights::RECV
        | CapabilityRights::SHARE
        | CapabilityRights::DESTROY;

    let (channel_obj, channel_cap) = core
        .create_channel(channel_rights)
        .expect("channel creation failed");

    // ---- Part 1: a `Move` message's capability is revoked. ----

    let move_object = core
        .create_object(ObjectKind::Cell)
        .expect("move object creation failed");

    let move_cap = core
        .allocate(move_object, CapabilityRights::READ)
        .expect("move capability allocation failed");

    assert!(core.grant_capability(cell_a, move_cap));
    assert!(core.send_capability(channel_cap, cell_a, move_cap));

    // move_cap is now in the channel's ring, not in A's cell.
    assert!(!core.cell_has_capability(cell_a, move_cap));
    assert!(core.lookup(move_cap).is_some());

    println!("  Move message in channel: SUCCESS");

    // ---- Part 2: a `BorrowIn` message's lender is reverted. ----

    let borrow_object = core
        .create_object(ObjectKind::Cell)
        .expect("borrow object creation failed");

    let borrow_cap = core
        .allocate(borrow_object, CapabilityRights::READ)
        .expect("borrow capability allocation failed");

    assert!(core.grant_capability(cell_a, borrow_cap));

    let _tag = core
        .borrow_capability(channel_cap, cell_a, borrow_cap)
        .expect("borrow_capability must succeed");

    use crate::capability::cell::BorrowState;

    assert_eq!(
        core.cell(cell_a).unwrap().borrow_state(borrow_cap),
        Some(BorrowState::BorrowedOut)
    );

    println!("  BorrowIn message in channel: SUCCESS");

    // ---- Destroy the channel. ----

    assert!(core.destroy_object(channel_obj));

    // The Move message's capability is revoked.
    assert!(
        core.lookup(move_cap).is_none(),
        "Move message's capability must be revoked on channel destroy"
    );

    // The BorrowIn lender's slot is reverted.
    assert_eq!(
        core.cell(cell_a).unwrap().borrow_state(borrow_cap),
        Some(BorrowState::Owned),
        "BorrowIn lender's slot must revert on channel destroy"
    );

    // The BorrowIn capability itself is NOT revoked: it belongs
    // to the lender, not the channel.
    assert!(
        core.lookup(borrow_cap).is_some(),
        "BorrowIn capability belongs to the lender and must survive"
    );

    println!("  Channel destruction cleanup: SUCCESS");

    // Cleanup.
    assert!(core.destroy_object(move_object));
    assert!(core.destroy_object(borrow_object));

    println!("  Channel destroy with pending messages: SUCCESS");
}

/// Verifies that a `BorrowIn` message dequeued by a receiver
/// creates an outstanding-borrow record, and that returning
/// removes it.
///
/// The record is the fabric's only knowledge that a borrower
/// holds a message. Without it, a borrower's task exit could not
/// release the lender's slot. This test asserts that the record
/// exists after a receive and is gone after a return.
fn test_borrow_tracking_on_recv() {
    println!();
    println!("Testing outstanding-borrow tracking...");

    let core = crate::capability::core_mut();

    let cell_a = core.create_cell().expect("cell A creation failed");
    let cell_b = core.create_cell().expect("cell B creation failed");

    let channel_rights = CapabilityRights::SEND
        | CapabilityRights::RECV
        | CapabilityRights::SHARE
        | CapabilityRights::DESTROY;

    let (channel_obj, channel_cap) = core
        .create_channel(channel_rights)
        .expect("channel creation failed");

    let object = core
        .create_object(ObjectKind::Cell)
        .expect("object creation failed");

    let cap = core
        .allocate(object, CapabilityRights::READ)
        .expect("capability allocation failed");

    assert!(core.grant_capability(cell_a, cap));

    let tag = core
        .borrow_capability(channel_cap, cell_a, cap)
        .expect("borrow_capability must succeed");

    // Receive. This is where the record is created.
    let message = core
        .recv_message(channel_cap, cell_b)
        .expect("receive must succeed");

    assert_eq!(message.tag, tag);

    // Simulate B's exit right here. If the record exists, this
    // will succeed and revert A's slot. If it doesn't, the slot
    // stays frozen and the assertion below fails.
    let broken = core.break_borrows_for_cell(cell_b);

    assert_eq!(broken, 1, "the record must have been created on receive");

    use crate::capability::cell::BorrowState;

    assert_eq!(
        core.cell(cell_a).unwrap().borrow_state(cap),
        Some(BorrowState::Owned)
    );

    println!("  Receive created the record; break used it: SUCCESS");

    // Cleanup.
    assert!(core.destroy_object(object));
    assert!(core.destroy_object(channel_obj));

    println!("  Outstanding-borrow tracking: SUCCESS");
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
/// `test_capability_transfer` function, which tested the fabric's
/// now-removed `transfer` operation. Under the three-operation
/// model, the tests are split into focused cases, one per
/// invariant: object lifecycle, the copy operation's leaf-only
/// semantics, the affine invariant, SHARE rejection, move failure
/// modes, the cross-cell operations, and the IPC / borrow layer.
///
/// `run_all` calls this function once; it in turn calls every
/// capability test in sequence.
pub fn test_capability_operations() {
    // ---- Core operations. ----
    test_object_lifecycle();
    test_copy_capability();
    test_affine_invariant();
    test_share_stripping();
    test_move_failure_modes();
    test_cross_cell_operations();

    // ---- IPC and borrow. ----
    test_channel_create_send_recv();
    test_borrow_and_return();
    test_borrow_cannot_move();
    test_borrow_tracking_on_recv();
    test_break_borrows_lender_exit();
    test_break_borrows_borrower_exit();
    test_channel_destroy_breaks_borrows();
}
