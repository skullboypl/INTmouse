//! Files dragged across the edge (receiving side): the steering computer announces a drop with
//! `BulkMsg::DragDrop`, then sends its files as ordinary transfers. Once every one of them is
//! saved (or failed) the saved ones are dropped under the cursor.

use std::path::PathBuf;
use std::time::{Duration, Instant};

/// A drop that waited longer than this for its files goes no further than Downloads: by then the
/// user is doing something else, and files let go into a chat window would be sent from there.
pub const DROP_AT_CURSOR_WITHIN: Duration = Duration::from_secs(8);

/// A drop whose files never all arrive (link lost) is forgotten after this long.
const FORGET_AFTER: Duration = Duration::from_secs(600);

struct Pending {
    peer: String,
    waiting: Vec<u64>,
    saved: Vec<PathBuf>,
    failed: usize,
    started: Instant,
}

/// Every file of one drop has settled.
#[derive(Debug, PartialEq)]
pub struct SettledDrop {
    pub saved: Vec<PathBuf>,
    pub failed: usize,
    pub waited: Duration,
}

#[derive(Default)]
pub struct PendingDrops(Vec<Pending>);

impl PendingDrops {
    /// `peer` announced a drop made of these transfers.
    pub fn expect(&mut self, peer: &str, transfer_ids: Vec<u64>, now: Instant) {
        self.0.retain(|p| now.duration_since(p.started) < FORGET_AFTER);
        if transfer_ids.is_empty() {
            return;
        }
        self.0.push(Pending { peer: peer.to_string(), waiting: transfer_ids, saved: Vec::new(), failed: 0, started: now });
    }

    /// `transfer_id` from `peer` belongs to an announced batch still waiting for it.
    pub fn expects(&self, peer: &str, transfer_id: u64) -> bool {
        self.0.iter().any(|p| p.peer == peer && p.waiting.contains(&transfer_id))
    }

    /// One transfer from `peer` ended: saved to `saved`, or failed (`None`). Returns the drop it
    /// completed, if any.
    pub fn settle(&mut self, peer: &str, transfer_id: u64, saved: Option<PathBuf>, now: Instant) -> Option<SettledDrop> {
        let i = self.0.iter().position(|p| p.peer == peer && p.waiting.contains(&transfer_id))?;
        let p = &mut self.0[i];
        p.waiting.retain(|id| *id != transfer_id);
        match saved {
            Some(path) => p.saved.push(path),
            None => p.failed += 1,
        }
        if !p.waiting.is_empty() {
            return None;
        }
        let p = self.0.remove(i);
        Some(SettledDrop { saved: p.saved, failed: p.failed, waited: now.duration_since(p.started) })
    }
}

/// Drop the saved files under the cursor only while the peer that dragged them still steers this
/// computer and they came quickly enough; otherwise they just stay in Downloads.
pub fn drop_at_cursor(settled: &SettledDrop, still_steered: bool) -> bool {
    still_steered && !settled.saved.is_empty() && settled.waited <= DROP_AT_CURSOR_WITHIN
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_drop_settles_once_every_file_has_ended() {
        let t0 = Instant::now();
        let mut d = PendingDrops::default();
        d.expect("PC1", vec![1, 2, 3], t0);
        assert!(d.settle("PC1", 1, Some("a.png".into()), t0).is_none());
        assert!(d.settle("PC2", 2, Some("x".into()), t0).is_none(), "another computer's transfer");
        assert!(d.settle("PC1", 9, Some("x".into()), t0).is_none(), "a plain file send");
        assert!(d.settle("PC1", 2, None, t0).is_none());
        let done = d.settle("PC1", 3, Some("c.txt".into()), t0 + Duration::from_secs(1)).unwrap();
        assert_eq!(done.saved, vec![PathBuf::from("a.png"), PathBuf::from("c.txt")]);
        assert_eq!(done.failed, 1);
        assert_eq!(done.waited, Duration::from_secs(1));
        assert!(d.settle("PC1", 3, None, t0).is_none(), "settled only once");
    }

    #[test]
    fn a_drop_that_never_completes_is_forgotten() {
        let t0 = Instant::now();
        let mut d = PendingDrops::default();
        d.expect("PC1", vec![1, 2], t0);
        d.expect("PC1", vec![7], t0 + FORGET_AFTER);
        assert!(d.settle("PC1", 1, None, t0).is_none());
        assert!(d.settle("PC1", 2, None, t0).is_none(), "the old drop is gone");
        assert!(d.settle("PC1", 7, None, t0).is_some());
    }

    #[test]
    fn files_go_under_the_cursor_only_while_steered_and_soon_enough() {
        let quick = SettledDrop { saved: vec!["a".into()], failed: 0, waited: Duration::from_secs(1) };
        assert!(drop_at_cursor(&quick, true));
        assert!(!drop_at_cursor(&quick, false), "control went back meanwhile");
        let slow = SettledDrop { waited: DROP_AT_CURSOR_WITHIN + Duration::from_millis(1), ..quick };
        assert!(!drop_at_cursor(&slow, true));
        let nothing = SettledDrop { saved: vec![], failed: 2, waited: Duration::ZERO };
        assert!(!drop_at_cursor(&nothing, true));
    }
}
