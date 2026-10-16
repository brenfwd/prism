//! Lock helper shared by the storage, state machine and transport modules.

use std::sync::{Mutex, MutexGuard, PoisonError};

/// Locks `m`, taking the guard even if another thread panicked while holding
/// it. Every mutex in this crate guards data that is valid after a panic: a
/// SQLite connection, a snapshot slot and a channel cache.
pub(crate) fn lock_unpoisoned<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}
