//! Forgiving-premove selection — the per-slot rule that picks a slot's canonical
//! Ply (Move Encoding — Sanki §Slot candidates and selection; ADR-0002).
//!
//! A `(session, signer, step)` slot may hold several candidate Plies. Ordered by
//! canonical attestation `created_at` (then event id), they are resolved against
//! the slot's **anchor** — the predecessor half-move's canonical attestation, or
//! t₀ for the first slot:
//!
//! - a candidate attested **before** the anchor is **anterior** (a premove,
//!   committed blind before the position it faces existed);
//! - one attested **at or after** the anchor is **informed** (played in knowledge
//!   of the position).
//!
//! The rule, with the normative **`K = 1`** anterior cap (one premove per slot,
//! no re-pre-play):
//!
//! 1. Consider the **earliest anterior** candidate only. If it is legal it is
//!    applied; any further anterior candidate is ignored (the cap).
//! 2. Otherwise — the earliest anterior is illegal (a blind speculation,
//!    *forgiven* not sanctioned) or there is none — take the **earliest
//!    informed** candidate: applied if legal, else `illegalmove` (decisive
//!    against its signer).
//! 3. Otherwise the slot is **unfilled**: the chain stops here.
//!
//! This module is the pure decision primitive — it consumes each candidate's
//! `legal` as a given (established by replaying the board, [`crate::natural_state`])
//! and pins only the *selection*. It is driven directly by the shared
//! `selection.json` conformance vectors, so the arbiter and the TypeScript client
//! agree bit-for-bit on which Ply is canonical.

use sashite_sanki_engine::domain::time::Timestamp;

/// The anterior cap: how many of a slot's earliest anterior candidates are
/// considered. `K = 1` — one premove per slot, no re-pre-play.
pub const ANTERIOR_CAP: usize = 1;

/// A slot candidate reduced to what selection needs: its identity (the race
/// tiebreak), its canonical attestation timing, and its legality in the slot's
/// replayed position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Candidate<Id> {
    /// The candidate's identity — the "smallest event id" race tiebreak.
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
    /// The selected candidate is an informed illegal move — `illegalmove`,
    /// decisive against its signer (the chain terminates here).
    IllegalMove(&'a Candidate<Id>),
    /// No candidate qualifies — the slot is unfilled (the chain stops).
    Unfilled,
}

impl<Id> Selection<'_, Id> {
    /// The selected candidate, if any (`None` only for [`Selection::Unfilled`]).
    #[inline]
    #[must_use]
    pub const fn selected(&self) -> Option<&Candidate<Id>> {
        match self {
            Self::Applied(candidate) | Self::IllegalMove(candidate) => Some(candidate),
            Self::Unfilled => None,
        }
    }
}

/// Selects the canonical candidate for a slot anchored at `anchor`.
///
/// `candidates` is the slot's unordered candidate set (each already legality-
/// judged). Ordering is by `(created_at, id)`. Implements the rule above,
/// including the `K = 1` anterior cap.
#[must_use]
pub fn select_candidate<'a, Id: Ord>(
    anchor: Timestamp,
    candidates: &'a [Candidate<Id>],
) -> Selection<'a, Id> {
    // `(created_at, id)` ordering, expressed without borrowing the key (so the
    // returned reference keeps the `'a` lifetime of the slice).
    let earlier = |a: &&'a Candidate<Id>, b: &&'a Candidate<Id>| {
        a.created_at
            .cmp(&b.created_at)
            .then_with(|| a.id.cmp(&b.id))
    };

    // 1. K = 1 — only the earliest-attested anterior candidate is considered.
    if let Some(anterior) = candidates
        .iter()
        .filter(|candidate| candidate.created_at < anchor)
        .min_by(earlier)
    {
        if anterior.legal {
            return Selection::Applied(anterior);
        }
        // An illegal blind premove is forgiven — skipped — and any further
        // anterior premove is ignored (no re-pre-play); fall through to informed.
    }

    // 2. The earliest informed candidate fills (or loses) the slot.
    match candidates
        .iter()
        .filter(|candidate| candidate.created_at >= anchor)
        .min_by(earlier)
    {
        Some(informed) if informed.legal => Selection::Applied(informed),
        Some(informed) => Selection::IllegalMove(informed),
        None => Selection::Unfilled,
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use super::{select_candidate, Candidate, Selection};
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
    fn legal_anterior_applied() {
        let cs = [cand("a1", 20, true)];
        assert_eq!(
            select_candidate(ts(50), &cs),
            Selection::Applied(&cs[0])
        );
    }

    #[test]
    fn illegal_anterior_no_informed_is_unfilled() {
        // A blind illegal premove is skipped; with no informed candidate the slot waits.
        let cs = [cand("a1", 20, false)];
        assert_eq!(select_candidate(ts(50), &cs), Selection::Unfilled);
    }

    #[test]
    fn k1_no_repreplay_falls_through_to_informed() {
        // earliest anterior (a1) illegal → skipped; the second anterior (a2, legal)
        // is IGNORED (K=1); selection falls through to the informed move (a3).
        let cs = [
            cand("a1", 10, false),
            cand("a2", 20, true),
            cand("a3", 70, true),
        ];
        let selection = select_candidate(ts(50), &cs);
        assert_eq!(selection, Selection::Applied(&cs[2]));
    }

    #[test]
    fn earliest_legal_anterior_wins() {
        let cs = [cand("a2", 20, true), cand("a1", 10, true)];
        assert_eq!(select_candidate(ts(50), &cs), Selection::Applied(&cs[1]));
    }

    #[test]
    fn fall_through_to_informed_legal() {
        let cs = [cand("a1", 10, false), cand("a2", 60, true)];
        assert_eq!(select_candidate(ts(50), &cs), Selection::Applied(&cs[1]));
    }

    #[test]
    fn informed_illegal_loses() {
        let cs = [cand("a1", 10, false), cand("a2", 55, false)];
        assert_eq!(select_candidate(ts(50), &cs), Selection::IllegalMove(&cs[1]));
    }

    #[test]
    fn earliest_informed_wins_even_if_illegal() {
        // Among informed candidates the earliest is selected regardless of legality;
        // a later legal informed does not save the slot.
        let cs = [cand("a1", 55, false), cand("a2", 60, true)];
        assert_eq!(select_candidate(ts(50), &cs), Selection::IllegalMove(&cs[0]));
    }

    #[test]
    fn created_at_tie_breaks_by_id() {
        let cs = [cand("b2", 20, true), cand("b1", 20, true)];
        assert_eq!(select_candidate(ts(50), &cs), Selection::Applied(&cs[1]));
    }

    #[test]
    fn k1_second_anterior_ignored_unfilled() {
        // earliest anterior illegal, the second anterior legal but ignored (K=1),
        // no informed → unfilled.
        let cs = [cand("c1", 1, false), cand("c2", 2, true)];
        assert_eq!(select_candidate(ts(100), &cs), Selection::Unfilled);
    }

    #[test]
    fn first_slot_anchor_t0_informed() {
        // anchor = t0 = 0: a candidate at/after 0 is informed.
        let legal = [cand("a1", 5, true)];
        assert_eq!(select_candidate(ts(0), &legal), Selection::Applied(&legal[0]));
        let illegal = [cand("a1", 5, false)];
        assert_eq!(
            select_candidate(ts(0), &illegal),
            Selection::IllegalMove(&illegal[0])
        );
    }
}
