//! Keeping the machine usable while the archive is read.
//!
//! Indexing is a batch job: it decodes every photograph in the archive and
//! will happily take every core for an hour. Nothing about it is urgent, so
//! its threads ask the scheduler for the back of the queue — the work still
//! uses idle capacity, but a browser, an editor or a video call always wins
//! the contest for a core, and the disk queue stays clear for them too.

/// Move the calling thread to a background scheduling class.
///
/// macOS has an explicit class for exactly this. It lowers the thread's CPU
/// priority *and* puts its disk reads in the throttled I/O tier, which is
/// what keeps the rest of the machine responsive while gigabytes stream past.
/// Windows has the same idea under a different name. Elsewhere a `nice` value
/// is the portable equivalent; on Linux it applies per thread, which is what
/// we want.
pub fn background_thread() {
    #[cfg(target_vendor = "apple")]
    unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_UTILITY, 0);
    }
    #[cfg(all(unix, not(target_vendor = "apple")))]
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, 0, 10);
    }
    #[cfg(windows)]
    unsafe {
        // BEGIN lowers CPU priority and the I/O priority together, and holds
        // until the matching END — which never comes, because the thread is
        // a decoder that exists only for this work.
        windows_sys::Win32::System::Threading::SetThreadPriority(
            windows_sys::Win32::System::Threading::GetCurrentThread(),
            windows_sys::Win32::System::Threading::THREAD_MODE_BACKGROUND_BEGIN,
        );
    }
}

/// How many decoding threads to run by default.
///
/// One core is left free on purpose: a pool sized to every core starves the
/// thread that writes to the database, and the machine feels seized even
/// though the scheduler class says otherwise.
pub fn default_workers() -> usize {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    cores.saturating_sub(1).max(1)
}

/// A rayon pool whose threads all run in the background class.
pub fn pool(workers: usize) -> rayon::ThreadPool {
    rayon::ThreadPoolBuilder::new()
        .num_threads(workers.max(1))
        .thread_name(|i| format!("pc-index-{i}"))
        .start_handler(|_| background_thread())
        .build()
        .expect("не создать пул потоков")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_worker_is_always_left_and_never_zero() {
        assert!(default_workers() >= 1);
    }

    #[test]
    fn the_pool_runs_work_on_the_requested_number_of_threads() {
        let pool = pool(2);
        assert_eq!(pool.current_num_threads(), 2);
        assert_eq!(pool.install(|| 1 + 1), 2);
    }
}
