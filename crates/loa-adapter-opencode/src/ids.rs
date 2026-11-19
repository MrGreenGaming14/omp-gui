//! OpenCode identifiers: `<prefix>_` + 12 hex digits of
//! `timestamp_ms * 0x1000 + counter` + 14 random base62 characters. Ascending
//! ids sort by creation time, which is how OpenCode orders messages and parts.

use std::hash::{BuildHasher, Hasher};
use std::sync::Mutex;

const BASE62: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

static CLOCK: Mutex<(i64, u64)> = Mutex::new((0, 0));

/// A new ascending id such as `msg_…` or `prt_…`, stamped with `now_ms`.
///
/// The random tail only has to make ids unique, not secret, so it comes from
/// the randomly keyed std hasher.
pub fn ascending(prefix: &str, now_ms: i64) -> String {
    let counter = {
        // The pair is only read and written under the lock, so a poisoned
        // lock still holds a consistent value.
        let mut clock = CLOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if clock.0 != now_ms {
            *clock = (now_ms, 0);
        }
        clock.1 += 1;
        clock.1
    };
    let value = u64::try_from(now_ms).unwrap_or(0).wrapping_mul(0x1000).wrapping_add(counter) & 0xffff_ffff_ffff;
    let mut id = String::with_capacity(prefix.len() + 27);
    id.push_str(prefix);
    id.push('_');
    id.push_str(&format!("{value:012x}"));
    let state = std::collections::hash_map::RandomState::new();
    for i in 0..14u64 {
        let mut h = state.build_hasher();
        h.write_u64(value ^ (counter << 16) ^ i);
        let digit = usize::try_from(h.finish() % 62).unwrap_or(0);
        id.push(char::from(BASE62[digit]));
    }
    id
}

#[cfg(test)]
mod tests {
    #[test]
    fn ids_in_one_millisecond_still_ascend() {
        let a = super::ascending("msg", 1_700_000_000_000);
        let b = super::ascending("msg", 1_700_000_000_000);
        assert!(a.starts_with("msg_") && a.len() == 4 + 26);
        assert!(a[..16] < b[..16]);
    }
}
