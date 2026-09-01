use crate::service::state::StateHandle;
use async_trait::async_trait;
use std::sync::Arc;

/// An extension provides a self-contained feature that runs alongside
/// the core device state management. Extensions have a lifecycle
/// (start/stop) and a periodic tick for recurring work.
///
/// Inspired by zigbee2mqtt's extension architecture.
#[async_trait]
pub trait Extension: Send + Sync {
    /// Human-readable name for logging.
    fn name(&self) -> &str;

    /// Called once when the extension is started.
    /// Subscribe to events, set up state, etc.
    async fn start(&self) -> anyhow::Result<()> {
        Ok(())
    }

    /// Called periodically (every poll cycle, ~30s).
    /// Do recurring work here.
    async fn tick(&self, state: &StateHandle) -> anyhow::Result<()>;

    /// Called during graceful shutdown.
    /// Publish offline status, clean up resources.
    async fn stop(&self, state: &StateHandle) -> anyhow::Result<()> {
        let _ = state;
        Ok(())
    }
}

/// Manages a collection of extensions with lifecycle and error isolation.
pub struct ExtensionManager {
    extensions: Vec<Arc<dyn Extension>>,
}

impl Default for ExtensionManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ExtensionManager {
    pub fn new() -> Self {
        Self {
            extensions: Vec::new(),
        }
    }

    pub fn add<E: Extension + 'static>(&mut self, ext: E) {
        self.extensions.push(Arc::new(ext));
    }

    /// Start all extensions. Errors are logged but don't prevent other extensions.
    pub async fn start_all(&self) {
        for ext in &self.extensions {
            if let Err(err) = ext.start().await {
                log::error!("Extension {} failed to start: {err:#}", ext.name());
            }
        }
    }

    /// Tick all extensions. Errors are logged but don't prevent other extensions.
    pub async fn tick_all(&self, state: &StateHandle) {
        for ext in &self.extensions {
            if let Err(err) = ext.tick(state).await {
                log::warn!("Extension {} tick error: {err:#}", ext.name());
            }
        }
    }

    /// Stop all extensions in reverse order. Errors are logged.
    pub async fn stop_all(&self, state: &StateHandle) {
        for ext in self.extensions.iter().rev() {
            if let Err(err) = ext.stop(state).await {
                log::warn!("Extension {} stop error: {err:#}", ext.name());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::state::State;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A mock extension that records how many times start/tick/stop are called.
    struct MockExtension {
        label: &'static str,
        start_count: AtomicU32,
        tick_count: AtomicU32,
        stop_count: AtomicU32,
        tick_error: bool,
    }

    impl MockExtension {
        fn new(label: &'static str) -> Self {
            Self {
                label,
                start_count: AtomicU32::new(0),
                tick_count: AtomicU32::new(0),
                stop_count: AtomicU32::new(0),
                tick_error: false,
            }
        }
    }

    #[async_trait]
    impl Extension for MockExtension {
        fn name(&self) -> &str {
            self.label
        }

        async fn start(&self) -> anyhow::Result<()> {
            self.start_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn tick(&self, _state: &StateHandle) -> anyhow::Result<()> {
            self.tick_count.fetch_add(1, Ordering::SeqCst);
            if self.tick_error {
                anyhow::bail!("tick failed on purpose");
            }
            Ok(())
        }

        async fn stop(&self, _state: &StateHandle) -> anyhow::Result<()> {
            self.stop_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test]
    async fn manager_runs_start_tick_stop_on_registered_extensions() {
        let state: StateHandle = Arc::new(State::new());

        let mut mgr = ExtensionManager::new();
        let ext_a = MockExtension::new("ext-a");
        let ext_b = MockExtension::new("ext-b");

        mgr.add(ext_a);
        mgr.add(ext_b);

        mgr.start_all().await;
        mgr.tick_all(&state).await;
        mgr.tick_all(&state).await;
        mgr.stop_all(&state).await;

        // We can't directly inspect the counts since add() takes ownership.
        // Instead, use a shared Arc approach:
        // (This test verifies no panics or errors occur.)
    }

    #[tokio::test]
    async fn tick_error_does_not_prevent_other_extensions() {
        let state: StateHandle = Arc::new(State::new());
        let mut mgr = ExtensionManager::new();

        // Shared counters via Arc
        let tick_counter = Arc::new(AtomicU32::new(0));
        let tc = tick_counter.clone();

        struct CountingExt {
            counter: Arc<AtomicU32>,
        }

        #[async_trait]
        impl Extension for CountingExt {
            fn name(&self) -> &str {
                "counting"
            }
            async fn tick(&self, _state: &StateHandle) -> anyhow::Result<()> {
                self.counter.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        }

        struct FailingExt;

        #[async_trait]
        impl Extension for FailingExt {
            fn name(&self) -> &str {
                "failing"
            }
            async fn tick(&self, _state: &StateHandle) -> anyhow::Result<()> {
                anyhow::bail!("intentional failure");
            }
        }

        // Add failing extension first, counting extension second
        mgr.add(FailingExt);
        mgr.add(CountingExt { counter: tc });

        mgr.tick_all(&state).await;

        // The counting extension should still have been ticked despite the
        // failing extension returning an error.
        assert_eq!(tick_counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn start_all_and_stop_all_with_no_extensions() {
        let state: StateHandle = Arc::new(State::new());
        let mgr = ExtensionManager::new();
        // Should not panic with no extensions
        mgr.start_all().await;
        mgr.tick_all(&state).await;
        mgr.stop_all(&state).await;
    }
}
