//! Equivocation — the single-content rule (Move Encoding — Sanki §Ply finality
//! and the single-content rule; Statuses — Sanki §The `illegalmove`
//! termination §Equivocation).
//!
//! Each player MUST play at most once per step: one `content` per
//! `(session, signer, step)` slot, whatever the step's value — already played,
//! current, or pending. Identical resubmissions are idempotent retries; a
//! second, *differing* content for a slot is an **equivocation**, ruled
//! `illegalmove` (decisive `100/0` against the equivocator).
//!
//! The rule applies to **every** slot, including pending ones: a Ply is never
//! judged for *legality* while pending, but an equivocation is a slot-level
//! fact, independent of any position, sanctionable as soon as both Plies are
//! attested within the cutoff window. The sanction is a security property: an
//! equivocation can never benefit its author positionally (the canonical Ply is
//! the first attested), so its only uses are hostile — divergent contents
//! published to trap the opponent, or a withheld earlier-attested content
//! revealed to rewrite the chain. Anchoring the violation at the
//! **second-attested** differing Ply makes both attacks self-defeating: the
//! violation always precedes, in attestation time, any reply it could have
//! misled.
//!
//! Detection is over the natural-state window: only Plies with a canonical
//! attestation `created_at` ≤ the cutoff count. The **mutual-equivocation**
//! rule (Statuses — Sanki §Mutual equivocation) collapses to a single
//! comparison: the loser is the signer of the *globally earliest* violating
//! Ply — smallest attestation `created_at`, ties broken by smallest
//! violating-Ply event id. With one offender, that offender's earliest
//! violation is trivially the global earliest; with two, the earlier-attested
//! violation loses.
//!
//! A former second commitment — *step ownership* — disappeared with the
//! per-player step semantics: the signer is part of the slot, so the
//! opponent's slots cannot be occupied (Move Encoding — Sanki §Within-step
//! ordering).

use crate::event::{Attestation, Ply, PublicKey};
use crate::race_resolution::canonical_attestation;
use crate::session::SessionParams;
use sashite_sanki_engine::domain::side::Side;
use sashite_sanki_engine::domain::time::Timestamp;
use std::collections::HashMap;

/// A ruled equivocation: the losing side and the offending Ply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Equivocation<'a> {
    /// The side ruled against (`illegalmove`, decisive `100/0`).
    pub loser: Side,
    /// The offending Ply (the differing content for an occupied slot).
    pub ply: &'a Ply,
    /// The offending Ply's canonical attestation `created_at` — the
    /// violation's anchor for verdict resolution.
    pub at: Timestamp,
}

/// A Ply active within the cutoff window, paired with its authoritative timing.
#[derive(Clone, Copy)]
struct Active<'a> {
    ply: &'a Ply,
    at: Timestamp,
}

