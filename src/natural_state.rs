//! The natural state of events at adjudication (kind `6425` §Natural state).
//!
//! When the arbiter rules, it rules on the **longest consecutive chain of
//! Plies** starting from step 1, where each step's Ply:
//!
//! 1. is canonical for its slot (race resolution);
//! 2. is signed by the signer the rule system expects at that step (Sanki's
//!    strict alternation, [`SessionParams::expected_signer`]);
//! 3. has a canonical attestation `created_at` **≤** the triggering Adjudication
//!    Request's canonical attestation `created_at` (the *cutoff*).
//!
//! The chain stops at the first step with no Ply satisfying all three. Because
//! the canonical Ply of a slot is the one with the smallest attestation
//! `created_at`, testing that single canonical Ply against the cutoff is
//! sufficient: if even it was attested after the Request, every other candidate
//! for the slot was too. A Ply signed by an unexpected signer forms a different
//! slot and simply is not a candidate for that step — it neither contributes to
//! nor disrupts the chain. Plies attested after the cutoff are excluded, so a
//! player cannot race the arbiter by playing after invoking.
//!
//! If the Request is not yet canonically attested, the cutoff is undefined and
//! the natural state cannot be computed ([`natural_state`] returns `None`); the
//! arbiter must wait for the Request to be attested.

use crate::event::{AdjudicationRequest, Attestation, Ply};
use crate::race_resolution::{canonical_attestation, canonical_ply, CanonicalPly};
use crate::session::SessionParams;
use sashite_sanki_engine::domain::time::Timestamp;

/// The natural state: the consecutive canonical Ply chain and the cutoff it was
/// computed against.
#[derive(Debug, Clone)]
pub struct NaturalState<'a> {
    /// The consecutive canonical Plies, `chain[i]` being the Ply of step `i + 1`.
    pub chain: Vec<CanonicalPly<'a>>,
    /// The cutoff: the triggering Request's canonical attestation `created_at`.
    pub cutoff: Timestamp,
}

impl NaturalState<'_> {
    /// The first step **not** in the chain — the step a continuation would
    /// occupy. With a chain of `k` plies (steps `1..=k`), this is `k + 1`.
    #[inline]
    #[must_use]
    pub fn next_step(&self) -> u32 {
        let played = u32::try_from(self.chain.len()).unwrap_or(u32::MAX);
        played.saturating_add(1)
    }

    /// Whether the chain is empty (no Ply played from step 1).
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.chain.is_empty()
    }
}

