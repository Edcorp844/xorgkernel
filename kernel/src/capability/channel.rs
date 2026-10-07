//! IPC channels.
//!
//! A channel is a bounded queue of fixed-size messages. Each
//! message can carry a capability (a moved or borrowed one), a
//! small payload, or both. Channels are how authority and data
//! move between execution cells.
//!
//! # Why a channel is an object
//!
//! A channel is an `ObjectKind`, not a special kernel data
//! structure. That means it is named by a `CapabilityId`, and
//! access to it is governed by the fabric's ordinary capability
//! rules. A cell that holds a capability to a channel can send to
//! it or receive from it, subject to the rights on that capability;
//! a cell that does not hold one cannot. Revoking the capability
//! revokes the cell's access to the channel with no further
//! bookkeeping.
//!
//! This is the same model as every other fabric object. It is
//! what the architecture calls "no ambient authority": there is no
//! global namespace of channels, no well-known IDs, no way to
//! reach a channel except by holding a capability to it.
//!
//! # Why the message lives in the channel
//!
//! When a capability is sent over a channel, the capability's
//! `CapabilityId` is moved into the channel's message ring. The
//! receiver's cell is **not** modified by the send. The receiver
//! dequeues the message when it is ready to receive, and at that
//! point the message (including its capability ID) leaves the
//! channel and becomes the receiver's responsibility.
//!
//! This is the design used by seL4, NEURON, Zircon, and L4. The
//! reasons:
//!
//! - **The kernel does not hold state on behalf of a task.** A
//!   cell is the task's capability namespace. If the kernel
//!   mutated a cell to insert a capability on a send, the kernel
//!   would be holding state that belongs to the task's execution
//!   context. The message-in-the-channel model avoids this: the
//!   message is the kernel's state, and the receiver reads it
//!   when it runs.
//!
//! - **The receiver controls when it takes on authority.** A
//!   message that sits in a channel is not yet the receiver's
//!   problem. If the receiver never dequeues it, the capability
//!   never enters the receiver's cell, and the sender's original
//!   move is simply undone when the channel is destroyed. This is
//!   how a receiver can decline authority without a special
//!   "reject" operation.
//!
//! - **Task exit is clean.** If a receiver exits mid-message, the
//!   message in the channel becomes orphaned. The channel's
//!   `Drop` handles it. No cell has to be told to update, because
//!   no cell was modified when the message was sent.
//!
//! # Message layout
//!
//! Each message is a fixed-size record:
//!
//! ```text
//!   capability : CapabilityId      4 bytes
//!   payload    : [u32; 4]         16 bytes
//!   tag        : u32               4 bytes
//!   kind       : MessageKind       1 byte (padded to 4)
//! ```
//!
//! The fixed size is a deliberate choice. Variable-size messages
//! require either a per-message memory object (heavyweight) or a
//! kernel-side allocator (the kernel has none, by design). A
//! fixed-size message covers the fast path — a capability move, a
//! short payload, a request ID — and large payloads travel as a
//! memory object capability in the `capability` field, with the
//! data mapped by the receiver.
//!
//! The payload is 4 words because that is what fits in the
//! message without making the ring unacceptably large: at 24
//! bytes per message and 32 messages per channel, the ring is
//! 768 bytes per channel. A larger payload would multiply that.
//! When a payload genuinely needs more than 4 words, the sender
//! should send a memory-object capability instead.
//!
//! # Non-blocking semantics
//!
//! `send` and `recv` are non-blocking. A send to a full channel
//! fails and returns `false`; a receive from an empty channel
//! fails and returns `None`. The caller decides what to do:
//! retry, drop the message, yield and try again, or report a
//! failure.
//!
//! Blocking IPC — where a sender waits for a free slot and a
//! receiver waits for a message — is a scheduling concern. It
//! requires the scheduler to have a wait queue per channel and a
//! wake path. That will be added on top of the non-blocking
//! primitives, not instead of them. The non-blocking operations
//! are the primitive; blocking is a policy the scheduler layers
//! on.
//!
//! # Borrow messages
//!
//! A borrow is a kind of message. When a cell borrows a
//! capability out to another cell, the lender's slot transitions
//! to `BorrowedOut`, and a message of kind `BorrowIn` is placed
//! in the channel. The borrower dequeues the message, uses the
//! capability for the duration of its invocation, and sends a
//! return message. On return, the lender's slot reverts to
//! `Owned` and the channel's borrow message is discarded.
//!
//! The borrower never holds the borrowed capability in its own
//! cell. The capability ID lives in the message, which lives in
//! the channel. This is what makes non-nested borrows easy to
//! enforce and what makes task exit clean: if the borrower exits
//! mid-borrow, the message is simply dropped by the channel's
//! `Drop`, and the lender's slot reverts.

