//! Runtime hooks supplied by the Jailhouse side of the final static link.

use core::alloc::{GlobalAlloc, Layout};

unsafe extern "C" {
    fn verihymem_jailhouse_alloc(size: usize, align: usize) -> *mut u8;
    fn verihymem_jailhouse_dealloc(ptr: *mut u8, size: usize, align: usize);
}

pub struct JailhouseAllocator;

unsafe impl GlobalAlloc for JailhouseAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        unsafe { verihymem_jailhouse_alloc(layout.size(), layout.align()) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { verihymem_jailhouse_dealloc(ptr, layout.size(), layout.align()) }
    }
}

#[cfg(not(test))]
#[global_allocator]
static GLOBAL_ALLOCATOR: JailhouseAllocator = JailhouseAllocator;

