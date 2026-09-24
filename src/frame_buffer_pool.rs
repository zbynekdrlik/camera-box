//! #280 / #1242 — a bounded pool of reusable frame buffers.
//!
//! A frame that crosses from the capture thread to another thread (the E2E burn thread since
//! #275b/#280, the production NDI send thread since #1242) must be COPIED off the V4L2 mmap, which
//! is only valid inside the capture callback. A per-frame `Vec::to_vec` (~4 MB at 1080p YUYV) is a
//! fresh heap allocation + free on every emitted frame at up to 60 fps. This pool recycles those
//! buffers: the capture thread [`take`](BufferPool::take)s a buffer (reusing a returned one, or
//! allocating only when the free list is empty), copies the frame in, and hands it over; the
//! receiving thread [`put`](BufferPool::put)s it back after its NDI send. The free list is BOUNDED
//! (its `cap`) so it can never grow without limit — a `put` over the cap simply drops the buffer
//! (it is freed). Memory is then bounded by the peak in-flight count instead of churning one
//! allocation per frame.
//!
//! This is a pure MEMORY optimization: it carries no frame identity, so it cannot change the frame
//! order or the carried timecode. Shared between the two threads via `Arc`. Moved here from
//! `probe::genlock` (which re-exports it) so the production send path can use it too.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

/// A bounded free list of reusable frame buffers.
pub struct BufferPool {
    free: Mutex<Vec<Vec<u8>>>,
    cap: usize,
    /// Count of FRESH allocations [`take`](Self::take) had to make (free list was empty). After
    /// warm-up this stops climbing — that flat count is the proof the pool recycles rather than
    /// allocating per frame.
    allocated: AtomicUsize,
}

impl BufferPool {
    /// Create an empty pool whose free list is bounded at `cap` buffers.
    pub fn new(cap: usize) -> Self {
        Self {
            free: Mutex::new(Vec::new()),
            cap,
            allocated: AtomicUsize::new(0),
        }
    }

    /// Take a buffer to copy a frame into: reuse a returned one when the free list is non-empty,
    /// else allocate a fresh `Vec` (and count it). The caller `clear()`s + fills it; a reused
    /// buffer keeps its ~4 MB capacity so the fill does not reallocate.
    pub fn take(&self) -> Vec<u8> {
        // Drop the lock BEFORE allocating on the empty path: pop releases the mutex, then the
        // fresh `Vec::new` (+ the counter bump) runs unlocked so it never holds the lock against
        // the other thread's `put`.
        let popped = self.free.lock().unwrap().pop();
        match popped {
            Some(buf) => buf,
            None => {
                self.allocated.fetch_add(1, Ordering::Relaxed);
                Vec::new()
            }
        }
    }

    /// Return a buffer for reuse after its send. BOUNDED: if the free list is already at `cap`,
    /// drop the buffer (it is freed) so the pool can never grow without limit.
    pub fn put(&self, buf: Vec<u8>) {
        let mut free = self.free.lock().unwrap();
        if free.len() < self.cap {
            free.push(buf);
        }
        // else: at capacity — drop `buf` (freed). Bounds the pool's memory.
    }

    /// Number of FRESH allocations [`take`](Self::take) has made (free list empty). A count that
    /// stays flat after warm-up proves the pool recycles instead of allocating per frame.
    pub fn allocations(&self) -> usize {
        self.allocated.load(Ordering::Relaxed)
    }

    /// Current number of idle buffers held in the free list (≤ `cap`).
    pub fn free_len(&self) -> usize {
        self.free.lock().unwrap().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_returned_buffer_is_reused_with_its_capacity_1242() {
        let pool = BufferPool::new(3);
        let mut b = pool.take();
        b.extend_from_slice(&[1u8; 4096]);
        let cap = b.capacity();
        pool.put(b);
        let again = pool.take();
        assert!(
            again.capacity() >= cap,
            "the recycled buffer keeps its capacity"
        );
        assert_eq!(
            pool.allocations(),
            1,
            "the second take reused the first buffer"
        );
    }

    #[test]
    fn the_free_list_never_grows_past_its_cap_1242() {
        let pool = BufferPool::new(2);
        for _ in 0..5 {
            pool.put(Vec::with_capacity(16));
        }
        assert_eq!(pool.free_len(), 2);
    }
}