/// Computes the natural state of `plies`/`attestations` for the session, cut off
/// at the canonical attestation timing of `request`.
///
/// Returns `None` if `request` has no canonical attestation from the designated
/// timestamper (the cutoff is undefined — the arbiter must wait).
#[must_use]
pub fn natural_state<'a>(
    params: &SessionParams,
    plies: &'a [Ply],
    attestations: &'a [Attestation],
    request: &AdjudicationRequest,
) -> Option<NaturalState<'a>> {
    let timestamper = params.timestamper();
    let session = params.session();

    // The cutoff: the Request's authoritative timing. Undefined ⇒ cannot rule.
    let cutoff = canonical_attestation(attestations, request.id, timestamper)?.created_at;

    let mut chain = Vec::new();
    let mut step: u32 = 1;
    loop {
        let signer = params.expected_signer(step);
        let candidates = plies
            .iter()
            .filter(|ply| ply.session == session && ply.signer == signer && ply.step == step);

        match canonical_ply(candidates, attestations, timestamper) {
            // Canonical, expected signer, and attested at or before the cutoff.
            Some(canonical) if canonical.at <= cutoff => {
                chain.push(canonical);
                step = step.saturating_add(1);
            }
            // No qualifying Ply for this step: the chain ends here.
            _ => break,
        }
    }

    Some(NaturalState { chain, cutoff })
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use super::natural_state;
    use crate::event::{AdjudicationRequest, Attestation, EventId, Ply, PublicKey};
    use crate::session::SessionParams;
    use sashite_sanki_engine::domain::time::{Duration, Timestamp};
    use sashite_sanki_engine::domain::time_control::{Period, TimeControl};
    use sashite_sanki_engine::position::Position;

    const FIRST: u8 = 10;
    const SECOND: u8 = 20;
    const TIMESTAMPER: u8 = 99;
    const SESSION: u8 = 50;
    const REQUEST: u8 = 170;

    fn pk(byte: u8) -> PublicKey {
        PublicKey::from_bytes([byte; 32])
    }

    fn eid(byte: u8) -> EventId {
        EventId::from_bytes([byte; 32])
    }

    fn ts(secs: i64) -> Timestamp {
        Timestamp::from_unix(secs)
    }

    fn ply(id: u8, signer: u8, step: u32) -> Ply {
        Ply::new(
            eid(id),
            pk(signer),
            eid(SESSION),
            step,
            false,
            String::new(),
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

    fn request() -> AdjudicationRequest {
        AdjudicationRequest::new(eid(REQUEST), pk(FIRST), eid(SESSION), pk(2))
    }

    /// Attestation of the Request setting the cutoff.
    fn cutoff_att(at: i64) -> Attestation {
        att(171, REQUEST, at)
    }

    #[test]
    fn complete_consecutive_chain() {
        let plies = [ply(1, FIRST, 1), ply(2, SECOND, 2), ply(3, FIRST, 3)];
        let atts = [
            att(101, 1, 100),
            att(102, 2, 200),
            att(103, 3, 300),
            cutoff_att(1000),
        ];
        let ns = natural_state(&params(), &plies, &atts, &request()).expect("attested request");
        assert_eq!(ns.chain.len(), 3);
        assert_eq!(ns.next_step(), 4);
        assert_eq!(*ns.chain[0].ply.id.as_bytes(), [1; 32]);
        assert_eq!(*ns.chain[2].ply.id.as_bytes(), [3; 32]);
    }

    #[test]
    fn cutoff_inclusivity() {
        // A Ply attested exactly at the cutoff is included (the `≤` condition).
        let plies = [ply(1, FIRST, 1)];
        let atts = [att(101, 1, 1000), cutoff_att(1000)];
        let ns = natural_state(&params(), &plies, &atts, &request()).expect("attested request");
        assert_eq!(ns.chain.len(), 1);
    }

    #[test]
    fn cutoff_excludes_a_later_ply() {
        // step 3 attested after the cutoff: excluded, the chain stops at 2.
        let plies = [ply(1, FIRST, 1), ply(2, SECOND, 2), ply(3, FIRST, 3)];
        let atts = [
            att(101, 1, 100),
            att(102, 2, 200),
            att(103, 3, 2000),
            cutoff_att(1000),
        ];
        let ns = natural_state(&params(), &plies, &atts, &request()).expect("attested request");
        assert_eq!(ns.chain.len(), 2);
        assert_eq!(ns.next_step(), 3);
    }

    #[test]
    fn unexpected_signer_breaks_the_chain() {
        // step 2 signed by `first` while `second` is expected: no candidate for the
        // expected slot, the chain stops at 1.
        let plies = [ply(1, FIRST, 1), ply(2, FIRST, 2)];
        let atts = [att(101, 1, 100), att(102, 2, 200), cutoff_att(1000)];
        let ns = natural_state(&params(), &plies, &atts, &request()).expect("attested request");
        assert_eq!(ns.chain.len(), 1);
    }

    #[test]
    fn pending_ply_breaks_the_chain() {
        // step 2 present but not attested: pending, excluded → chain of 1.
        let plies = [ply(1, FIRST, 1), ply(2, SECOND, 2)];
        let atts = [att(101, 1, 100), cutoff_att(1000)];
        let ns = natural_state(&params(), &plies, &atts, &request()).expect("attested request");
        assert_eq!(ns.chain.len(), 1);
    }

    #[test]
    fn gap_in_steps_stops_the_chain() {
        // Only step 1 exists: chain of length 1, next step 2.
        let plies = [ply(1, FIRST, 1)];
        let atts = [att(101, 1, 100), cutoff_att(1000)];
        let ns = natural_state(&params(), &plies, &atts, &request()).expect("attested request");
        assert_eq!(ns.chain.len(), 1);
        assert_eq!(ns.next_step(), 2);
        assert!(!ns.is_empty());
    }

    #[test]
    fn unattested_request_yields_none() {
        // No attestation for the Request: cutoff undefined.
        let plies = [ply(1, FIRST, 1)];
        let atts = [att(101, 1, 100)];
        assert!(natural_state(&params(), &plies, &atts, &request()).is_none());
    }

    #[test]
    fn empty_chain_if_no_first_ply() {
        let plies: [Ply; 0] = [];
        let atts = [cutoff_att(1000)];
        let ns = natural_state(&params(), &plies, &atts, &request()).expect("attested request");
        assert!(ns.is_empty());
        assert_eq!(ns.next_step(), 1);
    }
}