use crate::capability::capability::CapabilityId;

/// Maximum number of messages in a channel's ring.
///
/// The ring is a fixed-size array; a channel at capacity cannot
/// accept more messages until the receiver dequeues some. 32 is a
/// balance: large enough that typical IPC bursts fit, small
/// enough that a channel's memory footprint stays modest.
pub const MAX_CHANNEL_MESSAGES: usize = 32;

/// Number of payload words per message.
///
/// See the module documentation for why this is 4 and not larger.
pub const MESSAGE_PAYLOAD_WORDS: usize = 4;

/// A message in a channel.
///
/// Fixed-size. See the module documentation for the layout and the
/// rationale.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Message {
    /// The capability carried by this message, or
    /// [`CapabilityId::INVALID`] if the message carries no
    /// capability.
    ///
    /// For a `Move` message, the capability is being transferred
    /// to the receiver. For a `BorrowIn` message, the capability
    /// is being lent. For a `BorrowReturn` message, the field is
    /// `INVALID`; the return identifies the borrow by tag.
    pub capability: CapabilityId,

    /// Up to `MESSAGE_PAYLOAD_WORDS` words of payload.
    ///
    /// The meaning of the words is defined by the message's
    /// sender and receiver. The kernel does not interpret them.
    pub payload: [u32; MESSAGE_PAYLOAD_WORDS],

    /// A tag the sender can use to distinguish message kinds.
    ///
    /// The kernel does not interpret this field. It is preserved
    /// unchanged across send and receive. Typical uses:
    ///
    /// - A request ID for matching a reply to a request.
    /// - A syscall number for a syscall message.
    /// - A channel-specific message type.
    ///
    /// For `BorrowIn` and `BorrowReturn` messages, the tag is used
    /// internally to match a return to its borrow. Senders that
    /// never send borrow messages may use the tag for any purpose.
    pub tag: u32,

    /// The kind of message.
    ///
    /// See [`MessageKind`].
    pub kind: MessageKind,
}

impl Message {
    /// Creates a message with no capability and no payload.
    ///
    /// Useful as a starting point for a caller that will fill in
    /// the fields it cares about. Senders that always set every
    /// field may prefer a struct literal.
    pub const fn empty() -> Self {
        Self {
            capability: CapabilityId::INVALID,
            payload: [0; MESSAGE_PAYLOAD_WORDS],
            tag: 0,
            kind: MessageKind::Move,
        }
    }

    /// Creates a `Move` message carrying a capability.
    pub const fn move_capability(capability: CapabilityId) -> Self {
        Self {
            capability,
            payload: [0; MESSAGE_PAYLOAD_WORDS],
            tag: 0,
            kind: MessageKind::Move,
        }
    }

    /// Creates a `BorrowIn` message carrying a borrowed capability.
    pub const fn borrow_in(capability: CapabilityId, tag: u32) -> Self {
        Self {
            capability,
            payload: [0; MESSAGE_PAYLOAD_WORDS],
            tag,
            kind: MessageKind::BorrowIn,
        }
    }

    /// Creates a `BorrowReturn` message for the given borrow tag.
    pub const fn borrow_return(tag: u32) -> Self {
        Self {
            capability: CapabilityId::INVALID,
            payload: [0; MESSAGE_PAYLOAD_WORDS],
            tag,
            kind: MessageKind::BorrowReturn,
        }
    }
}

/// The kind of a message.
///
/// Determines how the message's `capability` field is treated.
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MessageKind {
    /// A capability move. The capability named in the message is
    /// being transferred from the sender to the receiver. The
    /// sender no longer holds it; the receiver becomes the sole
    /// holder when it dequeues the message.
    Move,

    /// A capability borrow. The capability named in the message is
    /// being lent by the sender for the duration of the receiver's
    /// invocation. The sender's slot is `BorrowedOut`. The
    /// receiver will return the capability by sending a
    /// `BorrowReturn` message with the same tag.
    BorrowIn,

    /// A borrow return. The message carries no capability; its
    /// `tag` matches a previously-sent `BorrowIn`. On receipt, the
    /// lender's slot for that borrow reverts to `Owned`.
    BorrowReturn,
}

