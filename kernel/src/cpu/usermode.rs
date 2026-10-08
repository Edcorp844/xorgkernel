//! User-mode support.
//!
//! This module provides the Rust-side pieces of the transition to
//! CPL 3:
//!
//! - [`UserEntry`], the descriptor of a user task's initial code
//!   and stack.
//! - [`iret_to_user`], the assembly label the kernel's return path
//!   to CPL 3 lands on.
//! - [`install_user_image`], a helper that copies a code blob into
//!   a memory object and returns the object's capability.
//! - [`SESSION_3_BLOB`], the small hand-assembled code the first
//!   user task runs.
//!
//! # Portability
//!
//! The design here is chosen so that the eventual x86_64 port is a
//! change to `cpu/usermode.S` and to the syscall entry stub, not a
//! change to the ABI or to the frame layout. See the assembly
//! file's header comment for the specific conditions the fast path
//! will check, and `cpu/syscall.rs` for the shape of the frame the
//! handler sees.
//!
//! # The initial user frame
//!
//! On first schedule of a user task, the kernel stack is laid out
//! so that `switch_context`'s `ret` lands in [`iret_to_user`], and
//! `iret` pops a five-dword frame:
//!
//! ```text
//!   [higher addresses]
//!   +-----------------------+
//!   | user SS   (0x23)      |
//!   +-----------------------+
//!   | user ESP              |
//!   +-----------------------+
//!   | user EFLAGS (0x202)   |
//!   +-----------------------+
//!   | user CS   (0x1B)      |
//!   +-----------------------+
//!   | user EIP              |
//!   +-----------------------+
//!   | return addr = iret_to_user |
//!   +-----------------------+
//!   | 0 (EBP)               |
//!   | 0 (EDI)               |
//!   | 0 (ESI)               |
//!   | 0 (EBX)               |  <- Task.esp points here
//!   +-----------------------+
//!   [lower addresses]
//! ```
//!
//! `Task::create_user` builds this frame. The layout is
//! deliberately identical to what the CPU pushes on a trap from
//! CPL 3, so that a syscall's return path and a fresh task's
//! first-run path converge on the same `iret` instruction with
//! the same frame shape.

use crate::capability::capability::{CapabilityId, CapabilityRights};
use crate::capability::object::ObjectId;
use crate::memory::direct_map;
use crate::memory::object::MemoryObject;

/// The user entry point and stack top for a task.
///
/// Held on the task's kernel stack in the layout `iret` expects.
/// The two fields are the *user-side* addresses, not kernel
/// addresses: `entry_point` is where the CPU starts executing at
/// CPL 3, and `stack_top` is where the user stack pointer is
/// initialized.
///
/// # Why a struct and not two loose `u32`s
///
/// The frame is a fixed layout in memory; the struct makes the
/// layout explicit and lets `Task::create_user` and the tests
/// agree on field order without magic offsets. The assembly stub
/// does not read this struct directly — it reads the memory
/// `Task::create_user` writes — but the two must agree, and a
/// struct is the right way to keep them in sync.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct UserEntry {
    /// The user code's entry point, as a user virtual address.
    pub entry_point: u32,

    /// The top of the user stack, as a user virtual address.
    ///
    /// The stack grows downward from this address. On first entry,
    /// the user code sees `ESP` set to this value.
    pub stack_top: u32,
}

impl UserEntry {
    /// Creates a user entry descriptor.
    pub const fn new(entry_point: u32, stack_top: u32) -> Self {
        Self {
            entry_point,
            stack_top,
        }
    }

    /// Returns the value of the `CS` selector a user task runs
    /// under.
    ///
    /// `0x18` is the user code descriptor's index; the low two
    /// bits are the RPL, which must be 3 for a ring-3 selector.
    /// `0x18 | 3 == 0x1B`.
    pub const fn user_cs() -> u32 {
        0x18 | 3
    }

    /// Returns the value of the `SS` selector a user task runs
    /// under.
    ///
    /// `0x20` is the user data descriptor's index; the low two
    /// bits are the RPL, which must be 3. `0x20 | 3 == 0x23`.
    pub const fn user_ss() -> u32 {
        0x20 | 3
    }

