//! A bounded tile cache.
//!
//! This is not an optimisation for the XY path, where the client already keeps
//! what it uploaded. It is what makes the orthogonal panes and the Z-projection
//! affordable: both re-read the same chunks over and over, and a chunk read
//! from S3 costs a request whatever the viewer does with it afterwards.
//!
//! Hand-rolled rather than a crate, because the whole of it is a map, a clock
//! and a byte budget, and a dependency would be more surface than code.

use omezarr_viewer_common::TileCoords;

use std::collections::HashMap;
use std::sync::Mutex;

/// What a cached entry is keyed by. Every field is part of the answer, so
/// every field is part of the key — `encoding` included, since the same region
/// has two byte forms.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct TileKey {
    pub layer: String,
    /// Which tile, in the shared crate's spelling.
    pub coords: TileCoords,
    pub encoding: &'static str,
    /// Projection, when the tile is one: `(kind, z0, z1)`.
    pub projection: Option<(&'static str, u64, u64)>,
}

struct Entry {
    bytes: std::sync::Arc<Vec<u8>>,
    /// Clock value at the last hit; the smallest is evicted first.
    used: u64,
}

/// An LRU cache over encoded tile bytes, bounded by total bytes held.
pub struct TileCache {
    inner: Mutex<Inner>,
    capacity: usize,
}

struct Inner {
    entries: HashMap<TileKey, Entry>,
    held: usize,
    clock: u64,
    hits: u64,
    misses: u64,
}

impl TileCache {
    /// `capacity_mb` of 0 disables the cache; every lookup misses and nothing
    /// is stored, so a caller need not branch on whether one exists.
    pub fn new(capacity_mb: usize) -> Self {
        Self {
            inner: Mutex::new(Inner {
                entries: HashMap::new(),
                held: 0,
                clock: 0,
                hits: 0,
                misses: 0,
            }),
            capacity: capacity_mb * 1024 * 1024,
        }
    }

    pub fn get(&self, key: &TileKey) -> Option<std::sync::Arc<Vec<u8>>> {
        if self.capacity == 0 {
            return None;
        }
        let mut inner = self.inner.lock().ok()?;
        inner.clock += 1;
        let clock = inner.clock;
        match inner.entries.get_mut(key) {
            Some(entry) => {
                entry.used = clock;
                let bytes = entry.bytes.clone();
                inner.hits += 1;
                Some(bytes)
            }
            None => {
                inner.misses += 1;
                None
            }
        }
    }

    pub fn put(&self, key: TileKey, bytes: std::sync::Arc<Vec<u8>>) {
        if self.capacity == 0 || bytes.len() > self.capacity {
            return;
        }
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        inner.clock += 1;
        let clock = inner.clock;
        let size = bytes.len();
        if let Some(old) = inner.entries.insert(key, Entry { bytes, used: clock }) {
            inner.held -= old.bytes.len();
        }
        inner.held += size;
        while inner.held > self.capacity {
            let Some(victim) = inner
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.used)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            if let Some(entry) = inner.entries.remove(&victim) {
                inner.held -= entry.bytes.len();
            }
        }
    }

    /// Drop everything. Called when a layer is removed, since its key prefix
    /// would otherwise outlive it.
    pub fn clear(&self) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.entries.clear();
            inner.held = 0;
        }
    }

    /// `(entries, bytes held, hits, misses)`, for `/api/stats`.
    pub fn stats(&self) -> (usize, usize, u64, u64) {
        match self.inner.lock() {
            Ok(inner) => (inner.entries.len(), inner.held, inner.hits, inner.misses),
            Err(_) => (0, 0, 0, 0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn key(x: u64) -> TileKey {
        TileKey {
            layer: "l".into(),
            coords: TileCoords {
                level: 0,
                t: 0,
                c: 0,
                z: 0,
                y: 0,
                x,
                h: 1,
                w: 1,
            },
            encoding: "f32",
            projection: None,
        }
    }

    #[test]
    fn round_trips_and_reports() {
        let cache = TileCache::new(1);
        assert!(cache.get(&key(0)).is_none());
        cache.put(key(0), Arc::new(vec![1, 2, 3]));
        assert_eq!(*cache.get(&key(0)).unwrap(), vec![1, 2, 3]);
        let (entries, held, hits, misses) = cache.stats();
        assert_eq!((entries, held, hits, misses), (1, 3, 1, 1));
    }

    #[test]
    fn evicts_least_recently_used_and_stays_under_capacity() {
        let cache = TileCache::new(1);
        let chunk = 400 * 1024;
        for x in 0..3 {
            cache.put(key(x), Arc::new(vec![0u8; chunk]));
            // Keep 0 warm so 1 is the eviction victim when 2 arrives.
            cache.get(&key(0));
        }
        let (_, held, _, _) = cache.stats();
        assert!(held <= 1024 * 1024, "held {held} over capacity");
        assert!(cache.get(&key(0)).is_some(), "the warm entry survived");
        assert!(cache.get(&key(1)).is_none(), "the cold entry was evicted");
    }

    #[test]
    fn zero_capacity_is_a_no_op() {
        let cache = TileCache::new(0);
        cache.put(key(0), Arc::new(vec![1]));
        assert!(cache.get(&key(0)).is_none());
    }
}
