use log::{error, warn};
use std::any::Any;
use std::future::Future;
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;

/// Delay before the first restart, doubled after each consecutive panic
const INITIAL_RESTART_DELAY: Duration = Duration::from_secs(1);
const MAX_RESTART_DELAY: Duration = Duration::from_secs(300);
/// If the task ran this long before panicking, the restart delay is reset
const HEALTHY_RUN: Duration = Duration::from_secs(600);

/// Aborts the inner task when dropped, so aborting the supervisor also stops the supervised task
struct AbortOnDrop<T>(JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Runs the future returned by `make` in its own tokio task, and starts it again if it panics.
///
/// A panic inside a `tokio::spawn` doesn't crash the process: the task just disappears, silently.
/// For the scheduler task loops that means a task that never runs again.
/// The supervisor returns when the future completes normally or when the supervisor itself is aborted.
pub async fn supervise<F, Fut>(name: String, make: F, initial_delay: Duration, max_delay: Duration)
where
    F: Fn() -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    let mut delay = initial_delay;

    loop {
        let start = Instant::now();
        let mut inner = AbortOnDrop(tokio::spawn(make()));

        match (&mut inner.0).await {
            Ok(()) => return,
            Err(e) if e.is_panic() => {
                if start.elapsed() >= HEALTHY_RUN {
                    delay = initial_delay;
                }
                error!(
                    "{} panicked: {}. Restarting in {}s",
                    name,
                    panic_message(&e.into_panic()),
                    delay.as_secs_f32()
                );
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(max_delay);
            }
            Err(_) => {
                warn!("{} was cancelled", name);
                return;
            }
        }
    }
}

/// Supervises with the default restart delays
pub async fn supervise_default<F, Fut>(name: String, make: F)
where
    F: Fn() -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    supervise(name, make, INITIAL_RESTART_DELAY, MAX_RESTART_DELAY).await
}

fn panic_message(payload: &Box<dyn Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "<unknown panic payload>".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    #[tokio::test]
    async fn restarts_after_panic_until_it_completes() {
        let runs = Arc::new(AtomicUsize::new(0));
        let runs2 = runs.clone();

        let make = move || {
            let runs = runs2.clone();
            async move {
                if runs.fetch_add(1, Ordering::SeqCst) < 2 {
                    panic!("boom");
                }
            }
        };
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            supervise("test".into(), make, Duration::from_millis(10), Duration::from_millis(20)),
        )
        .await;

        assert!(result.is_ok(), "supervisor did not finish");
        assert_eq!(runs.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn aborting_the_supervisor_aborts_the_inner_task() {
        struct SetOnDrop(Arc<AtomicBool>);
        impl Drop for SetOnDrop {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let dropped = Arc::new(AtomicBool::new(false));
        let dropped2 = dropped.clone();
        let make = move || {
            let guard = SetOnDrop(dropped2.clone());
            async move {
                let _guard = guard;
                tokio::time::sleep(Duration::from_secs(3600)).await;
            }
        };

        let handle = tokio::spawn(supervise_default("test".into(), make));
        tokio::time::sleep(Duration::from_millis(50)).await;
        handle.abort();
        let _ = handle.await;
        tokio::time::sleep(Duration::from_millis(50)).await;

        assert!(dropped.load(Ordering::SeqCst), "inner task still running after abort");
    }
}
