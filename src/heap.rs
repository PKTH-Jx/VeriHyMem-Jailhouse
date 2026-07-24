//! Rust heap hooks supplied by the Jailhouse side of the final static link.
//!
//! This allocator backs Rust-owned metadata such as `Box` and `Vec`. It is
//! independent of VeriHyMem's `GlobalFrameAllocator`, which allocates page-table
//! frames from its own dedicated frame pool.

use core::alloc::{GlobalAlloc, Layout};

unsafe extern "C" {
    fn vj_heap_alloc(size: usize, align: usize) -> *mut u8;
    fn vj_heap_dealloc(ptr: *mut u8, size: usize, align: usize);
}

pub struct JailhouseHeapAllocator;

unsafe impl GlobalAlloc for JailhouseHeapAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        unsafe { vj_heap_alloc(layout.size(), layout.align()) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { vj_heap_dealloc(ptr, layout.size(), layout.align()) }
    }
}

#[cfg(not(test))]
#[global_allocator]
static GLOBAL_HEAP_ALLOCATOR: JailhouseHeapAllocator = JailhouseHeapAllocator;
