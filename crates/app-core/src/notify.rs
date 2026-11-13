//! UI wake-up boundary used by background workers.

use std::sync::Arc;

/// A background worker only needs to tell the UI that new events are ready.
pub trait UiNotifier: Send + Sync + 'static {
    fn notify(&self);
}

pub type Notifier = Arc<dyn UiNotifier>;

/// Useful for headless controller tests and command-line self tests.
pub struct NoopNotifier;

impl UiNotifier for NoopNotifier {
    fn notify(&self) {}
}

pub fn noop() -> Notifier {
    Arc::new(NoopNotifier)
}