    /// Returns the initial `EFLAGS` value for a user task.
    ///
    /// `0x202`: bit 1 (reserved, always set) and bit 9 (IF, so
    /// hardware interrupts are delivered while user code runs).
    /// Without IF set, the user task would never be preempted,
    /// and a busy loop would hang the kernel.
    pub const fn user_eflags() -> u32 {
        0x202
    }
}

/// Returns the address of the `iret_to_user` assembly label.
///
/// Used by `Task::create_user` to place the label's address in the
/// return-address slot of a new user task's initial frame.
pub fn iret_to_user_address() -> u32 {
    iret_to_user as usize as u32
}

/// The assembly label that returns to CPL 3.
///
/// Defined in `cpu/usermode.S`. See that file for what it does and
/// why it is the single return-to-user instruction for the kernel.
///
/// # Safety
///
/// The caller must have arranged the kernel stack so that the top
/// five dwords are a valid user frame: `EIP, CS, EFLAGS, ESP, SS`.
/// The frame must be valid in the address space that will be active
/// after the `iret`. See [`UserEntry`] for the frame layout.
unsafe extern "C" {
    pub fn iret_to_user() -> !;
}

/// The user code blob for Session 5.
///
/// The blob exercises the three syscalls implemented in this
/// session. It is hand-assembled 32-bit x86; the encoding of each
/// instruction is documented in the comments.
///
/// # What it does
///
/// 1. `SYS_DEBUG_WRITE` with a pointer to a "hello" string and its
///    length. The kernel validates the buffer against the user
///    address space and prints it.
/// 2. `SYS_SELF_CELL`, to verify that a syscall can return a value
///    in EDX. The value is ignored by the blob; the observable
///    evidence is the absence of an error status.
/// 3. `SYS_YIELD`, to give up the CPU and let the scheduler run
///    another task. The blob resumes when rescheduled.
/// 4. An infinite loop, so the task stays alive and continues to
///    be preempted by the timer.
///
/// The string is embedded in the blob at a known offset. The blob
/// is copied into the user code page by `write_blob_into_object`,
/// so the string is at `USER_CODE_BASE + offset`, and the `mov ebx`
/// instruction loads that absolute address.
///
/// # Layout
///
/// ```text
///   offset  bytes                       meaning
///   0x00    B8 01 00 00 00              mov eax, SYS_DEBUG_WRITE
///   0x05    BB <ptr>                    mov ebx, msg_addr
///   0x0A    B9 <len>                    mov ecx, msg_len
///   0x0F    CD 80                       int 0x80
///   0x11    B8 03 00 00 00              mov eax, SYS_SELF_CELL
///   0x16    CD 80                       int 0x80
///   0x18    B8 02 00 00 00              mov eax, SYS_YIELD
///   0x1D    CD 80                       int 0x80
///   0x1F    EB FE                       jmp $
///   0x21    "hello from user mode\n"    the message
/// ```
///
/// The `mov ebx, msg_addr` and `mov ecx, msg_len` instructions are
/// patched at runtime by `setup_session_5_user_space` before the
/// blob is copied into the code page. The initial encoding in the
/// array has zeros in those fields; the runtime patch fills them
/// with the correct address and length.
pub const SESSION_5_BLOB: [u8; 64] = [
    // 0x00: mov eax, SYS_DEBUG_WRITE (1)
    0xB8, 0x01, 0x00, 0x00, 0x00, // 0x05: mov ebx, <msg address>  (patched at runtime)
    0xBB, 0x00, 0x00, 0x00, 0x00, // 0x0A: mov ecx, <msg length>   (patched at runtime)
    0xB9, 0x00, 0x00, 0x00, 0x00, // 0x0F: int 0x80
    0xCD, 0x80, // 0x11: mov eax, SYS_SELF_CELL (3)
    0xB8, 0x03, 0x00, 0x00, 0x00, // 0x16: int 0x80
    0xCD, 0x80, // 0x18: mov eax, SYS_YIELD (2)
    0xB8, 0x02, 0x00, 0x00, 0x00, // 0x1D: int 0x80
    0xCD, 0x80, // 0x1F: jmp $ (infinite loop)
    0xEB, 0xFE, // 0x21: message (21 bytes)
    b'h', b'e', b'l', b'l', b'o', b' ', b'f', b'r', b'o', b'm', b' ', b'u', b's', b'e', b'r', b' ',
    b'm', b'o', b'd', b'e', b'\n', // padding to 64 bytes: 64 - 33 - 21 = 10 zeros
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];
/// Writes a byte blob into the physical frames of a memory object.
///
/// The memory object must have at least `blob.len()` bytes of
/// capacity; the function writes at offset 0 of the object's first
/// frame and does not check the object's size. The caller is
/// responsible for allocating an object large enough.
///
/// # How it works
///
/// `MemoryObject::frame(i)` returns the `Frame` for the i-th page
/// of the object, and `Frame::address()` returns its physical
/// address. The direct map (`direct_map::phys_to_virt`) gives a
/// kernel-accessible pointer to any physical address, so the write
/// proceeds through the direct map without the object being mapped
/// anywhere first.
///
/// This is a temporary mechanism for Session 3. When a proper
/// loader exists (Session 4 or later), it will write user images
/// by mapping the target object into a kernel-accessible view and
/// copying through that view, which is the same mechanism but with
/// the mapping explicit.
///
/// # Safety
///
/// The caller must ensure:
///
/// - the memory object is at least `blob.len()` bytes;
/// - the caller has exclusive access to the object's frames (which
///   is true for a freshly allocated object that has not been
///   shared).
pub fn write_blob_into_object(object: &MemoryObject, blob: &[u8]) {
    let bytes_per_frame = 4096;
    let mut remaining = blob;

    let mut frame_index = 0;
    while !remaining.is_empty() {
        let frame = object
            .frame(frame_index)
            .expect("user blob exceeds memory object size");

        let frame_phys = frame.address();
        let frame_virt = direct_map::phys_to_virt(frame_phys) as *mut u8;

        let chunk_len = remaining.len().min(bytes_per_frame);
        unsafe {
            core::ptr::copy_nonoverlapping(remaining.as_ptr(), frame_virt, chunk_len);
        }

        remaining = &remaining[chunk_len..];
        frame_index += 1;
    }
}

