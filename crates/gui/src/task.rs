//! Work done on a background thread so the window stays responsive.

use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

pub struct Task<T> {
    rx: Receiver<T>,
    pub label: String,
    started: Instant,
}

impl<T: Send + 'static> Task<T> {
    pub fn spawn(label: impl Into<String>, ctx: &eframe::egui::Context, f: impl FnOnce() -> T + Send + 'static) -> Self {
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(f());
            ctx.request_repaint();
        });
        Task { rx, label: label.into(), started: Instant::now() }
    }

    pub fn poll(&self) -> Option<T> {
        self.rx.try_recv().ok()
    }

    /// How long this task has been running.
    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }
}
