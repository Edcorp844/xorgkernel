//! Intrusive doubly-linked list of tasks.
//!
//! The list threads through one pair of `next` and `prev` pointers
//! inside each `Task`. Which pair is determined at construction
//! time: the caller passes the byte offsets of the two link fields.
//!
//! The scheduler creates one `IntrusiveList` per purpose:
//!
//! - The all-tasks list uses the `all_next`/`all_prev` offsets.
//! - Each run queue uses the `run_next`/`run_prev` offsets.
//!
//! Because the offsets are computed with `core::mem::offset_of!` at
//! compile time, they cannot drift from the `Task` struct
//! definition.
//!
//! # Invariants
//!
//! The list maintains the following invariants at all times:
//!
//! - If the list is empty, `head` and `tail` are both `None` and
//!   `len == 0`.
//! - If the list is non-empty, the head's `prev` link is `None` and
//!   the tail's `next` link is `None`.
//! - For every task in the list except the tail, that task's `next`
//!   link points at the next task and that task's `prev` link points
//!   back.
//! - `len` equals the number of tasks in the list.
//!
//! # Caller contract
//!
//! This module does **not** check whether a task is already in a
//! list. A task that is the sole member of a list has both its
//! `next` and `prev` links set to `None`, so "are the links set?" is
//! not a reliable test of membership. Instead, the caller is
//! required to know whether a task is in the list, and to only call
//! `push_back` on tasks that are not, and `remove` on tasks that
//! are.
//!
//! In the scheduler, membership is tracked by task state:
//!
//! - A task in a run queue has state `Ready`. No other state
//!   corresponds to run-queue membership.
//! - Every live task is in the all-tasks list for its entire
//!   lifetime.
//!
//! The scheduler's `make_ready`, `make_blocked`, and `exit` methods
//! enforce these correspondences before calling into this module.
//!
//! # Raw-pointer API
//!
//! To avoid fighting the borrow checker, mutating operations take a
//! `*mut Task` rather than a `&mut Task`. The scheduler is the sole
//! user of this module; its invariants guarantee that every pointer
//! passed in is valid and that every task in a list stays alive
//! while it is a member.

use core::ptr::NonNull;

use super::task::Task;

/// An intrusive doubly-linked list of tasks.
pub struct IntrusiveList {
    /// First task in the list, or `None` if empty.
    head: Option<NonNull<Task>>,

    /// Last task in the list, or `None` if empty.
    tail: Option<NonNull<Task>>,

    /// Number of tasks in the list.
    len: usize,

    /// Byte offset of the `next` link in `Task`.
    next_offset: usize,

    /// Byte offset of the `prev` link in `Task`.
    prev_offset: usize,
}

impl IntrusiveList {
    /// Creates an empty list that will use the `next` and `prev`
    /// links at the given byte offsets inside `Task`.
    pub const fn new(next_offset: usize, prev_offset: usize) -> Self {
        Self {
            head: None,
            tail: None,
            len: 0,
            next_offset,
            prev_offset,
        }
    }

    /// Returns `true` if the list contains no tasks.
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Returns the number of tasks in the list.
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Returns the task at the front of the list without removing it.
    ///
    /// # Safety
    ///
    /// No mutable reference to the returned task may be live while
    /// the returned reference is used.
    pub unsafe fn peek_front(&self) -> Option<&Task> {
        self.head.map(|ptr| unsafe { ptr.as_ref() })
    }

    // -----------------------------------------------------------------
    // Link accessors
    // -----------------------------------------------------------------

