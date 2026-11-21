//! Poison-tolerant locking: a panicked holder never takes the lock down with it.

use std::sync::{Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

/// `Mutex::lock` that recovers the guard from a poisoned lock.
pub trait LockExt<T: ?Sized> {
    fn locked(&self) -> MutexGuard<'_, T>;
}

/// `RwLock::read`/`write` that recover the guard from a poisoned lock.
pub trait RwLockExt<T: ?Sized> {
    fn read_ok(&self) -> RwLockReadGuard<'_, T>;
    fn write_ok(&self) -> RwLockWriteGuard<'_, T>;
}

impl<T: ?Sized> LockExt<T> for Mutex<T> {
    fn locked(&self) -> MutexGuard<'_, T> { self.lock().unwrap_or_else(PoisonError::into_inner) }
}

impl<T: ?Sized> RwLockExt<T> for RwLock<T> {
    fn read_ok(&self) -> RwLockReadGuard<'_, T> { self.read().unwrap_or_else(PoisonError::into_inner) }
    fn write_ok(&self) -> RwLockWriteGuard<'_, T> { self.write().unwrap_or_else(PoisonError::into_inner) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poisoned_locks_still_hand_out_guards() {
        let (m, rw) = (Mutex::new(1), RwLock::new(1));
        std::thread::scope(|s| s.spawn(|| { let _held = (m.lock().unwrap(), rw.write().unwrap()); panic!("poison") }).join().unwrap_err());
        assert!(m.is_poisoned() && rw.is_poisoned());
        *m.locked() += 1;
        *rw.write_ok() += 1;
        assert_eq!((*m.locked(), *rw.read_ok()), (2, 2));
    }
}
