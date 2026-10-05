//! `shared::archive::reviewed` runs on every upload, unauthenticated and
//! before payment. Numbering costs at least five bytes a line (`"   1 "` and
//! the newline), so text numbered whole before the budget is checked grows
//! about nine times over for a file of empty lines, and the build service's
//! memory with it.
//!
//! Sequence: an archive holding a 4 MiB file of empty lines, named as a
//! model is (every file counts); `reviewed` is called. Expected: it is
//! refused having held no more than the budget, never the whole numbered
//! text.

// Counting what is allocated needs a global allocator, the only unsafe here.
#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Counting;

static NOW: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let now = NOW.fetch_add(layout.size(), Ordering::SeqCst) + layout.size();
        PEAK.fetch_max(now, Ordering::SeqCst);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        NOW.fetch_sub(layout.size(), Ordering::SeqCst);
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if new_size > layout.size() {
            let now = NOW.fetch_add(new_size - layout.size(), Ordering::SeqCst) + new_size
                - layout.size();
            PEAK.fetch_max(now, Ordering::SeqCst);
        } else {
            NOW.fetch_sub(layout.size() - new_size, Ordering::SeqCst);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

#[test]
fn a_project_past_what_the_reviewer_reads_is_refused_before_it_is_numbered_whole() {
    let root = std::env::temp_dir().join(format!("shonin-past-the-budget-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("Cargo.toml"), "[package]\nname = \"agent\"\n").unwrap();
    let lines = 4 * 1024 * 1024;
    std::fs::write(root.join("model.onnx"), vec![b'\n'; lines]).unwrap();
    let archive = shared::archive::pack(&root).unwrap();
    let _ = std::fs::remove_dir_all(&root);

    let before = NOW.load(Ordering::SeqCst);
    PEAK.store(before, Ordering::SeqCst);
    let refused = shared::archive::reviewed(&archive);
    let used = PEAK.load(Ordering::SeqCst) - before;

    assert!(refused.is_err(), "a project past the budget was read");
    // What it needs: the budget's worth of one file's bytes, and the budget
    // itself (twice, for growth); anything past that is spent on text that is
    // thrown away.
    let room = 3 * shared::archive::MOST_REVIEWED_BYTES + 1024 * 1024;
    assert!(
        used <= room,
        "refusing a {} KiB archive took {} MiB of memory (room: {} MiB): the budget is checked \
         only after the text is numbered whole",
        archive.len() / 1024,
        used / (1024 * 1024),
        room / (1024 * 1024),
    );
}
