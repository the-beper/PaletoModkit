//! Work done on a background thread so the window stays responsive.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

pub struct Task<T> {
    rx: Receiver<T>,
    pub label: String,
    started: Instant,
    cancel: Cancel,
    cancellable: bool,
}

/// Set once nobody wants a task's result: a worker that checks it stops early.
#[derive(Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn is_set(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

impl<T: Send + 'static> Task<T> {
    pub fn spawn(label: impl Into<String>, ctx: &eframe::egui::Context, f: impl FnOnce() -> T + Send + 'static) -> Self {
        let mut task = Self::spawn_cancellable(label, ctx, move |_| f());
        task.cancellable = false;
        task
    }

    /// A task the status bar can cancel; `f` checks the flag as it goes.
    pub fn spawn_cancellable(label: impl Into<String>, ctx: &eframe::egui::Context, f: impl FnOnce(&Cancel) -> T + Send + 'static) -> Self {
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        let cancel = Cancel::default();
        let flag = cancel.clone();
        std::thread::spawn(move || {
            let _ = tx.send(f(&flag));
            ctx.request_repaint();
        });
        Task { rx, label: label.into(), started: Instant::now(), cancel, cancellable: true }
    }

    pub fn poll(&self) -> Option<T> {
        self.rx.try_recv().ok()
    }

    /// How long this task has been running.
    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    pub fn cancellable(&self) -> bool {
        self.cancellable
    }
}

/// A task dropped (replaced, cancelled, its window closed) is told to stop;
/// one that does not check runs to its end, and its result is dropped.
impl<T> Drop for Task<T> {
    fn drop(&mut self) {
        self.cancel.0.store(true, Ordering::Relaxed);
    }
}