// ---------------------------------------------------------------------
// Address-space setup helper
// ---------------------------------------------------------------------

/// The virtual address where a Session 3 user task's code is
/// mapped.
///
/// Chosen to be well above the kernel's identity map (which
/// occupies the low 4 MiB) and above the framebuffer mapping (PDEs
/// 256-259, or 0x40000000-0x43FFFFFF). It sits inside the range
/// `AddressSpace::map` allows for user mappings (PDEs 1-767), and
/// it is a page-aligned address.
///
/// 0x80000000 is PDE 512. Nothing else in the kernel's user-mapping
/// range uses it, so a future addition of a data segment or a
/// second code page will not collide.
pub const USER_CODE_BASE: u32 = 0x8000_0000;

/// The virtual address where a Session 3 user task's stack is
/// mapped.
///
/// 0x80010000 is PDE 512 plus one 64 KiB range. Well separated
/// from the code, and inside the user-mapping range.
pub const USER_STACK_BASE: u32 = 0x8001_0000;

/// The size of a Session 3 user task's stack, in bytes.
///
/// 4 KiB, one page. Enough for the Session 3 blob, which does not
/// use the stack at all beyond what `int 0x80` pushes. Session 4
/// will size the stack per task.
pub const USER_STACK_SIZE: u32 = 4096;

