//! Two-window forgiving-premove selection — the per-slot rule that picks a slot's
//! canonical Ply (Move Encoding — Sanki §Slot candidates and selection).
//!
//! A `(session, signer, step)` slot may hold several candidate Plies. Each is
//! classified against the slot's **boundary** — the predecessor half-move's
//! canonical attestation, or t₀ for the first slot — by its own canonical
//! attestation `created_at`:
//!
//! - a candidate attested **strictly before** the boundary is **anterior** (a
//!   premove, committed before the position it faces existed);
//! - one attested **at or after** the boundary is **informed** (a live move,
//!   played in knowledge of the position).
//!
//! The canonical Ply is chosen by trying the two windows in order, anterior first:
//!
//! 1. **Anterior (premoves) — latest legal wins.** Among the `K` most recent
//!    anterior candidates by `(created_at, id)`, scanned newest-first, the first
//!    legal one — the *latest* legal premove — is applied. A re-premove supersedes
//!    an older one; an illegal premove is skipped in favour of the next-newest.
//! 2. **Informed (live moves) — earliest legal wins.** If no anterior candidate is
//!    legal, among the `K` earliest informed candidates by `(created_at, id)`,
//!    scanned oldest-first, the first legal one — the *earliest* legal live move —
//!    is applied. A move played in full knowledge is committed, not overwritten.
//! 3. Otherwise the slot is **unfilled**: the chain stops here.
//!
//! An **illegal candidate is always skipped** — premove or live, never a loss;
//! there is **no `illegalmove` outcome**. Legality is a precondition in both
//! windows; the window governs only which legal candidate binds when several exist.
//!
//! This module is the pure decision primitive — it consumes each candidate's
//! `legal` as a given (established by replaying the board, [`crate::natural_state`])
//! and pins only the *selection*. It is driven directly by the shared
//! `selection.json` conformance vectors, so the arbiter and the TypeScript client
//! agree bit-for-bit on which Ply is canonical.

use sashite_sanki_engine::domain::time::Timestamp;

/// The per-window candidate cap `K`: at most the `K` most-recent anterior
/// candidates, or the `K` earliest informed ones, are considered (≤ `2K` legality
/// tests per slot). `K > 1` leaves room for an honest re-premove or retry; a player
/// flooding their own window past `K` only self-harms. Deployment-tunable.
pub const CANDIDATE_CAP: usize = 8;

/// A slot candidate reduced to what selection needs: its identity (the race
/// tiebreak), its canonical attestation timing, and its legality in the slot's
/// replayed position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Candidate<Id> {
    /// The candidate's identity — the event-id race tiebreak.
    pub id: Id,
    /// The candidate's canonical attestation `created_at`.
    pub created_at: Timestamp,
    /// Whether the candidate is legal in the slot's position (full rule system).
    pub legal: bool,
}

/// The outcome of selecting among a slot's candidates.
#[derive(Debug, PartialEq, Eq)]
pub enum Selection<'a, Id> {
    /// A candidate fills the slot (the chain continues with it).
    Applied(&'a Candidate<Id>),
    /// No candidate qualifies — the slot is unfilled (the chain stops).
    Unfilled,
}

impl<Id> Selection<'_, Id> {
    /// The selected candidate, if any (`None` only for [`Selection::Unfilled`]).
    #[inline]
    #[must_use]
    pub const fn selected(&self) -> Option<&Candidate<Id>> {
        match self {
            Self::Applied(candidate) => Some(candidate),
            Self::Unfilled => None,
        }
    }
}

