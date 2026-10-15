//! Concurrency control for outbound work: a semaphore with a bounded wait
//! queue, and coalescing of identical in-flight jobs.

use std::collections::HashMap;
use std::future::Future;
use std::hash::Hash;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use futures::FutureExt;
use futures::future::{BoxFuture, Shared};
use tokio::sync::Semaphore;

/// Work was rejected because every slot was busy and the queue was full.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueFull;

/// A counting semaphore with a bounded wait queue.
///
/// Limits how many tasks run at once. When every slot is busy, further tasks
/// wait, but only up to `queue_limit` of them. Past that, [`Gate::run`]
/// rejects immediately rather than growing the queue without end, which
/// under load only converts a fast failure into a slow one.
pub struct Gate {
    slots: Semaphore,
    waiting: AtomicUsize,
    queue_limit: usize,
}

impl Gate {
    pub fn new(limit: usize, queue_limit: usize) -> Self {
        assert!(limit >= 1, "Gate limit must be at least 1.");
        Self {
            slots: Semaphore::new(limit),
            waiting: AtomicUsize::new(0),
            queue_limit,
        }
    }

    /// Run `fut` once a slot is free, releasing the slot when it settles.
    pub async fn run<T>(&self, fut: impl Future<Output = T>) -> Result<T, QueueFull> {
        let permit = match self.slots.try_acquire() {
            Ok(permit) => permit,
            Err(_) => {
                if self.waiting.fetch_add(1, Ordering::SeqCst) >= self.queue_limit {
                    self.waiting.fetch_sub(1, Ordering::SeqCst);
                    return Err(QueueFull);
                }
                // Tokio's semaphore is fair, so waiters are served in order.
                let permit = self.slots.acquire().await;
                self.waiting.fetch_sub(1, Ordering::SeqCst);
                permit.expect("semaphore is never closed")
            }
        };
        let out = fut.await;
        drop(permit);
        Ok(out)
    }
}

/// Runs at most one job per key at a time; everyone who asks for a key while
/// its job runs awaits the same result.
///
/// Jobs are spawned rather than polled by the caller, because they are
/// shared: the first client to disconnect must not cancel the work for
/// everybody else still waiting on it.
pub struct Coalescer<K, T: Clone> {
    in_flight: Arc<Mutex<HashMap<K, Shared<BoxFuture<'static, T>>>>>,
}

impl<K, T> Coalescer<K, T>
where
    K: Eq + Hash + Clone + Send + 'static,
    T: Clone + Send + Sync + 'static,
{
    pub fn new() -> Self {
        Self {
            in_flight: Arc::default(),
        }
    }

    /// Await the job for `key`, starting it with `job` if none is running.
    pub fn run<F>(&self, key: K, job: impl FnOnce() -> F) -> Shared<BoxFuture<'static, T>>
    where
        F: Future<Output = T> + Send + 'static,
    {
        let mut map = self.in_flight.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(shared) = map.get(&key) {
            return shared.clone();
        }

        // The lock is held until the entry is inserted, so the task's own
        // removal below can never run before it.
        let in_flight = self.in_flight.clone();
        let task_key = key.clone();
        let fut = job();
        let handle = tokio::spawn(async move {
            let out = fut.await;
            in_flight
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&task_key);
            out
        });
        let shared = async move { handle.await.expect("coalesced job panicked") }
            .boxed()
            .shared();
        map.insert(key, shared.clone());
        shared
    }
}

impl<K, T> Default for Coalescer<K, T>
where
    K: Eq + Hash + Clone + Send + 'static,
    T: Clone + Send + Sync + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    use super::*;

    #[tokio::test]
    async fn caps_concurrency_and_sheds_past_the_queue() {
        let gate = Arc::new(Gate::new(1, 1));
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();

        let g = gate.clone();
        let first = tokio::spawn(async move { g.run(async { rx.await.ok() }).await });
        tokio::time::sleep(Duration::from_millis(10)).await;

        let g = gate.clone();
        let second = tokio::spawn(async move { g.run(async { 2 }).await });
        tokio::time::sleep(Duration::from_millis(10)).await;

        // One running, one queued: the third is turned away.
        assert_eq!(gate.run(async { 3 }).await, Err(QueueFull));

        tx.send(()).unwrap();
        assert!(first.await.unwrap().is_ok());
        assert_eq!(second.await.unwrap(), Ok(2));
        assert_eq!(gate.run(async { 4 }).await, Ok(4));
    }

    #[tokio::test]
    async fn coalesces_concurrent_jobs_for_one_key() {
        let coalescer = Coalescer::<&str, usize>::new();
        let calls = Arc::new(AtomicUsize::new(0));

        let start = |c: Arc<AtomicUsize>| {
            move || async move {
                tokio::time::sleep(Duration::from_millis(20)).await;
                c.fetch_add(1, Ordering::SeqCst) + 1
            }
        };
        let a = coalescer.run("k", start(calls.clone()));
        let b = coalescer.run("k", start(calls.clone()));
        let other = coalescer.run("other", start(calls.clone()));
        let (a, b, _) = tokio::join!(a, b, other);
        assert_eq!(a, b);
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        // Settled jobs are forgotten, so the next request starts afresh.
        coalescer.run("k", start(calls.clone())).await;
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }
}