/// An IPC channel.
///
/// A bounded FIFO ring of [`Message`]s. The channel is owned by
/// the fabric's core (as a channel-kind object), not by any cell.
/// Cells reach it through a `CapabilityId`; the fabric checks that
/// the capability carries `SEND` or `RECV` as appropriate.
///
/// # Concurrency
///
/// The channel is not internally synchronized. All operations go
/// through `CapabilityCore`, which is single-threaded and
/// interrupt-disabled by the fabric's own discipline. Adding
/// concurrency later would mean adding a lock or a lock-free
/// design to `CapabilityCore`, not to `Channel`.
pub struct Channel {
    /// The fabric's ID for this object.
    ///
    /// Assigned by the registry when the channel is created
    /// through the fabric. `ObjectId::INVALID` before registration.
    id: crate::capability::object::ObjectId,

    /// Ring of messages.
    ///
    /// Valid entries are `head..head+len` modulo `MAX_CHANNEL_MESSAGES`.
    /// The rest are stale and must not be read.
    messages: [Message; MAX_CHANNEL_MESSAGES],

    /// Index of the first valid message in the ring.
    ///
    /// Only meaningful when `len > 0`.
    head: usize,

    /// Number of valid messages in the ring.
    len: usize,

    /// Next tag value for outbound borrow messages.
    ///
    /// Tags are generated monotonically per channel and wrap on
    /// overflow. A tag is used to match a `BorrowReturn` to the
    /// `BorrowIn` that created the borrow.
    next_borrow_tag: u32,
}

impl Channel {
    /// Creates an empty channel.
    ///
    /// The channel is not yet registered with the fabric. The
    /// fabric calls `set_id` during `create_channel`.
    pub const fn new() -> Self {
        Self {
            id: crate::capability::object::ObjectId::INVALID,
            // The message array is initialized with `empty()`
            // messages. These are placeholders; the ring's `len`
            // field is 0, so no placeholder is ever read as a
            // valid message.
            messages: [const { Message::empty() }; MAX_CHANNEL_MESSAGES],
            head: 0,
            len: 0,
            next_borrow_tag: 1,
        }
    }

    /// Returns the number of messages currently in the channel.
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Returns whether the channel is empty.
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Returns whether the channel is full.
    pub const fn is_full(&self) -> bool {
        self.len == MAX_CHANNEL_MESSAGES
    }

    /// Returns the fabric's ID for this channel.
    pub const fn id(&self) -> crate::capability::object::ObjectId {
        self.id
    }

    /// Sets the fabric's ID for this channel.
    ///
    /// Called by the fabric during `create_channel`. Not public
    /// outside the crate: a caller that wants a channel must go
    /// through the fabric.
    pub(crate) fn set_id(&mut self, id: crate::capability::object::ObjectId) {
        self.id = id;
    }

    /// Appends a message to the ring.
    ///
    /// Returns `false` if the ring is full.
    pub fn push(&mut self, message: Message) -> bool {
        if self.is_full() {
            return false;
        }

        let index = (self.head + self.len) % MAX_CHANNEL_MESSAGES;
        self.messages[index] = message;
        self.len += 1;

        true
    }

    /// Prepends a message to the ring.
    ///
    /// Returns `false` if the ring is full. Used by
    /// [`crate::capability::core::CapabilityCore::recv_message`]
    /// to put a `BorrowIn` message back when the borrow table is
    /// full, preserving the channel's FIFO order.
    ///
    /// # Correctness
    ///
    /// The ring is a circular array with `head` pointing at the
    /// oldest message and `len` counting the live messages. The
    /// slot at `head - 1` (modulo ring size) is the slot *before*
    /// the oldest message, which is always free: it holds either
    /// a stale copy of a message that was already popped, or a
    /// placeholder. The `is_full` check guarantees the ring has
    /// at least one free slot, so moving `head` backward by one
    /// and writing into the vacated slot cannot overwrite a live
    /// message.
    ///
    /// The logical order after the operation is
    /// `[new, old_head, old_head + 1, ...]`, which is what a
    /// push-to-front should produce.
    ///
    /// # Cost
    ///
    /// O(1). The name "shift the head backward" describes the
    /// index arithmetic, not a data movement; no messages are
    /// copied. The O(len) cost mentioned in earlier designs was
    /// wrong — the ring layout allows O(1) push-front because the
    /// slot before the head is always free.
    pub fn push_front(&mut self, message: Message) -> bool {
        if self.is_full() {
            return false;
        }

        // Move `head` backward by one slot (wrapping) and write
        // the new message there. The slot is free: see the doc.
        self.head = (self.head + MAX_CHANNEL_MESSAGES - 1) % MAX_CHANNEL_MESSAGES;
        self.messages[self.head] = message;
        self.len += 1;

        true
    }

