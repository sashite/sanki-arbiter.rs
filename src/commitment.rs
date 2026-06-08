//! Player commitment violations (Move Encoding — Sanki; Statuses — Sanki
//! §Player commitment violations).
//!
//! Sanki binds players to two commitments, whose breach the arbiter rules
//! `illegalmove` (decisive `100/0` against the violator):
//!
//! - **Single-content** — for each `(session, signer, step)` slot, a player must
//!   commit to one Ply `content`. Identical resubmissions are idempotent retries;
//!   a second, *differing* content for the slot is a violation.
//! - **Step ownership** — under strict alternation, side `first` signs the odd
//!   steps and side `second` the even ones. A Ply signed at a step that is not
//!   the signer's is a violation (and is also excluded from the natural chain).
//!
//! Detection is over the natural-state window: only Plies with a canonical
//! attestation `created_at` ≤ the cutoff count. The **mutual-violation** rule
//! (Statuses — Sanki) collapses to a single comparison: the loser is the signer
//! of the *globally earliest* violating Ply — smallest attestation `created_at`,
//! ties broken by smallest violating-Ply event id. With only one offender, that
//! offender's earliest violation is trivially the global earliest; with two, the
//! earlier-attested violation loses.
//!
//! When a Ply breaches both commitments (a wrong-step Ply with differing
//! content), step ownership takes precedence — the wrong step makes the content
//! moot.

use crate::event::{Attestation, Ply, PublicKey};
use crate::race_resolution::canonical_attestation;
use crate::session::SessionParams;
use sashite_sanki_engine::domain::side::Side;
use sashite_sanki_engine::domain::time::Timestamp;
use std::collections::HashMap;

/// Which commitment a Ply breached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViolationKind {
    /// A second, differing `content` for a `(session, signer, step)` slot.
    SingleContent,
    /// A Ply signed at a step that is not the signer's under strict alternation.
    StepOwnership,
}

/// A ruled commitment violation: the losing side and the offending Ply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Violation<'a> {
    /// The side ruled against (`illegalmove`, decisive `100/0`).
    pub loser: Side,
    /// Which commitment was breached.
    pub kind: ViolationKind,
    /// The offending Ply.
    pub ply: &'a Ply,
    /// The offending Ply's canonical attestation `created_at`.
    pub at: Timestamp,
}

/// A Ply active within the cutoff window, paired with its authoritative timing.
#[derive(Clone, Copy)]
struct Active<'a> {
    ply: &'a Ply,
    at: Timestamp,
}

/// The ruling commitment violation in the session, if any, evaluated within the
/// natural-state window bounded by `cutoff` (the triggering Request's canonical
/// attestation `created_at`).
///
/// Returns the losing side per the mutual-violation rule, or `None` if no player
/// breached a commitment within the window.
#[must_use]
pub fn commitment_violation<'a>(
    params: &SessionParams,
    plies: &'a [Ply],
    attestations: &'a [Attestation],
    cutoff: Timestamp,
) -> Option<Violation<'a>> {
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

    // The committed (earliest) Ply of each (signer, step) slot.
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

    // Classify each active Ply; step ownership takes precedence over content.
    let mut violations: Vec<Violation<'a>> = Vec::new();
    for entry in &active {
        let kind = if entry.ply.signer != params.expected_signer(entry.ply.step) {
            Some(ViolationKind::StepOwnership)
        } else if let Some(first) = slot_first.get(&(entry.ply.signer, entry.ply.step)) {
            (entry.ply.id != first.ply.id && entry.ply.content != first.ply.content)
                .then_some(ViolationKind::SingleContent)
        } else {
            None
        };

        if let (Some(kind), Some(loser)) = (kind, params.side_of(entry.ply.signer)) {
            violations.push(Violation {
                loser,
                kind,
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

    use super::{commitment_violation, ViolationKind};
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
    fn violation_single_content() {
        // Same slot (first, step 1), two differing contents: violation.
        let plies = [ply(1, FIRST, 1, "A"), ply(2, FIRST, 1, "B")];
        let atts = [att(101, 1, 100), att(102, 2, 200)];
        let v = commitment_violation(&params(), &plies, &atts, ts(1000)).expect("violation");
        assert_eq!(v.loser, Side::First);
        assert_eq!(v.kind, ViolationKind::SingleContent);
        assert_eq!(*v.ply.id.as_bytes(), [2; 32]); // the second, divergent content
    }

    #[test]
    fn identical_resubmissions_do_not_violate() {
        // Same content twice: idempotent retry, no violation.
        let plies = [ply(1, FIRST, 1, "A"), ply(2, FIRST, 1, "A")];
        let atts = [att(101, 1, 100), att(102, 2, 200)];
        assert!(commitment_violation(&params(), &plies, &atts, ts(1000)).is_none());
    }

    #[test]
    fn violation_step_ownership() {
        // `second` signs step 1 (expected: `first`).
        let plies = [ply(1, SECOND, 1, "A")];
        let atts = [att(101, 1, 100)];
        let v = commitment_violation(&params(), &plies, &atts, ts(1000)).expect("violation");
        assert_eq!(v.loser, Side::Second);
        assert_eq!(v.kind, ViolationKind::StepOwnership);
    }

    #[test]
    fn proper_alternation_does_not_violate() {
        let plies = [ply(1, FIRST, 1, "A"), ply(2, SECOND, 2, "B")];
        let atts = [att(101, 1, 100), att(102, 2, 200)];
        assert!(commitment_violation(&params(), &plies, &atts, ts(1000)).is_none());
    }

    #[test]
    fn cutoff_excludes_the_divergent_ply() {
        // The divergent content is attested after the cutoff: excluded.
        let plies = [ply(1, FIRST, 1, "A"), ply(2, FIRST, 1, "B")];
        let atts = [att(101, 1, 100), att(102, 2, 2000)];
        assert!(commitment_violation(&params(), &plies, &atts, ts(1000)).is_none());
    }

    #[test]
    fn pending_ply_does_not_count() {
        // The divergent content is not attested: pending, excluded.
        let plies = [ply(1, FIRST, 1, "A"), ply(2, FIRST, 1, "B")];
        let atts = [att(101, 1, 100)];
        assert!(commitment_violation(&params(), &plies, &atts, ts(1000)).is_none());
    }

    #[test]
    fn mutual_violations_earliest_loses() {
        // First violates single-content (ply 2, attested at 300);
        // Second violates step-ownership (ply 3, attested at 200, earlier).
        let plies = [
            ply(1, FIRST, 1, "A"),
            ply(2, FIRST, 1, "B"),
            ply(3, SECOND, 1, "X"),
        ];
        let atts = [att(101, 1, 100), att(102, 2, 300), att(103, 3, 200)];
        let v = commitment_violation(&params(), &plies, &atts, ts(1000)).expect("violation");
        assert_eq!(v.loser, Side::Second); // earliest violation (200)
        assert_eq!(*v.ply.id.as_bytes(), [3; 32]);
    }

    #[test]
    fn non_player_ply_is_ignored() {
        // Signer pk(77): not a player of the session.
        let plies = [ply(1, 77, 1, "A"), ply(2, 77, 1, "B")];
        let atts = [att(101, 1, 100), att(102, 2, 200)];
        assert!(commitment_violation(&params(), &plies, &atts, ts(1000)).is_none());
    }
}
