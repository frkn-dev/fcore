use std::collections::{HashMap, HashSet};

use fcore::Tag;
use uuid::Uuid;

/// A full sync that has been requested for one tag.
///
/// The api publishes the full list before it answers the HTTP request, so the
/// node may receive messages before it learns how many connections to expect.
/// The tracker therefore keeps collecting ids and only releases the prune set
/// once both halves are known and agree.
#[derive(Debug, Default)]
pub struct FullSyncState {
    seen: HashSet<Uuid>,
    expected: Option<usize>,
}

/// Full-sync bookkeeping for every tag served by this node.
#[derive(Debug, Default)]
pub struct FullSyncTracker {
    states: HashMap<Tag, FullSyncState>,
}

impl FullSyncTracker {
    /// Mark a tag as waiting for a full list. Called before the HTTP request.
    pub fn arm(&mut self, tag: Tag) {
        self.states.insert(tag, FullSyncState::default());
    }

    /// Forget a pending full sync after a failed request.
    pub fn cancel(&mut self, tag: Tag) {
        self.states.remove(&tag);
    }

    /// Tags that currently have a full sync in flight.
    pub fn pending_tags(&self) -> HashSet<Tag> {
        self.states.keys().copied().collect()
    }

    /// Record an applied connection id for a pending full sync.
    pub fn record(&mut self, tag: Tag, conn_id: Uuid) {
        if let Some(state) = self.states.get_mut(&tag) {
            state.seen.insert(conn_id);
        }
    }

    /// Supply the count returned by the api and release the keep-set if every
    /// expected connection has already arrived.
    pub fn finish(&mut self, tag: Tag, expected: usize) -> Option<HashSet<Uuid>> {
        let state = self.states.get_mut(&tag)?;
        state.expected = Some(expected);
        if state.seen.len() >= expected {
            return self.states.remove(&tag).map(|state| state.seen);
        }
        None
    }

    /// Release keep-sets for full syncs whose expected count has been reached.
    pub fn take_ready(&mut self) -> Vec<(Tag, HashSet<Uuid>)> {
        let ready: Vec<Tag> = self
            .states
            .iter()
            .filter(|(_, state)| {
                state
                    .expected
                    .is_some_and(|expected| state.seen.len() >= expected)
            })
            .map(|(tag, _)| *tag)
            .collect();

        ready
            .into_iter()
            .filter_map(|tag| self.states.remove(&tag).map(|state| (tag, state.seen)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_batch_does_not_prune_until_count_is_reached() {
        let mut tracker = FullSyncTracker::default();
        let tag = Tag::Wireguard;
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();

        tracker.arm(tag);
        tracker.record(tag, first);

        assert!(tracker.finish(tag, 2).is_none());
        assert!(tracker.take_ready().is_empty());

        tracker.record(tag, second);
        let ready = tracker.take_ready();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].0, tag);
        assert_eq!(ready[0].1.len(), 2);
        assert!(tracker.pending_tags().is_empty());
    }

    #[test]
    fn empty_full_sync_releases_an_empty_keep_set() {
        let mut tracker = FullSyncTracker::default();
        let tag = Tag::AmneziaWg;

        tracker.arm(tag);
        let keep = tracker.finish(tag, 0).expect("empty full sync is ready");

        assert!(keep.is_empty());
        assert!(tracker.pending_tags().is_empty());
    }

    #[test]
    fn failed_sync_can_be_cancelled_without_pruning() {
        let mut tracker = FullSyncTracker::default();
        let tag = Tag::Wireguard;

        tracker.arm(tag);
        tracker.record(tag, Uuid::new_v4());
        tracker.cancel(tag);

        assert!(tracker.pending_tags().is_empty());
        assert!(tracker.take_ready().is_empty());
    }

    #[test]
    fn a_new_full_sync_resets_stale_accumulated_ids() {
        let mut tracker = FullSyncTracker::default();
        let tag = Tag::Wireguard;

        tracker.arm(tag);
        tracker.record(tag, Uuid::new_v4());
        tracker.arm(tag);

        assert!(tracker.finish(tag, 1).is_none());
        assert!(tracker.take_ready().is_empty());
    }
}