/// The ruling equivocation in the session, if any, evaluated within the
/// natural-state window bounded by `cutoff` (the triggering Request's canonical
/// attestation `created_at`).
///
/// Returns the losing side per the mutual-equivocation rule, or `None` if no
/// player equivocated within the window.
#[must_use]
pub fn equivocation<'a>(
    params: &SessionParams,
    plies: &'a [Ply],
    attestations: &'a [Attestation],
    cutoff: Timestamp,
) -> Option<Equivocation<'a>> {
    let timestamper = params.timestamper();
    let session = params.session();

    // Active Plies: by a player, in this session, canonically attested ≤ cutoff.
    let active: Vec<Active<'a>> = plies
        .iter()
        .filter(|ply| ply.session == session && params.is_player(ply.signer))
        .filter_map(|ply| {
            let at = canonical_attestation(attestations, ply.id, timestamper)?.created_at;
            (at <= cutoff).then_some(Active { ply, at })
        })
        .collect();

    // The committed (earliest-attested) Ply of each (signer, step) slot.
    let mut slot_first: HashMap<(PublicKey, u32), Active<'a>> = HashMap::new();
    for entry in &active {
        slot_first
            .entry((entry.ply.signer, entry.ply.step))
            .and_modify(|first| {
                if (entry.at, entry.ply.id) < (first.at, first.ply.id) {
                    *first = *entry;
                }
            })
            .or_insert(*entry);
    }

    // Every active Ply whose content differs from its slot's committed content
    // is a violation; identical resubmissions are idempotent retries.
    let mut violations: Vec<Equivocation<'a>> = Vec::new();
    for entry in &active {
        let Some(first) = slot_first.get(&(entry.ply.signer, entry.ply.step)) else {
            continue;
        };
        if entry.ply.id == first.ply.id || entry.ply.content == first.ply.content {
            continue;
        }
        if let Some(loser) = params.side_of(entry.ply.signer) {
            violations.push(Equivocation {
                loser,
                ply: entry.ply,
                at: entry.at,
            });
        }
    }

    // The globally earliest violation determines the loser.
    violations
        .into_iter()
        .min_by_key(|violation| (violation.at, violation.ply.id))
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use super::equivocation;
    use crate::event::{Attestation, EventId, Ply, PublicKey};
    use crate::session::SessionParams;
    use sashite_sanki_engine::domain::side::Side;
    use sashite_sanki_engine::domain::time::{Duration, Timestamp};
    use sashite_sanki_engine::domain::time_control::{Period, TimeControl};
    use sashite_sanki_engine::position::Position;

    const FIRST: u8 = 10;
    const SECOND: u8 = 20;
    const TIMESTAMPER: u8 = 99;
    const SESSION: u8 = 50;

    fn pk(byte: u8) -> PublicKey {
        PublicKey::from_bytes([byte; 32])
    }

    fn eid(byte: u8) -> EventId {
        EventId::from_bytes([byte; 32])
    }

    fn ts(secs: i64) -> Timestamp {
        Timestamp::from_unix(secs)
    }

    fn ply(id: u8, signer: u8, step: u32, content: &str) -> Ply {
        Ply::new(
            eid(id),
            pk(signer),
            eid(SESSION),
            step,
            false,
            content.to_owned(),
        )
    }

    fn att(id: u8, attests: u8, at: i64) -> Attestation {
        Attestation::new(eid(id), pk(TIMESTAMPER), eid(attests), ts(at))
    }

    fn params() -> SessionParams {
        let period = Period::new(Duration::from_secs(600), None, None).expect("valid period");
        SessionParams::new(
            eid(SESSION),
            pk(2),
            pk(TIMESTAMPER),
            pk(FIRST),
            pk(SECOND),
            TimeControl::new(period, Vec::new()),
            Position::parse("4k^3/8/8/8/8/8/8/4K^3 / W/w").expect("valid FEEN"),
            ts(0),
        )
    }

    #[test]
    fn differing_content_is_an_equivocation() {
        // Same slot (first, step 1), two differing contents: violation, anchored
        // at the second-attested content.
        let plies = [ply(1, FIRST, 1, "A"), ply(2, FIRST, 1, "B")];
        let atts = [att(101, 1, 100), att(102, 2, 200)];
        let v = equivocation(&params(), &plies, &atts, ts(1000)).expect("violation");
        assert_eq!(v.loser, Side::First);
        assert_eq!(*v.ply.id.as_bytes(), [2; 32]); // the second, divergent content
        assert_eq!(v.at, ts(200));
    }

    #[test]
    fn identical_resubmissions_do_not_violate() {
        // Same content twice: idempotent retry, no violation.
        let plies = [ply(1, FIRST, 1, "A"), ply(2, FIRST, 1, "A")];
        let atts = [att(101, 1, 100), att(102, 2, 200)];
        assert!(equivocation(&params(), &plies, &atts, ts(1000)).is_none());
    }

    #[test]
    fn distinct_slots_do_not_violate() {
        // Different steps and different signers: every slot holds one content.
        let plies = [
            ply(1, FIRST, 1, "A"),
            ply(2, SECOND, 1, "B"),
            ply(3, FIRST, 2, "C"),
        ];
        let atts = [att(101, 1, 100), att(102, 2, 200), att(103, 3, 300)];
        assert!(equivocation(&params(), &plies, &atts, ts(1000)).is_none());
    }

    #[test]
    fn pending_slot_equivocation_counts() {
        // The slot (first, step 5) is far beyond the play order, yet two
        // differing contents for it are an equivocation all the same.
        let plies = [ply(1, FIRST, 5, "A"), ply(2, FIRST, 5, "B")];
        let atts = [att(101, 1, 100), att(102, 2, 200)];
        let v = equivocation(&params(), &plies, &atts, ts(1000)).expect("violation");
        assert_eq!(v.loser, Side::First);
    }

    #[test]
    fn cutoff_excludes_the_divergent_ply() {
        // The divergent content is attested after the cutoff: excluded.
        let plies = [ply(1, FIRST, 1, "A"), ply(2, FIRST, 1, "B")];
        let atts = [att(101, 1, 100), att(102, 2, 2000)];
        assert!(equivocation(&params(), &plies, &atts, ts(1000)).is_none());
    }

    #[test]
    fn pending_ply_does_not_count() {
        // The divergent content is not attested: pending, excluded.
        let plies = [ply(1, FIRST, 1, "A"), ply(2, FIRST, 1, "B")];
        let atts = [att(101, 1, 100)];
        assert!(equivocation(&params(), &plies, &atts, ts(1000)).is_none());
    }

    #[test]
    fn mutual_equivocations_earliest_loses() {
        // First equivocates at 300; Second equivocates at 200 (earlier): the
        // earlier-attested violation loses.
        let plies = [
            ply(1, FIRST, 1, "A"),
            ply(2, FIRST, 1, "B"),
            ply(3, SECOND, 1, "X"),
            ply(4, SECOND, 1, "Y"),
        ];
        let atts = [
            att(101, 1, 100),
            att(102, 2, 300),
            att(103, 3, 150),
            att(104, 4, 200),
        ];
        let v = equivocation(&params(), &plies, &atts, ts(1000)).expect("violation");
        assert_eq!(v.loser, Side::Second); // earliest violation (200)
        assert_eq!(*v.ply.id.as_bytes(), [4; 32]);
    }

    #[test]
    fn non_player_ply_is_ignored() {
        // Signer pk(77): not a player of the session.
        let plies = [ply(1, 77, 1, "A"), ply(2, 77, 1, "B")];
        let atts = [att(101, 1, 100), att(102, 2, 200)];
        assert!(equivocation(&params(), &plies, &atts, ts(1000)).is_none());
    }
}
