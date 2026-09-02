//! Counting the chunks a request mix reads, to find out whether the panes
//! actually share any.
//!
//! Test-only, and `#[cfg(test)]` rather than a feature on purpose: the hook it
//! needs sits in [`crate::zarr_reader::ZarrStore::read_subset`], which is the
//! hottest function in the server, and a counter nobody reads in production is
//! still a lock taken per read.
//!
//! What is counted is *chunks*, not `get` calls on the store. `zarrs` decodes
//! every chunk that intersects the subset exactly once per
//! `retrieve_array_subset`, so the chunks a subset covers and the chunk reads
//! it costs are the same number — and asking the chunk grid is far less
//! machinery than wrapping two storage traits that no generic unifies.

use std::cell::RefCell;
use std::collections::HashMap;

use zarrs::array::Array;
use zarrs::array_subset::ArraySubset;

/// A chunk, identified across a whole session: which store, which level, which
/// cell of that level's chunk grid.
type ChunkId = (usize, usize, Vec<u64>);

// Both of these are thread-local, and that is what makes two measurements
// runnable at once: `cargo test` runs the tests in parallel, an actix test
// drives its whole request on the thread that started it, and `record` is
// called by this crate before the read rather than by whatever `zarrs` decodes
// chunks on. A shared tally would have each test resetting the others.
//
// Which route is asking is a thread-local rather than an argument because the
// hook is three call sites deep and every frame of that stack would otherwise
// grow a parameter it does not use.
thread_local! {
    static ROUTE: RefCell<&'static str> = const { RefCell::new("other") };
    static COUNTS: RefCell<Option<Counts>> = const { RefCell::new(None) };
}

#[derive(Default)]
struct Counts {
    /// The route that read each chunk *first*. What makes a repeat a
    /// cross-panel repeat rather than one panel re-reading its own chunk.
    first: HashMap<ChunkId, &'static str>,
    per_route: HashMap<&'static str, RouteCounts>,
    reads: u64,
}

/// One route's share of the reads.
#[derive(Clone, Copy, Default)]
pub struct RouteCounts {
    /// Chunk reads this route made.
    pub reads: u64,
    /// Of those, the ones that were the first read of that chunk.
    pub first: u64,
    /// Repeats of a chunk this same route had already read.
    pub repeat_own: u64,
    /// Repeats of a chunk another route had read first — the overlap the
    /// hypothesis is about.
    pub repeat_cross: u64,
}

/// What a measured run cost.
pub struct Report {
    pub reads: u64,
    pub unique: u64,
    pub per_route: Vec<(&'static str, RouteCounts)>,
}

impl Report {
    /// Repeat reads as a fraction of all reads.
    pub fn duplicate_rate(&self) -> f64 {
        if self.reads == 0 {
            return 0.0;
        }
        (self.reads - self.unique) as f64 / self.reads as f64
    }

    /// A table, for a test that prints its measurement rather than only
    /// asserting on it.
    pub fn table(&self) -> String {
        let mut out = format!(
            "chunk reads {}, unique {}, duplicate rate {:.1}%\n",
            self.reads,
            self.unique,
            self.duplicate_rate() * 100.0
        );
        out.push_str("  route      reads    first  repeat(own)  repeat(cross)\n");
        for (route, c) in &self.per_route {
            out.push_str(&format!(
                "  {:<8} {:>7}  {:>7}  {:>11}  {:>13}\n",
                route, c.reads, c.first, c.repeat_own, c.repeat_cross
            ));
        }
        out
    }
}

/// Begin counting, discarding whatever a previous run left.
pub fn start() {
    COUNTS.with(|c| *c.borrow_mut() = Some(Counts::default()));
}

/// Stop counting and take the tally.
pub fn finish() -> Report {
    let taken = COUNTS.with(|c| c.borrow_mut().take()).unwrap_or_default();
    let mut per_route: Vec<_> = taken.per_route.into_iter().collect();
    per_route.sort_by_key(|(route, _)| *route);
    Report {
        reads: taken.reads,
        unique: taken.first.len() as u64,
        per_route,
    }
}

/// Attribute every read made while the returned guard lives to `route`.
pub fn route(route: &'static str) -> RouteGuard {
    let previous = ROUTE.with(|r| std::mem::replace(&mut *r.borrow_mut(), route));
    RouteGuard { previous }
}

pub struct RouteGuard {
    previous: &'static str,
}

impl Drop for RouteGuard {
    fn drop(&mut self) {
        ROUTE.with(|r| *r.borrow_mut() = self.previous);
    }
}

/// Record the chunks one subset read covers.
///
/// Generic over the storage type because the two backends' arrays are two
/// different types; only the chunk grid is asked anything, and that is shared.
pub fn record<T: ?Sized>(store: usize, level: usize, array: &Array<T>, subset: &ArraySubset) {
    COUNTS.with(|cell| {
        let mut guard = cell.borrow_mut();
        let Some(counts) = guard.as_mut() else {
            return;
        };
        let Ok(Some(chunks)) = array.chunks_in_array_subset(subset) else {
            // A chunk grid that cannot name the chunks a subset covers (a
            // rectangular grid, say) is not something to guess at.
            return;
        };
        let route = ROUTE.with(|r| *r.borrow());
        for indices in &chunks.indices() {
            counts.reads += 1;
            let entry = counts.per_route.entry(route).or_default();
            entry.reads += 1;
            match counts.first.get(&(store, level, indices.clone())) {
                None => {
                    entry.first += 1;
                    counts.first.insert((store, level, indices), route);
                }
                Some(&owner) if owner == route => entry.repeat_own += 1,
                Some(_) => entry.repeat_cross += 1,
            }
        }
    });
}
