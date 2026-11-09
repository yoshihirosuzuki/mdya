//! Running what blocks without holding up the other async work.

use std::panic::resume_unwind;

use tokio::task::{JoinError, spawn_blocking};

/// Runs `work` on a blocking thread and waits for it. A panic in `work`
/// reaches the caller as that panic.
pub(super) async fn run_blocking<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> T {
    match spawn_blocking(work).await {
        Ok(value) => value,
        Err(error) => resume_panic(error),
    }
}

#[expect(
    clippy::panic,
    reason = "a blocking task that is awaited ends without a panic only when the runtime shuts down, which the update cannot carry on from"
)]
fn resume_panic(error: JoinError) -> ! {
    match error.try_into_panic() {
        Ok(panic) => resume_unwind(panic),
        Err(error) => panic!(
            "reading the archive was cancelled, which happens only when the runtime shuts down: {error}"
        ),
    }
}
