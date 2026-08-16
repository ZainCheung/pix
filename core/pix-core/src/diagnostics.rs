//! Payload-free host timing records for home and session loading.
//!
//! Records contain only event names and numeric fields (durations, counts,
//! byte lengths). Paths, session names, message text, keys, tokens, and
//! channel secrets must never appear.

use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use serde_json::{Map, Value};

type Sink = Box<dyn Fn(&str, &Value) + Send + Sync>;

static SINK: OnceLock<Mutex<Option<Sink>>> = OnceLock::new();

/// Milliseconds since `started`, saturating at `u64::MAX`.
#[must_use]
pub fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Installs the process-wide diagnostic sink. Later calls replace it.
///
/// The host CLI wires this to the payload-free JSONL log. Tests leave it
/// unset and read thread-local captures instead.
pub fn install_sink<F>(sink: F)
where
    F: Fn(&str, &Value) + Send + Sync + 'static,
{
    let mut guard = sink_slot()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *guard = Some(Box::new(sink));
}

/// Records one numeric-only timing event.
pub fn record(event: &'static str, fields: &[(&'static str, u64)]) {
    let mut body = Map::new();
    for (key, value) in fields {
        body.insert((*key).to_owned(), Value::from(*value));
    }
    let body = Value::Object(body);

    #[cfg(test)]
    capture_for_thread(event, &body);

    if let Some(sink) = sink_slot()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
    {
        sink(event, &body);
    }
}

fn sink_slot() -> &'static Mutex<Option<Sink>> {
    SINK.get_or_init(|| Mutex::new(None))
}

#[cfg(test)]
thread_local! {
    static CAPTURE: std::cell::RefCell<Vec<(String, Value)>> = const {
        std::cell::RefCell::new(Vec::new())
    };
}

#[cfg(test)]
fn capture_for_thread(event: &str, body: &Value) {
    CAPTURE.with(|cell| cell.borrow_mut().push((event.to_owned(), body.clone())));
}

#[cfg(test)]
pub(crate) fn take_thread_records() -> Vec<(String, Value)> {
    CAPTURE.with(|cell| std::mem::take(&mut *cell.borrow_mut()))
}

#[cfg(test)]
mod tests {
    use super::{elapsed_ms, record, take_thread_records};
    use std::time::Instant;

    #[test]
    fn records_are_numeric_and_payload_free() {
        let _ = take_thread_records();
        record(
            "session.list",
            &[
                ("enumerate_ms", 3),
                ("scan_ms", 12),
                ("file_count", 4),
                ("session_count", 2),
                ("response_bytes", 256),
            ],
        );
        let records = take_thread_records();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].0, "session.list");
        let rendered = records[0].1.to_string();
        assert!(rendered.contains("\"scan_ms\":12"));
        for forbidden in [
            "secret", "channel", "prompt", "message", "cwd", "path", "token", "proof", "/",
        ] {
            assert!(
                !rendered.contains(forbidden),
                "timing record leaked {forbidden}: {rendered}"
            );
        }
    }

    #[test]
    fn elapsed_ms_is_monotonic_and_small_for_instant_work() {
        let started = Instant::now();
        let elapsed = elapsed_ms(started);
        assert!(elapsed < 1_000);
    }
}
