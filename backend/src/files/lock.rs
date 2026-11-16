//! A single process-wide serialization point for mutating the file tree and the
//! content-addressed blob store.
//!
//! Nimbo is intentionally a single-instance application: SQLite is embedded and
//! blobs live on one local filesystem. Serializing every mutation through one
//! async mutex makes the "check parent/ancestry, then insert/move/delete"
//! sequences and the "promote blob, then create its reference" sequences
//! atomic with respect to each other. In particular the background garbage
//! collector can never observe a promoted blob before the node (or thumbnail)
//! that references it has committed, and two concurrent moves cannot both pass
//! an ancestry check and create a cycle.
//!
//! The lock is deliberately *not* re-entrant. A holder must never call a
//! function that acquires it again; helpers that expect the caller to hold the
//! guard take a `&MutationGuard` argument.

use std::sync::OnceLock;
use tokio::sync::{Mutex, MutexGuard};

static MUTATION_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

/// Guard proving the caller holds the process-wide mutation lock.
pub struct MutationGuard {
    _guard: MutexGuard<'static, ()>,
}

/// Waits for and acquires the process-wide mutation lock.
pub async fn mutation() -> MutationGuard {
    let mutex = MUTATION_LOCK.get_or_init(|| Mutex::new(()));
    MutationGuard {
        _guard: mutex.lock().await,
    }
}
