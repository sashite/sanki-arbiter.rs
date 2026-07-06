//! The natural state of events at adjudication (kind `6425` §Natural state).
//!
//! When the arbiter rules, it replays the session's play order from its first
//! half-move, selecting the canonical Ply for each successive slot under the
//! **forgiving-premove** rule ([`crate::selection`]) and applying it through the
//! engine. The replay is a single pass that is at once the chain builder and the
//! legality authority — a slot's selection depends on each candidate's legality
//! in the *replayed* position, so the two cannot be separated.
//!
//! For each play-order position (`(signer, step)` under Sanki's strict
//! alternation), the candidates are the Plies for that slot whose canonical
//! attestation `created_at` lies in `[t₀, cutoff]` — `t₀` the session start, the
//! `cutoff` the triggering Request's canonical attestation (so a player cannot
//! race the arbiter by playing after invoking). The slot's **anchor** is the
//! predecessor half-move's canonical attestation (`t₀` for the first slot), and
//! [`select_candidate`] resolves the candidates against it (the boundary `T`):
//!
//! - **applied** — the selected Ply is applied to the board: the *latest* legal
//!   premove (anterior, timed before `T`), else the *earliest* legal live move
//!   (informed, timed at/after `T`). An illegal candidate — premove or live — is
//!   skipped, never sanctioned;
//! - **unfilled** — no candidate is legal in either window: the chain stops, still
//!   ongoing.
//!
//! Applying a selected Ply through the engine ([`step`]) also surfaces a
//! rule-system ending (checkmate, …) or a played-Ply timeout, which terminates the
//! chain. The replay therefore yields either a **terminal verdict** (rule-system
//! ending / timeout, with the attestation time that caused it) or a still-**ongoing**
//! end position for the post-chain resolution ([`crate::verdict`]). There is no
//! `illegalmove` termination — an illegal Ply is skipped, never a loss.
//!
//! If the Request is not yet canonically attested the cutoff is undefined and the
//! natural state cannot be computed ([`natural_state`] returns `None`).

use crate::event::{AdjudicationRequest, Attestation, EventId, Ply};
use crate::race_resolution::{canonical_attestation, CanonicalPly};
use crate::selection::{select_candidate, Candidate, Selection, CANDIDATE_CAP};
use crate::session::SessionParams;
use sashite_sanki_engine::domain::half_move::Move;
use sashite_sanki_engine::domain::outcome::Verdict;
use sashite_sanki_engine::domain::status::Status;
use sashite_sanki_engine::domain::time::Timestamp;
use sashite_sanki_engine::kernel::state::SessionState;
use sashite_sanki_engine::kernel::step::step;

/// How the replayed chain ends.
#[derive(Debug, Clone)]
pub enum Conclusion {
    /// The chain reached a terminal verdict during replay — a rule-system ending
    /// or a played-Ply timeout — at the given attestation time. Post-chain
    /// resolution does not apply.
    Terminal(Verdict, Timestamp),
    /// The chain replayed to a still-ongoing position: post-chain resolution
    /// (draw acceptance, abandonment timeout, residual resignation) decides the
    /// verdict on this state. Boxed — a [`SessionState`] dwarfs the terminal
    /// variant, so the box keeps the enum small.
    Ongoing(Box<SessionState>),
}

/// The natural state: the selected canonical Ply chain, the cutoff it was
/// computed against, and how the chain concluded.
#[derive(Debug, Clone)]
pub struct NaturalState<'a> {
    /// The selected canonical Plies, `chain[i]` being the Ply at play-order
    /// position `i + 1`. A skipped illegal candidate is **not** included (it is not
    /// a played half-move); a terminating *applied* Ply (a mating move, …) **is**.
    pub chain: Vec<CanonicalPly<'a>>,
    /// The cutoff: the triggering Request's canonical attestation `created_at`.
    pub cutoff: Timestamp,
    /// How the chain concluded (terminal verdict or ongoing end position).
    pub conclusion: Conclusion,
}

impl NaturalState<'_> {
    /// The first play-order position **not** filled by an applied Ply — the
    /// position a continuation would occupy. With a chain of `k` half-moves,
    /// this is `k + 1`.
    #[inline]
    #[must_use]
    pub fn next_half_move(&self) -> u32 {
        let played = u32::try_from(self.chain.len()).unwrap_or(u32::MAX);
        played.saturating_add(1)
    }

    /// Whether the chain is empty (no applied Ply from step 1).
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.chain.is_empty()
    }
}