/// Sets up a Session 3 user address space.
///
/// Allocates two memory objects (one for the code, one for the
/// stack), copies the blob into the code object, allocates a fresh
/// address space, and maps both objects at the addresses above
/// with the user-accessible bit set.
///
/// Returns `(address_space_cap, code_object, stack_object,
/// UserEntry)` on success. The caller is responsible for
/// eventually destroying the objects and the address space.
///
/// # Rights
///
/// The returned address-space capability carries `MAP | UNMAP |
/// ACTIVATE | SHARE`. The returned memory-object capabilities
/// carry `MAP | READ` (code) and `MAP | READ | WRITE` (stack), and
/// they are *not* returned to the caller — the function keeps them
/// internally, because the caller only needs the `ObjectId`s to
/// destroy them later.
///
/// # Failure modes
///
/// Returns `None` on any allocation failure. Partial state is
/// rolled back: an address space created for a task that cannot be
/// fully set up is destroyed before returning.
/// Sets up a Session 5 user address space.
///
/// Same as the Session 3 setup, but patches the `mov ebx, imm32`
/// and `mov ecx, imm32` instructions in the blob so that they load
/// the correct absolute address and length of the message string.
///
/// The message string is embedded in the blob at a known offset.
/// Because the blob is copied into the user code page at
/// `USER_CODE_BASE`, the runtime address of the string is
/// `USER_CODE_BASE + MESSAGE_OFFSET`.
pub fn setup_session_5_user_space() -> Option<(CapabilityId, ObjectId, ObjectId, UserEntry)> {
    /// Offset of the message string within the blob.
    const MESSAGE_OFFSET: u32 = 0x21;

    /// Length of the message string, in bytes.
    const MESSAGE_LENGTH: u32 = 21;

    /// Offset of the `mov ebx, imm32` instruction's immediate.
    const EBX_IMM_OFFSET: usize = 0x06;

    /// Offset of the `mov ecx, imm32` instruction's immediate.
    const ECX_IMM_OFFSET: usize = 0x0B;

    let core = crate::capability::core_mut();

    let code_rights = CapabilityRights::MAP | CapabilityRights::READ;
    let (code_obj, code_cap) = core.allocate_memory(1, code_rights)?;

    // Build the patched blob on the kernel stack.
    let mut patched = SESSION_5_BLOB;

    let message_address = USER_CODE_BASE + MESSAGE_OFFSET;
    let message_length = MESSAGE_LENGTH;

    // Patch `mov ebx, imm32`.
    patched[EBX_IMM_OFFSET + 0] = (message_address & 0xFF) as u8;
    patched[EBX_IMM_OFFSET + 1] = ((message_address >> 8) & 0xFF) as u8;
    patched[EBX_IMM_OFFSET + 2] = ((message_address >> 16) & 0xFF) as u8;
    patched[EBX_IMM_OFFSET + 3] = ((message_address >> 24) & 0xFF) as u8;

    // Patch `mov ecx, imm32`.
    patched[ECX_IMM_OFFSET + 0] = (message_length & 0xFF) as u8;
    patched[ECX_IMM_OFFSET + 1] = ((message_length >> 8) & 0xFF) as u8;
    patched[ECX_IMM_OFFSET + 2] = ((message_length >> 16) & 0xFF) as u8;
    patched[ECX_IMM_OFFSET + 3] = ((message_length >> 24) & 0xFF) as u8;

    // Write the patched blob into the code object's first frame.
    {
        let mem = core
            .memory_object(code_cap)
            .expect("just-created memory object must resolve");
        write_blob_into_object(mem, &patched);
    }

    // From here, the same sequence as Session 3: allocate the
    // stack object, allocate the address space, map both, build
    // the UserEntry.

    let stack_rights = CapabilityRights::MAP | CapabilityRights::READ | CapabilityRights::WRITE;
    let (stack_obj, stack_cap) = match core.allocate_memory(1, stack_rights) {
        Some(s) => s,
        None => {
            core.destroy_object(code_obj);
            return None;
        }
    };

    let as_rights = CapabilityRights::MAP
        | CapabilityRights::UNMAP
        | CapabilityRights::ACTIVATE
        | CapabilityRights::SHARE;

    let (as_obj, as_cap) = match core.allocate_address_space(as_rights) {
        Some(a) => a,
        None => {
            core.destroy_object(code_obj);
            core.destroy_object(stack_obj);
            return None;
        }
    };

    if core
        .map_memory(as_cap, code_cap, USER_CODE_BASE, false, true)
        .is_err()
    {
        core.destroy_object(as_obj);
        core.destroy_object(code_obj);
        core.destroy_object(stack_obj);
        return None;
    }

    if core
        .map_memory(as_cap, stack_cap, USER_STACK_BASE, true, true)
        .is_err()
    {
        core.destroy_object(as_obj);
        core.destroy_object(code_obj);
        core.destroy_object(stack_obj);
        return None;
    }

    let entry = UserEntry::new(USER_CODE_BASE, USER_STACK_BASE + USER_STACK_SIZE);

    Some((as_cap, code_obj, stack_obj, entry))
}