    /// Removes and returns the oldest message in the ring.
    ///
    /// Returns `None` if the ring is empty.
    pub fn pop(&mut self) -> Option<Message> {
        if self.is_empty() {
            return None;
        }

        let message = self.messages[self.head];
        self.head = (self.head + 1) % MAX_CHANNEL_MESSAGES;
        self.len -= 1;

        Some(message)
    }

    /// Generates a fresh tag for an outbound borrow.
    ///
    /// The tags are monotonic per channel and wrap on overflow.
    /// Two borrows on the same channel will not have the same tag
    /// unless the channel has sent 2^32 borrows, at which point
    /// the channel is either extremely busy or the kernel has been
    /// running for a very long time. The wrap is a real (if
    /// remote) possibility; a future version could use a
    /// free-list of tags to guarantee uniqueness for the lifetime
    /// of the channel.
    pub fn next_borrow_tag(&mut self) -> u32 {
        let tag = self.next_borrow_tag;
        self.next_borrow_tag = self.next_borrow_tag.wrapping_add(1);

        // Skip 0, which is reserved for "no tag."
        if self.next_borrow_tag == 0 {
            self.next_borrow_tag = 1;
        }

        tag
    }

    /// Returns whether the channel contains a `BorrowReturn`
    /// message with the given tag.
    ///
    /// This is a read-only helper provided for symmetry with
    /// `remove_borrow_in`. The fabric's return path uses
    /// `remove_borrow_in` directly, which both finds and removes
    /// the message. This method is available for callers that
    /// need to check a return's presence without consuming it.
    ///
    /// Scans the ring linearly. The ring is small (at most 32
    /// messages), so the scan is cheap.
    pub fn has_borrow_return(&self, tag: u32) -> bool {
        for offset in 0..self.len {
            let index = (self.head + offset) % MAX_CHANNEL_MESSAGES;
            let message = self.messages[index];

            if message.kind == MessageKind::BorrowReturn && message.tag == tag {
                return true;
            }
        }

        false
    }

    /// Returns the message at the given offset from the head,
    /// without removing it.
    ///
    /// `offset == 0` returns the oldest message. Returns `None`
    /// if `offset >= len`.
    ///
    /// Used by [`crate::capability::core::CapabilityCore::destroy_object`]
    /// to inspect a channel's pending messages before dropping the
    /// channel, and by tests that need to verify a message without
    /// consuming it.
    pub fn peek(&self, offset: usize) -> Option<Message> {
        if offset >= self.len {
            return None;
        }

        let index = (self.head + offset) % MAX_CHANNEL_MESSAGES;
        Some(self.messages[index])
    }

    /// Removes a `BorrowIn` message whose capability matches the
    /// given ID and whose tag matches the given tag.
    ///
    /// Returns `true` if the message was found and removed. Used
    /// by [`crate::capability::core::CapabilityCore::return_capability`]
    /// to remove the borrow message when the borrower returns the
    /// capability.
    ///
    /// The `tag` identifies the borrow uniquely. The capability
    /// ID is checked as a defense: if the tag matched but the
    /// capability did not, the message is not the one the caller
    /// is returning, and removing it would corrupt the channel's
    /// state.
    pub fn remove_borrow_in(&mut self, capability: CapabilityId, tag: u32) -> bool {
        for offset in 0..self.len {
            let index = (self.head + offset) % MAX_CHANNEL_MESSAGES;
            let message = self.messages[index];

            if message.kind == MessageKind::BorrowIn
                && message.tag == tag
                && message.capability == capability
            {
                // Shift the ring to remove the message at
                // `offset`. This is O(len), which is fine for
                // the small ring sizes the fabric uses.
                for shift in offset..(self.len - 1) {
                    let from = (self.head + shift + 1) % MAX_CHANNEL_MESSAGES;
                    let to = (self.head + shift) % MAX_CHANNEL_MESSAGES;
                    self.messages[to] = self.messages[from];
                }

                // The vacated slot becomes a placeholder.
                let last = (self.head + self.len - 1) % MAX_CHANNEL_MESSAGES;
                self.messages[last] = Message::empty();

                self.len -= 1;
                return true;
            }
        }

        false
    }
}