/// Whether `content` is a legal half-move in `position`'s state, under the full
/// rule system (the engine's `step` path, which includes ōgi uchifuzume — not the
/// mechanical `engine::validate`). Legality is resolved **before** the clock, so a
/// legal-but-timed-out move is still legal here; an unparseable content is illegal.
fn is_legal(state: &SessionState, content: &str, at: Timestamp) -> bool {
    let Ok(mv) = Move::parse(content) else {
        return false;
    };
    let outcome = step(state.clone(), &mv, at).outcome;
    !matches!(
        outcome.verdict,
        Verdict::Terminated {
            status: Status::IllegalMove,
            ..
        }
    )
}

/// A slot candidate paired with its source Ply (so the selection can be mapped
/// back to the played event).
struct SlotCandidate<'a> {
    ply: &'a Ply,
    candidate: Candidate<EventId>,
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
    let start = params.anchor(); // t₀: the lower bound and the first slot's anchor.

    // The cutoff: the Request's authoritative timing. Undefined ⇒ cannot rule.
    let cutoff = canonical_attestation(attestations, request.id, timestamper)?.created_at;

    let mut chain: Vec<CanonicalPly<'a>> = Vec::new();
    let mut state = params.initial_state();
    let mut anchor = start;
    let mut half_move: u32 = 1;

    let conclusion = loop {
        let signer = params.player_at(half_move);
        let step_no = params.step_at(half_move);

        // Candidates for this slot: canonically attested within [t₀, cutoff],
        // each judged for legality on the live replayed board.
        let slot: Vec<SlotCandidate<'a>> = plies
            .iter()
            .filter(|ply| ply.session == session && ply.signer == signer && ply.step == step_no)
            .filter_map(|ply| {
                let at = canonical_attestation(attestations, ply.id, timestamper)?.created_at;
                (at >= start && at <= cutoff).then(|| SlotCandidate {
                    ply,
                    candidate: Candidate {
                        id: ply.id,
                        created_at: at,
                        legal: is_legal(&state, &ply.content, at),
                    },
                })
            })
            .collect();

        let candidates: Vec<Candidate<EventId>> = slot.iter().map(|sc| sc.candidate).collect();

        match select_candidate(anchor, &candidates, CANDIDATE_CAP) {
            // No candidate is legal in either window: the chain stops, still ongoing.
            Selection::Unfilled => break Conclusion::Ongoing(Box::new(state)),

            // A candidate fills the slot: apply it and advance (or terminate on a
            // rule-system ending / timeout the application surfaces).
            Selection::Applied(chosen) => {
                let at = chosen.created_at;
                let Some(ply) = slot
                    .iter()
                    .find(|sc| sc.ply.id == chosen.id)
                    .map(|sc| sc.ply)
                else {
                    // Unreachable: the selected candidate is one of this slot's
                    // candidates. Degrade safely to an ongoing chain end.
                    break Conclusion::Ongoing(Box::new(state));
                };

                // Selection guarantees legality, so the content parses; a defensive
                // failure stops the chain safely (an illegal Ply is never a loss).
                let Ok(mv) = Move::parse(&ply.content) else {
                    break Conclusion::Ongoing(Box::new(state));
                };

                let result = step(state, &mv, at);
                chain.push(CanonicalPly { ply, at });
                match result.next {
                    Some(next) => {
                        state = next;
                        anchor = at;
                        half_move = half_move.saturating_add(1);
                    }
                    None => break Conclusion::Terminal(result.outcome.verdict, at),
                }
            }
        }
    };

    Some(NaturalState {
        chain,
        cutoff,
        conclusion,
    })
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use super::{natural_state, Conclusion};
    use crate::event::{AdjudicationRequest, Attestation, EventId, Ply, PublicKey};
    use crate::session::SessionParams;
    use sashite_sanki_engine::domain::outcome::Verdict;
    use sashite_sanki_engine::domain::status::Status;
    use sashite_sanki_engine::domain::time::{Duration, Timestamp};
    use sashite_sanki_engine::domain::time_control::{Period, TimeControl};
    use sashite_sanki_engine::position::Position;

    const FIRST: u8 = 10;
    const SECOND: u8 = 20;
    const TIMESTAMPER: u8 = 99;
    const SESSION: u8 = 50;
    const REQUEST: u8 = 170;

    // A chess rook-and-king endgame: white Rook a1, white King e1, black King e8.
    // White to move. Gives a stock of legal moves for the chain tests.
    const ROOK_KING: &str = "4k^3/8/8/8/8/8/8/R3K^3 / W/w";

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

    fn params_feen(feen: &str) -> SessionParams {
        let period = Period::new(Duration::from_secs(600), None, None).expect("valid period");
        SessionParams::new(
            eid(SESSION),
            pk(2),
            pk(TIMESTAMPER),
            pk(FIRST),
            pk(SECOND),
            TimeControl::new(period, Vec::new()),
            Position::parse(feen).expect("valid FEEN"),
            ts(0),
        )
    }

    fn params() -> SessionParams {
        params_feen(ROOK_KING)
    }

    fn request() -> AdjudicationRequest {
        AdjudicationRequest::new(eid(REQUEST), pk(FIRST), eid(SESSION), pk(2))
    }

    fn cutoff_att(at: i64) -> Attestation {
        att(171, REQUEST, at)
    }

    // Legal moves in the ROOK_KING line.
    const RA1A4: &str = "[\"a1\",\"a4\",null]"; // first, step 1
    const KE8E7: &str = "[\"e8\",\"e7\",null]"; // second, step 1
    const RA4A5: &str = "[\"a4\",\"a5\",null]"; // first, step 2

    #[test]
    fn complete_consecutive_chain() {
        let plies = [
            ply(1, FIRST, 1, RA1A4),
            ply(2, SECOND, 1, KE8E7),
            ply(3, FIRST, 2, RA4A5),
        ];
        let atts = [
            att(101, 1, 100),
            att(102, 2, 200),
            att(103, 3, 300),
            cutoff_att(1000),
        ];
        let ns = natural_state(&params(), &plies, &atts, &request()).expect("attested request");
        assert_eq!(ns.chain.len(), 3);
        assert_eq!(ns.next_half_move(), 4);
        assert_eq!(*ns.chain[0].ply.id.as_bytes(), [1; 32]);
        assert_eq!(*ns.chain[2].ply.id.as_bytes(), [3; 32]);
        assert!(matches!(ns.conclusion, Conclusion::Ongoing(_)));
    }

    #[test]
    fn cutoff_inclusivity() {
        // A Ply attested exactly at the cutoff is included (the `≤` condition).
        let plies = [ply(1, FIRST, 1, RA1A4)];
        let atts = [att(101, 1, 1000), cutoff_att(1000)];
        let ns = natural_state(&params(), &plies, &atts, &request()).expect("attested request");
        assert_eq!(ns.chain.len(), 1);
    }

    #[test]
    fn cutoff_excludes_a_later_ply() {
        // Position 3 attested after the cutoff: excluded, the chain stops at 2.
        let plies = [
            ply(1, FIRST, 1, RA1A4),
            ply(2, SECOND, 1, KE8E7),
            ply(3, FIRST, 2, RA4A5),
        ];
        let atts = [
            att(101, 1, 100),
            att(102, 2, 200),
            att(103, 3, 2000),
            cutoff_att(1000),
        ];
        let ns = natural_state(&params(), &plies, &atts, &request()).expect("attested request");
        assert_eq!(ns.chain.len(), 2);
        assert_eq!(ns.next_half_move(), 3);
    }

    #[test]
    fn opponent_slot_cannot_be_filled() {
        // `first` premoves their own step 2 while `second` never plays step 1:
        // position 2 expects (second, step 1) — `first`'s extra Ply is a
        // future-slot Ply and cannot fill it. The chain stops at 1.
        let plies = [ply(1, FIRST, 1, RA1A4), ply(2, FIRST, 2, RA4A5)];
        let atts = [att(101, 1, 100), att(102, 2, 200), cutoff_att(1000)];
        let ns = natural_state(&params(), &plies, &atts, &request()).expect("attested request");
        assert_eq!(ns.chain.len(), 1);
        assert!(matches!(ns.conclusion, Conclusion::Ongoing(_)));
    }

    #[test]
    fn pending_ply_breaks_the_chain() {
        // (second, step 1) present but not attested: pending, excluded → chain of 1.
        let plies = [ply(1, FIRST, 1, RA1A4), ply(2, SECOND, 1, KE8E7)];
        let atts = [att(101, 1, 100), cutoff_att(1000)];
        let ns = natural_state(&params(), &plies, &atts, &request()).expect("attested request");
        assert_eq!(ns.chain.len(), 1);
    }

    #[test]
    fn gap_in_play_order_stops_the_chain() {
        let plies = [ply(1, FIRST, 1, RA1A4)];
        let atts = [att(101, 1, 100), cutoff_att(1000)];
        let ns = natural_state(&params(), &plies, &atts, &request()).expect("attested request");
        assert_eq!(ns.chain.len(), 1);
        assert_eq!(ns.next_half_move(), 2);
        assert!(!ns.is_empty());
    }

    #[test]
    fn deep_premove_activates_by_chain_progression() {
        // `first` publishes step 1 (informed @100) and step 2 (a premove @110,
        // attested before second's reply @200 — anterior to its slot 3); `second`
        // answers step 1 @200. The interleaved chain consumes all three, the
        // step-2 premove applying as a forgiving anterior selection.
        let plies = [
            ply(1, FIRST, 1, RA1A4),
            ply(3, FIRST, 2, RA4A5),
            ply(2, SECOND, 1, KE8E7),
        ];
        let atts = [
            att(101, 1, 100),
            att(103, 3, 110), // premove attested before second's reply
            att(102, 2, 200),
            cutoff_att(1000),
        ];
        let ns = natural_state(&params(), &plies, &atts, &request()).expect("attested request");
        assert_eq!(ns.chain.len(), 3);
        assert_eq!(*ns.chain[2].ply.id.as_bytes(), [3; 32]);
    }

    #[test]
    fn re_premove_correction_supersedes_illegal_premove() {
        // `first` plays Ra1-a4 informed @200. `second` premoved two candidates for
        // slot 2, both anterior (before 200): an illegal Ke8-e6 (older @50) and a
        // newer legal Ke8-e7 (@60). The anterior window binds the LATEST legal
        // premove: the illegal older one is skipped and the newer legal correction
        // fills the slot — chain of 2.
        let plies = [
            ply(1, FIRST, 1, RA1A4),
            ply(2, SECOND, 1, "[\"e8\",\"e6\",null]"), // illegal (king moves two), older @50
            ply(3, SECOND, 1, KE8E7),                  // legal, newer @60 -> wins
        ];
        let atts = [
            att(101, 1, 200),
            att(102, 2, 50),
            att(103, 3, 60),
            cutoff_att(1000),
        ];
        let ns = natural_state(&params(), &plies, &atts, &request()).expect("attested request");
        assert_eq!(ns.chain.len(), 2);
        assert_eq!(*ns.chain[1].ply.id.as_bytes(), [3; 32]);
        assert!(matches!(ns.conclusion, Conclusion::Ongoing(_)));
    }

    #[test]
    fn informed_illegal_is_skipped_leaving_ongoing() {
        // `first` plays Ra1-a4 @100 (applied); `second` then plays an informed
        // illegal move (Ke8-e6 @200, ≥ boundary 100). Under the two-window rule it is
        // skipped (no `illegalmove`), leaving the slot unfilled and the chain ongoing.
        let plies = [
            ply(1, FIRST, 1, RA1A4),
            ply(2, SECOND, 1, "[\"e8\",\"e6\",null]"),
        ];
        let atts = [att(101, 1, 100), att(102, 2, 200), cutoff_att(1000)];
        let ns = natural_state(&params(), &plies, &atts, &request()).expect("attested request");
        assert_eq!(ns.chain.len(), 1); // the illegal live move is skipped, not in the chain
        assert!(matches!(ns.conclusion, Conclusion::Ongoing(_)));
    }

    #[test]
    fn mating_move_terminates_the_chain() {
        // Ra1-a8 mates the walled-in black King: a rule-system ending surfaced by
        // applying the move.
        let plies = [ply(1, FIRST, 1, "[\"a1\",\"a8\",null]")];
        let atts = [att(101, 1, 100), cutoff_att(1000)];
        let p = params_feen("7k^/6pp/8/8/8/8/8/R3K^3 / W/w");
        let ns = natural_state(&p, &plies, &atts, &request()).expect("attested request");
        assert_eq!(ns.chain.len(), 1); // the mating move is part of the chain
        match ns.conclusion {
            Conclusion::Terminal(verdict, at) => {
                assert!(matches!(
                    verdict,
                    Verdict::Terminated {
                        status: Status::Checkmate,
                        ..
                    }
                ));
                assert_eq!(at, ts(100));
            }
            Conclusion::Ongoing(_) => panic!("expected a checkmate termination"),
        }
    }

    #[test]
    fn unattested_request_yields_none() {
        let plies = [ply(1, FIRST, 1, RA1A4)];
        let atts = [att(101, 1, 100)];
        assert!(natural_state(&params(), &plies, &atts, &request()).is_none());
    }

    #[test]
    fn empty_chain_if_no_first_ply() {
        let plies: [Ply; 0] = [];
        let atts = [cutoff_att(1000)];
        let ns = natural_state(&params(), &plies, &atts, &request()).expect("attested request");
        assert!(ns.is_empty());
        assert_eq!(ns.next_half_move(), 1);
        assert!(matches!(ns.conclusion, Conclusion::Ongoing(_)));
    }
}
