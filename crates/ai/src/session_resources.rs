use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

type CleanupFn = Box<dyn Fn(Option<&str>) + Send + Sync + 'static>;

pub type SessionResourceCleanup = dyn Fn(Option<&str>) + Send + Sync + 'static;

#[derive(Debug, thiserror::Error)]
#[error("Failed to cleanup session resources ({panic_count} panic(s))")]
pub struct SessionResourceCleanupError {
    pub panic_count: usize,
}

fn session_resource_cleanups() -> &'static Mutex<Vec<(usize, CleanupFn)>> {
    static CLEANUPS: OnceLock<Mutex<Vec<(usize, CleanupFn)>>> = OnceLock::new();
    CLEANUPS.get_or_init(|| Mutex::new(Vec::new()))
}

fn next_cleanup_id() -> usize {
    static NEXT_ID: AtomicUsize = AtomicUsize::new(0);
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

pub fn register_session_resource_cleanup(
    cleanup: impl Fn(Option<&str>) + Send + Sync + 'static,
) -> impl FnOnce() + Send + Sync + 'static {
    let cleanup_id = next_cleanup_id();
    let mut cleanups = session_resource_cleanups().lock().unwrap();
    cleanups.push((cleanup_id, Box::new(cleanup)));

    move || {
        session_resource_cleanups()
            .lock()
            .unwrap()
            .retain(|(candidate_id, _)| *candidate_id != cleanup_id);
    }
}

pub fn cleanup_session_resources(
    session_id: Option<&str>,
) -> Result<(), SessionResourceCleanupError> {
    let cleanups = session_resource_cleanups().lock().unwrap();
    let mut panic_count = 0;
    for (_, cleanup) in cleanups.iter() {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cleanup(session_id)));
        if result.is_err() {
            panic_count += 1;
        }
    }

    if panic_count == 0 {
        Ok(())
    } else {
        Err(SessionResourceCleanupError { panic_count })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn cleanup_invokes_registered_callbacks_with_session_id() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let calls_clone = calls.clone();
        let unregister = register_session_resource_cleanup(move |session_id| {
            calls_clone
                .lock()
                .unwrap()
                .push(session_id.map(str::to_string));
        });

        cleanup_session_resources(Some("session-1")).unwrap();
        unregister();

        assert_eq!(
            calls.lock().unwrap().as_slice(),
            &[Some("session-1".to_string())]
        );
    }

    #[test]
    fn unregister_removes_only_target_cleanup() {
        let calls = Arc::new(Mutex::new(Vec::new()));

        let calls_first = calls.clone();
        let first = register_session_resource_cleanup(move |_| {
            calls_first.lock().unwrap().push("first".to_string());
        });

        let calls_second = calls.clone();
        let second = register_session_resource_cleanup(move |_| {
            calls_second.lock().unwrap().push("second".to_string());
        });

        let calls_third = calls.clone();
        let _third = register_session_resource_cleanup(move |_| {
            calls_third.lock().unwrap().push("third".to_string());
        });

        first();
        second();

        cleanup_session_resources(None).unwrap();

        assert_eq!(calls.lock().unwrap().as_slice(), &["third".to_string()]);
    }

    #[test]
    fn cleanup_aggregates_panicking_callbacks() {
        let ok = register_session_resource_cleanup(|_| {});
        let panics = register_session_resource_cleanup(|_| panic!("boom"));

        let error = cleanup_session_resources(None).expect_err("panic count");

        ok();
        panics();

        assert_eq!(error.panic_count, 1);
    }
}