/// Selects the canonical candidate for a slot with the given `boundary`.
///
/// `candidates` is the slot's unordered candidate set (each already legality-
/// judged). A candidate is *anterior* iff `created_at < boundary`, else *informed*.
/// Implements the two-window rule above, with the per-window cap `cap` (`K`).
#[must_use]
pub fn select_candidate<'a, Id: Ord>(
    boundary: Timestamp,
    candidates: &'a [Candidate<Id>],
    cap: usize,
) -> Selection<'a, Id> {
    // Anterior window (premoves, created_at < boundary): the K most recent by
    // (created_at, id), newest first — the first legal is the LATEST legal premove
    // (a re-premove supersedes an older one).
    let mut anterior: Vec<&'a Candidate<Id>> = candidates
        .iter()
        .filter(|candidate| candidate.created_at < boundary)
        .collect();
    anterior.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| b.id.cmp(&a.id))
    });
    if let Some(chosen) = anterior
        .into_iter()
        .take(cap)
        .find(|candidate| candidate.legal)
    {
        return Selection::Applied(chosen);
    }

    // Informed window (live moves, created_at >= boundary): the K earliest by
    // (created_at, id), oldest first — the first legal is the EARLIEST legal live
    // move (committed on its first legal instance, not overwritten by a later one).
    let mut informed: Vec<&'a Candidate<Id>> = candidates
        .iter()
        .filter(|candidate| candidate.created_at >= boundary)
        .collect();
    informed.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then_with(|| a.id.cmp(&b.id))
    });
    if let Some(chosen) = informed
        .into_iter()
        .take(cap)
        .find(|candidate| candidate.legal)
    {
        return Selection::Applied(chosen);
    }

    Selection::Unfilled
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use super::{select_candidate, Candidate, Selection, CANDIDATE_CAP};
    use sashite_sanki_engine::domain::time::Timestamp;

    fn ts(secs: i64) -> Timestamp {
        Timestamp::from_unix(secs)
    }

    /// `(id, created_at, legal)` → a candidate with a `&str` id.
    fn cand(id: &'static str, created_at: i64, legal: bool) -> Candidate<&'static str> {
        Candidate {
            id,
            created_at: ts(created_at),
            legal,
        }
    }

    #[test]
    fn single_informed_legal_applied() {
        let cs = [cand("a1", 120, true)];
        assert_eq!(
            select_candidate(ts(100), &cs, CANDIDATE_CAP),
            Selection::Applied(&cs[0])
        );
    }

    #[test]
    fn single_illegal_unfilled() {
        let cs = [cand("a1", 120, false)];
        assert_eq!(
            select_candidate(ts(100), &cs, CANDIDATE_CAP),
            Selection::Unfilled
        );
    }

    #[test]
    fn anterior_latest_legal_wins() {
        // Two legal premoves (both before the boundary): the later — the re-premove — wins.
        let cs = [cand("a1", 20, true), cand("a2", 60, true)];
        assert_eq!(
            select_candidate(ts(100), &cs, CANDIDATE_CAP),
            Selection::Applied(&cs[1])
        );
    }

    #[test]
    fn anterior_skips_newest_illegal_to_next_legal() {
        // The newest premove is illegal; the next-newest legal premove wins.
        let cs = [cand("a1", 20, true), cand("a2", 60, false)];
        assert_eq!(
            select_candidate(ts(100), &cs, CANDIDATE_CAP),
            Selection::Applied(&cs[0])
        );
    }

    #[test]
    fn informed_earliest_legal_wins() {
        // Two legal live moves (both at/after the boundary): the earliest wins.
        let cs = [cand("a1", 10, true), cand("a2", 20, true)];
        assert_eq!(
            select_candidate(ts(0), &cs, CANDIDATE_CAP),
            Selection::Applied(&cs[0])
        );
    }

    #[test]
    fn informed_skips_earliest_illegal_to_next_legal() {
        let cs = [cand("a1", 10, false), cand("a2", 20, true)];
        assert_eq!(
            select_candidate(ts(0), &cs, CANDIDATE_CAP),
            Selection::Applied(&cs[1])
        );
    }

    #[test]
    fn legal_anterior_preferred_over_informed() {
        // A legal premove binds even though a legal live move also exists.
        let cs = [cand("p1", 50, true), cand("L1", 150, true)];
        assert_eq!(
            select_candidate(ts(100), &cs, CANDIDATE_CAP),
            Selection::Applied(&cs[0])
        );
    }

    #[test]
    fn fallthrough_to_informed_when_no_legal_anterior() {
        // The only premove is illegal → fall through to the earliest legal live move.
        let cs = [cand("p1", 50, false), cand("L1", 150, true)];
        assert_eq!(
            select_candidate(ts(100), &cs, CANDIDATE_CAP),
            Selection::Applied(&cs[1])
        );
    }

    #[test]
    fn all_illegal_both_windows_unfilled() {
        let cs = [cand("p1", 50, false), cand("L1", 150, false)];
        assert_eq!(
            select_candidate(ts(100), &cs, CANDIDATE_CAP),
            Selection::Unfilled
        );
    }

    #[test]
    fn anterior_tie_breaks_by_largest_id_first() {
        // Equal timing in the anterior window: the more recent is the larger id.
        let cs = [cand("b1", 60, true), cand("b2", 60, true)];
        assert_eq!(
            select_candidate(ts(100), &cs, CANDIDATE_CAP),
            Selection::Applied(&cs[1])
        );
    }

    #[test]
    fn informed_tie_breaks_by_smallest_id_first() {
        // Equal timing in the informed window: the earliest is the smaller id.
        let cs = [cand("b1", 20, true), cand("b2", 20, true)];
        assert_eq!(
            select_candidate(ts(0), &cs, CANDIDATE_CAP),
            Selection::Applied(&cs[0])
        );
    }

    #[test]
    fn cap_anterior_most_recent_buries_older_legal() {
        // cap K=2 considers the 2 MOST RECENT premoves (both illegal); an older legal
        // premove is beyond the cap → unfilled (flooding one's own recent premoves is
        // self-harm).
        let cs = [
            cand("a1", 10, true),
            cand("a2", 800, false),
            cand("a3", 900, false),
        ];
        assert_eq!(select_candidate(ts(1000), &cs, 2), Selection::Unfilled);
        // With K=3 the older legal premove is reached.
        assert_eq!(
            select_candidate(ts(1000), &cs, 3),
            Selection::Applied(&cs[0])
        );
    }

    #[test]
    fn cap_informed_earliest_buries_later_legal() {
        // cap K=2 considers the 2 EARLIEST live moves (both illegal); a later legal
        // live move is beyond the cap → unfilled.
        let cs = [
            cand("a1", 10, false),
            cand("a2", 20, false),
            cand("a3", 30, true),
        ];
        assert_eq!(select_candidate(ts(0), &cs, 2), Selection::Unfilled);
        assert_eq!(select_candidate(ts(0), &cs, 3), Selection::Applied(&cs[2]));
    }

    #[test]
    fn first_slot_boundary_t0_is_informed() {
        // boundary = t₀ = 0: a candidate at/after 0 is informed; an illegal one is
        // skipped (no `illegalmove`), leaving the slot unfilled.
        let legal = [cand("a1", 5, true)];
        assert_eq!(
            select_candidate(ts(0), &legal, CANDIDATE_CAP),
            Selection::Applied(&legal[0])
        );
        let illegal = [cand("a1", 5, false)];
        assert_eq!(
            select_candidate(ts(0), &illegal, CANDIDATE_CAP),
            Selection::Unfilled
        );
    }
}
