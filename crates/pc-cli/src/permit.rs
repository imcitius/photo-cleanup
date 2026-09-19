//! A counting semaphore, used to keep concurrency where it helps and out of
//! where it hurts.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};

#[derive(Clone)]
pub struct Semaphore {
    inner: Arc<(Mutex<usize>, Condvar)>,
}

pub struct Permit<'a>(&'a Semaphore);

impl Semaphore {
    pub fn new(n: usize) -> Self {
        Self {
            inner: Arc::new((Mutex::new(n.max(1)), Condvar::new())),
        }
    }

    pub fn acquire(&self) -> Permit<'_> {
        let (lock, cv) = &*self.inner;
        let mut avail = lock.lock().unwrap();
        while *avail == 0 {
            avail = cv.wait(avail).unwrap();
        }
        *avail -= 1;
        Permit(self)
    }
}

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        let (lock, cv) = &*self.0.inner;
        *lock.lock().unwrap() += 1;
        cv.notify_one();
    }
}

/// One semaphore per physical disk.
///
/// An Unraid array is not striped: a file lives entirely on one spindle, so
/// several readers on one disk turn a sequential pass into seek thrash, while
/// readers on *different* disks scale almost linearly. Decoding is a separate
/// concern and runs on every core.
pub struct DiskPermits {
    per_disk: HashMap<u64, Semaphore>,
    readers_per_disk: usize,
}

impl DiskPermits {
    pub fn new(devs: impl IntoIterator<Item = u64>, readers_per_disk: usize) -> Self {
        Self {
            per_disk: devs
                .into_iter()
                .map(|d| (d, Semaphore::new(readers_per_disk)))
                .collect(),
            readers_per_disk,
        }
    }

    pub fn get(&self, dev: u64) -> Option<&Semaphore> {
        self.per_disk.get(&dev)
    }

    pub fn readers_per_disk(&self) -> usize {
        self.readers_per_disk
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn never_lets_more_than_n_through_at_once() {
        let sem = Semaphore::new(2);
        let live = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));

        std::thread::scope(|s| {
            for _ in 0..16 {
                let (sem, live, peak) = (sem.clone(), live.clone(), peak.clone());
                s.spawn(move || {
                    let _p = sem.acquire();
                    let now = live.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    std::thread::yield_now();
                    live.fetch_sub(1, Ordering::SeqCst);
                });
            }
        });

        assert!(peak.load(Ordering::SeqCst) <= 2, "одновременно {peak:?}");
    }

    #[test]
    fn a_permit_is_returned_when_dropped() {
        let sem = Semaphore::new(1);
        drop(sem.acquire());
        drop(sem.acquire());
    }
}