    /// Returns a mutable reference to the `next` link of a task.
    ///
    /// # Safety
    ///
    /// `task` must point at a live `Task`.
    #[inline]
    unsafe fn next_link(&self, task: *mut Task) -> &'static mut Option<NonNull<Task>> {
        unsafe {
            &mut *((task as *mut u8).add(self.next_offset) as *mut Option<NonNull<Task>>)
        }
    }

    /// Returns a mutable reference to the `prev` link of a task.
    ///
    /// # Safety
    ///
    /// `task` must point at a live `Task`.
    #[inline]
    unsafe fn prev_link(&self, task: *mut Task) -> &'static mut Option<NonNull<Task>> {
        unsafe {
            &mut *((task as *mut u8).add(self.prev_offset) as *mut Option<NonNull<Task>>)
        }
    }

    // -----------------------------------------------------------------
    // Mutations
    // -----------------------------------------------------------------

    /// Pushes a task onto the back of the list.
    ///
    /// # Safety
    ///
    /// `task` must point at a live `Task` that is **not** currently
    /// in any list using the same link pair. The caller is
    /// responsible for this; `push_back` does not check.
    pub unsafe fn push_back(&mut self, task: *mut Task) {
        let task_nonnull = unsafe { NonNull::new_unchecked(task) };

        // Clear the task's own links and point its prev at the
        // current tail.
        unsafe {
            *self.next_link(task) = None;
            *self.prev_link(task) = self.tail;
        }

        // Update the old tail (or the head, if the list was empty)
        // to point at the new task.
        match self.tail {
            Some(mut tail) => unsafe {
                *self.next_link(tail.as_ptr()) = Some(task_nonnull);
            },
            None => {
                self.head = Some(task_nonnull);
            }
        }

        self.tail = Some(task_nonnull);
        self.len += 1;
    }

    /// Pops a task from the front of the list.
    ///
    /// Returns a raw pointer to the removed task, or null if the
    /// list is empty.
    ///
    /// # Safety
    ///
    /// The caller must ensure the returned task stays alive and that
    /// no other mutable reference to it is live.
    pub unsafe fn pop_front(&mut self) -> *mut Task {
        let head = match self.head {
            Some(h) => h,
            None => return core::ptr::null_mut(),
        };

        let task_ptr = head.as_ptr();
        let next = unsafe { *self.next_link(task_ptr) };

        self.head = next;

        // Clear the removed task's links so it does not appear
        // linked in the future.
        unsafe {
            *self.next_link(task_ptr) = None;
            *self.prev_link(task_ptr) = None;
        }

        match self.head {
            Some(mut new_head) => unsafe {
                *self.prev_link(new_head.as_ptr()) = None;
            },
            None => {
                self.tail = None;
            }
        }

        self.len -= 1;
        task_ptr
    }

    /// Removes a specific task from the list.
    ///
    /// # Safety
    ///
    /// `task` must point at a live `Task` that is currently a member
    /// of this list. The caller is responsible for this; `remove`
    /// does not check.
    pub unsafe fn remove(&mut self, task: *mut Task) {
        let prev = unsafe { *self.prev_link(task) };
        let next = unsafe { *self.next_link(task) };

        // Fix the previous task's next link, or the head if the
        // removed task was at the front.
        match prev {
            Some(mut prev) => unsafe {
                *self.next_link(prev.as_ptr()) = next;
            },
            None => {
                self.head = next;
            }
        }

        // Fix the next task's prev link, or the tail if the removed
        // task was at the back.
        match next {
            Some(mut next) => unsafe {
                *self.prev_link(next.as_ptr()) = prev;
            },
            None => {
                self.tail = prev;
            }
        }

        // Clear the removed task's links.
        unsafe {
            *self.next_link(task) = None;
            *self.prev_link(task) = None;
        }

        self.len -= 1;
    }

    /// Returns an iterator over the tasks in the list.
    ///
    /// # Safety
    ///
    /// The caller must ensure no task in the list is modified while
    /// the iterator is live.
    pub unsafe fn iter(&self) -> Iter<'_> {
        Iter {
            next: self.head,
            next_offset: self.next_offset,
            _marker: core::marker::PhantomData,
        }
    }
}

/// Iterator over the tasks in an `IntrusiveList`.
pub struct Iter<'a> {
    next: Option<NonNull<Task>>,
    next_offset: usize,
    _marker: core::marker::PhantomData<&'a Task>,
}

impl<'a> Iterator for Iter<'a> {
    type Item = &'a Task;

    fn next(&mut self) -> Option<Self::Item> {
        let current = self.next?;
        let task: &Task = unsafe { current.as_ref() };

        // Read the next link via the offset.
        let next_link = unsafe {
            &*((task as *const Task as *const u8).add(self.next_offset)
                as *const Option<NonNull<Task>>)
        };

        self.next = *next_link;
        Some(task)
    }
}