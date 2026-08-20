//! Bounded, drop-oldest event buffer shared by log and terminal streams.
//!
//! A pod that logs in a tight loop can outrun the UI by orders of magnitude.
//! An unbounded channel turns that into unbounded memory; a bounded channel
//! that drops the *newest* line makes follow-mode useless, because the newest
//! line is the one the user is watching for. So the ring drops the oldest and
//! records how many were lost, and the consumer renders that as an explicit
//! marker rather than silently showing a gap.

use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use parking_lot::Mutex;
use tokio::sync::Notify;

pub struct Ring<T> {
    items: Mutex<VecDeque<T>>,
    dropped: AtomicU64,
    notify: Notify,
    capacity: usize,
    closed: AtomicBool,
}

impl<T> Ring<T> {
    pub fn new(capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            items: Mutex::new(VecDeque::with_capacity(capacity.min(1024))),
            dropped: AtomicU64::new(0),
            notify: Notify::new(),
            capacity,
            closed: AtomicBool::new(false),
        })
    }

    /// Append an item, evicting the oldest if the ring is full.
    pub fn push(&self, item: T) {
        {
            let mut items = self.items.lock();
            if items.len() >= self.capacity {
                items.pop_front();
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
            items.push_back(item);
        }
        self.notify.notify_one();
    }

    /// Mark the producer side finished; a waiting consumer wakes and ends.
    pub fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// Take everything buffered plus the number of items dropped since the last
    /// drain. Returns `None` when nothing is pending.
    pub fn drain(&self) -> Option<(Vec<T>, u64)> {
        let mut items = self.items.lock();
        if items.is_empty() {
            return None;
        }
        let batch: Vec<T> = items.drain(..).collect();
        let dropped = self.dropped.swap(0, Ordering::Relaxed);
        Some((batch, dropped))
    }

    /// Wait for the next non-empty batch. `None` once closed and drained.
    pub async fn next_batch(&self) -> Option<(Vec<T>, u64)> {
        loop {
            // Register interest *before* checking, otherwise a push between the
            // check and the await would be missed.
            let notified = self.notify.notified();
            if let Some(batch) = self.drain() {
                return Some(batch);
            }
            if self.is_closed() {
                return None;
            }
            notified.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_oldest_and_counts() {
        let ring: Arc<Ring<u32>> = Ring::new(3);
        for i in 0..5 {
            ring.push(i);
        }
        let (batch, dropped) = ring.drain().unwrap();
        assert_eq!(batch, vec![2, 3, 4], "newest items survive");
        assert_eq!(dropped, 2);
        assert!(ring.drain().is_none());
    }

    #[tokio::test]
    async fn next_batch_wakes_on_push() {
        let ring: Arc<Ring<u32>> = Ring::new(8);
        let writer = ring.clone();
        tokio::spawn(async move {
            writer.push(7);
        });
        let (batch, dropped) = ring.next_batch().await.unwrap();
        assert_eq!(batch, vec![7]);
        assert_eq!(dropped, 0);
    }

    #[tokio::test]
    async fn next_batch_ends_when_closed() {
        let ring: Arc<Ring<u32>> = Ring::new(8);
        let closer = ring.clone();
        tokio::spawn(async move {
            closer.close();
        });
        assert!(ring.next_batch().await.is_none());
    }

    #[tokio::test]
    async fn close_still_yields_buffered_items() {
        let ring: Arc<Ring<u32>> = Ring::new(8);
        ring.push(1);
        ring.close();
        assert_eq!(ring.next_batch().await.unwrap().0, vec![1]);
        assert!(ring.next_batch().await.is_none());
    }
}
