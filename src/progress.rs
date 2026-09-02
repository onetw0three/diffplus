//! Minimal dependency-free progress reporting for long-running analysis.

use std::sync::atomic::{AtomicBool, Ordering};

static ENABLED: AtomicBool = AtomicBool::new(true);

pub(crate) fn set_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
}

pub(crate) fn info(message: impl std::fmt::Display) {
    if ENABLED.load(Ordering::Relaxed) {
        eprintln!("[diffplus] {message}");
    }
}

pub(crate) fn subprocess(label: &str, stream: &str, message: &str) {
    if ENABLED.load(Ordering::Relaxed) {
        eprintln!("[diffplus] {label} {stream}: {message}");
    }
}
