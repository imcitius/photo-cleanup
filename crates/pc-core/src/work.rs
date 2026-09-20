//! Cooperative boundaries and a cheap, thread-safe progress snapshot.
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::Instant;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct DiskProgress {
    pub disk: String,
    pub done: u64,
    pub total: u64,
}
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Progress {
    pub phase: String,
    pub done: u64,
    pub total: u64,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub current: String,
    pub per_disk: Vec<DiskProgress>,
    pub eta_secs: Option<u64>,
    pub note: String,
    /// Which stage of a multi-stage job this is, 1-based. Zero when the job
    /// has only one stage, which is what the interface reads to decide
    /// whether "step 3 of 5" is worth showing at all.
    pub step: u32,
    pub steps: u32,
    pub refusals: Vec<(String, String)>,
}
#[derive(Debug, thiserror::Error)]
#[error("{}", crate::tr!("Остановлено пользователем на границе файла", "Stopped by the user at a file boundary"))]
pub struct Cancelled;
#[derive(Clone)]
pub struct Control {
    pub cancel: Arc<AtomicBool>,
    pub progress: Arc<Mutex<Progress>>,
    started: Arc<Mutex<Instant>>,
}
impl Default for Control {
    fn default() -> Self {
        Self {
            cancel: Arc::new(AtomicBool::new(false)),
            progress: Arc::new(Mutex::new(Progress::default())),
            started: Arc::new(Mutex::new(Instant::now())),
        }
    }
}
impl Control {
    pub fn check(&self) -> Result<()> {
        if self.cancel.load(Ordering::Relaxed) {
            Err(Cancelled.into())
        } else {
            Ok(())
        }
    }
    /// Announce which stage of a chain is starting. Left alone by `begin`,
    /// so a stage that begins several phases keeps its place in the chain.
    pub fn stage(&self, step: u32, steps: u32) {
        let mut p = self.progress.lock().unwrap();
        p.step = step;
        p.steps = steps;
    }
    pub fn begin(&self, phase: &str, total: u64, bytes: u64) -> Result<()> {
        self.check()?;
        let mut p = self.progress.lock().unwrap();
        *self.started.lock().unwrap() = Instant::now();
        p.current.clear();
        p.per_disk.clear();
        p.phase = phase.into();
        p.total = total;
        p.done = 0;
        p.bytes_total = bytes;
        p.bytes_done = 0;
        p.eta_secs = None;
        Ok(())
    }
    pub fn current(&self, path: &str) -> Result<()> {
        self.check()?;
        self.progress.lock().unwrap().current = path.into();
        Ok(())
    }
    pub fn advance(&self, bytes: u64, disk: Option<&str>) {
        let mut p = self.progress.lock().unwrap();
        p.done += 1;
        p.bytes_done += bytes;
        if p.done > 0 && p.total >= p.done {
            p.eta_secs = Some(
                (self.started.lock().unwrap().elapsed().as_secs_f64() / p.done as f64
                    * (p.total - p.done) as f64) as u64,
            );
        }
        if let Some(disk) = disk {
            if let Some(d) = p.per_disk.iter_mut().find(|d| d.disk == disk) {
                d.done += 1;
            }
        }
    }
    pub fn refuse(&self, path: &str, why: &str) {
        self.progress
            .lock()
            .unwrap()
            .refusals
            .push((path.into(), why.into()));
    }
}
