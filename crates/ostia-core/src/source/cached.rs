//! In-process TTL cache around a [`ProfileSource`].
//!
//! Slice 2 of `changes/006-profile-source-providers/`. Wraps the underlying
//! source so that successful loads are reused for `ttl`, and refreshes happen
//! lazily on the next call after expiry. Refresh failures are fail-open (the
//! caller logs and keeps the previous data — see [`RefreshOutcome`] below).
//!
//! The cache lives in the McpServer's state and is consulted on every per-call
//! surface (`tools/list`, `tools/call`, `initialize`). See `serve.rs`.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

use super::profile::{ProfileSource, SourcedConfig};

/// Result of a refresh attempt against the underlying source.
pub enum RefreshOutcome {
    /// The cached entry is still warm; no source call was made.
    NotDue,
    /// The cache was due and the refresh succeeded. New data is now cached and
    /// returned for the current request.
    Refreshed {
        sourced: SourcedConfig,
        binary_diff: BinaryDiff,
    },
    /// The cache was due but the refresh failed. Fail-open: caller keeps the
    /// previous data. The captured error is for logging.
    Failed { error: anyhow::Error },
}

/// Difference in the union-of-binaries across all profiles between successive
/// successful refreshes. Emitted as an observable for Slice 3 to consume.
#[derive(Debug, Default)]
pub struct BinaryDiff {
    pub added: Vec<String>,
    pub removed: Vec<String>,
}

impl BinaryDiff {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty()
    }
}

struct CachedState {
    fetched_at: Instant,
    last_binary_set: HashSet<String>,
}

/// TTL-bounded wrapper around any [`ProfileSource`].
///
/// Cloning the cache (via `Arc`) shares the state across handlers.
pub struct CachedProfileSource {
    inner: Box<dyn ProfileSource>,
    ttl: Duration,
    state: Mutex<Option<CachedState>>,
}

impl CachedProfileSource {
    pub fn new(inner: Box<dyn ProfileSource>, ttl: Duration) -> Arc<Self> {
        Arc::new(Self {
            inner,
            ttl,
            state: Mutex::new(None),
        })
    }

    /// Tell the cache that an initial successful load just happened with the
    /// given data — typically called from `OstiaConfig::load_resolved` so the
    /// startup load primes the cache (so the next per-call refresh check sees
    /// it as fresh).
    pub async fn prime(&self, sourced: &SourcedConfig) {
        let mut guard = self.state.lock().await;
        *guard = Some(CachedState {
            fetched_at: Instant::now(),
            last_binary_set: binary_set(sourced),
        });
    }

    /// If the cache is past its TTL, attempt a refresh. Returns one of three
    /// outcomes — callers act on each accordingly.
    pub async fn refresh_if_due(&self) -> RefreshOutcome {
        // Snapshot the deadline under the lock, but drop the guard before
        // calling `inner.load()` to avoid holding the mutex across an `.await`
        // that could fail or take a while.
        let due = {
            let guard = self.state.lock().await;
            match guard.as_ref() {
                None => true,
                Some(s) => s.fetched_at.elapsed() >= self.ttl,
            }
        };

        if !due {
            return RefreshOutcome::NotDue;
        }

        match self.inner.load().await {
            Ok(sourced) => {
                let new_set = binary_set(&sourced);
                let prev_set = {
                    let mut guard = self.state.lock().await;
                    let prev = guard
                        .as_ref()
                        .map(|s| s.last_binary_set.clone())
                        .unwrap_or_default();
                    *guard = Some(CachedState {
                        fetched_at: Instant::now(),
                        last_binary_set: new_set.clone(),
                    });
                    prev
                };
                let diff = compute_diff(&prev_set, &new_set);
                RefreshOutcome::Refreshed {
                    sourced,
                    binary_diff: diff,
                }
            }
            Err(error) => RefreshOutcome::Failed { error },
        }
    }
}

/// Union of binary names referenced by any profile's bundles in `sourced`.
fn binary_set(sourced: &SourcedConfig) -> HashSet<String> {
    let mut set = HashSet::new();
    for profile in sourced.profiles.values() {
        for bundle_name in &profile.bundles {
            if let Some(bundle) = sourced.bundles.get(bundle_name) {
                for bin in &bundle.binaries {
                    set.insert(bin.clone());
                }
            }
        }
    }
    set
}

fn compute_diff(prev: &HashSet<String>, current: &HashSet<String>) -> BinaryDiff {
    let mut added: Vec<String> = current.difference(prev).cloned().collect();
    let mut removed: Vec<String> = prev.difference(current).cloned().collect();
    added.sort();
    removed.sort();
    BinaryDiff { added, removed }
}
