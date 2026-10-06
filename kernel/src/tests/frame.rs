//! Frame allocator tests.

use crate::arch;
use crate::println;

/// Tests that the frame allocator never hands out a frame that
/// overlaps any bootstrap region.
pub fn test_frame_allocator() {
    println!();
    println!("Testing frame allocator isolation...");

    for _ in 0..16 {
        let frame = crate::memory::frame::allocate().expect("frame allocation failed");
        let address = frame.address();

        assert!(
            address < arch::__bootstrap_start() || address >= arch::__bootstrap_stack_top(),
            "frame allocator returned a bootstrap frame: 0x{:08x}",
            address
        );

        println!("  Allocated frame: 0x{:08x}", address);
    }

    println!("  Frame allocator isolation: SUCCESS");
}
