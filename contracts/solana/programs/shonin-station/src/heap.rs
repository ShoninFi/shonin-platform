//! The station's heap.
//!
//! A program's default allocator hands out the 32 KiB the runtime maps for
//! every call, from the top down, and never frees anything; a close asks
//! every driver several times, and each ask allocates, so a close through
//! several drivers would run out of it. This allocator instead grows upwards
//! from the start of the heap, as far as the transaction's heap frame
//! reaches, up to the [`MAX_HEAP`] a transaction may ask for: within the 32
//! KiB every call has it works without asking, and past what the transaction
//! asked for the call fails, as running out of the default heap does. So the
//! station's callers ask for [`MAX_HEAP`] (the vault included), and the
//! station's limits are measured with it.
//!
//! It also gives back the most recent allocation when it is freed, and grows
//! the most recent one in place, so an ask's temporaries and a growing
//! vector don't pile up.

use std::alloc::{GlobalAlloc, Layout};
use std::ptr::null_mut;

/// The most heap a transaction may ask for (`RequestHeapFrame`), which every
/// caller of the station asks for.
pub const MAX_HEAP: usize = 256 * 1024;

/// Where the runtime maps the heap.
const START: usize = 0x3_0000_0000;
/// The first word of the heap holds the address of the first free byte (zero
/// at the start of a call, when the heap is all zeroes); allocations follow.
const FIRST: usize = START + core::mem::size_of::<usize>();
const END: usize = START + MAX_HEAP;

pub struct Heap;

impl Heap {
    /// The first free byte.
    unsafe fn free(&self) -> usize {
        let free = *(START as *const usize);
        if free == 0 {
            FIRST
        } else {
            free
        }
    }

    unsafe fn set_free(&self, at: usize) {
        *(START as *mut usize) = at;
    }
}

unsafe impl GlobalAlloc for Heap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let mask = layout.align() - 1;
        let Some(at) = self.free().checked_add(mask).map(|at| at & !mask) else {
            return null_mut();
        };
        match at.checked_add(layout.size()) {
            Some(end) if end <= END => {
                self.set_free(end);
                at as *mut u8
            }
            _ => null_mut(),
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // Only the most recent allocation can be given back.
        if ptr as usize + layout.size() == self.free() {
            self.set_free(ptr as usize);
        }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let at = ptr as usize;
        if at + layout.size() == self.free() {
            // The most recent allocation grows or shrinks where it is.
            return match at.checked_add(new_size) {
                Some(end) if end <= END => {
                    self.set_free(end);
                    ptr
                }
                _ => null_mut(),
            };
        }
        let new_layout = Layout::from_size_align_unchecked(new_size, layout.align());
        let moved = self.alloc(new_layout);
        if !moved.is_null() {
            core::ptr::copy_nonoverlapping(ptr, moved, layout.size().min(new_size));
        }
        moved
    }
}
