//! Kernel heap tests.

use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::println;

/// Tests the kernel heap by allocating through Rust's `alloc`
/// types.
pub fn test_heap() {
    println!();
    println!("Testing kernel heap...");

    let boxed = Box::new(42u32);

    assert_eq!(*boxed, 42);

    println!("  Box<u32> = {}: SUCCESS", *boxed);

    let mut vec: Vec<u64> = Vec::new();

    for i in 0..100 {
        vec.push(i);
    }

    assert_eq!(vec.len(), 100);

    for i in 0..100 {
        assert_eq!(vec[i], i as u64);
    }

    println!("  Vec<u64> with 100 elements: SUCCESS");

    let mut big: Vec<u64> = Vec::new();

    for i in 0..100_000 {
        big.push(i as u64);
    }

    assert_eq!(big.len(), 100_000);
    assert_eq!(big[50_000], 50_000);
    assert_eq!(big[99_999], 99_999);

    println!("  Vec<u64> with 100 000 elements: SUCCESS");

    let committed = crate::memory::heap::committed();

    println!(
        "  Committed: {} bytes ({} KiB)",
        committed,
        committed / 1024
    );
    println!("  Kernel heap: SUCCESS");

    core::hint::black_box(&boxed);
    core::hint::black_box(&vec);
    core::hint::black_box(&big);
}