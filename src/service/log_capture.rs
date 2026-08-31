use std::sync::OnceLock;
use tokio::sync::broadcast;

/// A captured log entry.
#[derive(Clone, Debug, serde::Serialize)]
pub struct LogEntry {
    pub timestamp: String,
    pub level: String,
    pub target: String,
    pub message: String,
}

struct LogCapture {
    tx: broadcast::Sender<LogEntry>,
    /// Ring buffer of recent entries for the /api/logs endpoint.
    recent: parking_lot::Mutex<Vec<LogEntry>>,
}

const MAX_RECENT: usize = 500;

static CAPTURE: OnceLock<LogCapture> = OnceLock::new();

fn get_capture() -> &'static LogCapture {
    CAPTURE.get_or_init(|| {
        let (tx, _) = broadcast::channel(256);
        LogCapture {
            tx,
            recent: parking_lot::Mutex::new(Vec::with_capacity(MAX_RECENT)),
        }
    })
}

/// Push a log entry into the capture system.
/// Called from the custom log formatter.
pub fn push_log(level: &str, target: &str, message: &str) {
    let entry = LogEntry {
        timestamp: chrono::Utc::now().to_rfc3339(),
        level: level.to_string(),
        target: target.to_string(),
        message: message.to_string(),
    };

    let capture = get_capture();

    // Add to ring buffer
    {
        let mut recent = capture.recent.lock();
        if recent.len() >= MAX_RECENT {
            recent.remove(0);
        }
        recent.push(entry.clone());
    }

    // Broadcast to WebSocket subscribers (ignore if no receivers)
    let _ = capture.tx.send(entry);
}

/// Get the recent log entries.
pub fn recent_logs() -> Vec<LogEntry> {
    get_capture().recent.lock().clone()
}

/// Subscribe to live log entries.
pub fn subscribe() -> broadcast::Receiver<LogEntry> {
    get_capture().tx.subscribe()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_log_adds_to_recent_logs() {
        // Push a unique entry we can identify
        let unique_msg = format!(
            "test-push-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        push_log("INFO", "test_target", &unique_msg);

        let logs = recent_logs();
        assert!(
            logs.iter().any(|e| e.message == unique_msg),
            "Expected to find our pushed log entry"
        );
    }

    #[test]
    fn recent_logs_returns_pushed_entries() {
        let unique = format!(
            "recent-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        push_log("WARN", "my_target", &unique);

        let logs = recent_logs();
        let found = logs.iter().find(|e| e.message == unique).unwrap();
        assert_eq!(found.level, "WARN");
        assert_eq!(found.target, "my_target");
    }

    #[test]
    fn ring_buffer_caps_at_500_entries() {
        // Fill the buffer well beyond 500
        for i in 0..600 {
            push_log("DEBUG", "cap_test", &format!("entry-{i}"));
        }
        let logs = recent_logs();
        assert!(
            logs.len() <= MAX_RECENT,
            "Recent logs should not exceed {MAX_RECENT}, got {}",
            logs.len()
        );
    }

    #[tokio::test]
    async fn subscribe_receives_new_entries() {
        let mut rx = subscribe();
        let unique = format!(
            "subscribe-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        push_log("ERROR", "sub_target", &unique);

        // Drain messages until we find our unique one (other tests may
        // have pushed entries on the shared broadcast channel).
        loop {
            let entry = rx.recv().await.unwrap();
            if entry.message == unique {
                assert_eq!(entry.level, "ERROR");
                assert_eq!(entry.target, "sub_target");
                break;
            }
        }
    }
}
