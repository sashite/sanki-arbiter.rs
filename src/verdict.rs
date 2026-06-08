//! Adjudication assembly and the top-level [`adjudicate`] orchestration.
//!
//! [`adjudicate`] turns a session's public events into the arbiter's binding
//! verdict (kind `6425`): a termination [`Status`] (the event's `content`) and a
//! result distribution ([`Outcome3`], the `result` tags). It composes every
//! layer below it.
//!
//! # Precedence by attestation time
//!
//! Several termination causes may coexist; the protocol leaves their
//! cross-category precedence unstated, so this orchestration ranks every
//! candidate by the **attestation time** of the event that caused it and rules
//! the earliest. Each candidate carries such a time:
//!
//! - a **commitment violation** (`illegalmove`) — the violating Ply's canonical
//!   attestation `created_at`;
//! - a **chain-replay** termination (an in-chain illegal move, a rule-system
//!   ending, or a played-ply timeout) — the terminating Ply's canonical
//!   attestation `created_at`;
//! - a **post-chain** termination on an otherwise-ongoing game (draw by
//!   agreement, resignation, or an opponent's abandonment timeout) — the cutoff
//!   (the Request's canonical attestation `created_at`).
//!
//! Ranking by time is the principled unification: an equivocation at step 3
//! preempts a checkmate at step 20, while an in-chain illegal move at step 2
//! preempts an equivocation at step 4. A commitment violation wins an exact tie.
//!
//! The **abandonment timeout** realizes kind `6424`'s abandonment-recovery: when
//! the chain is ongoing and it is the *opponent's* turn (not the invoker's), the
//! opponent's clock is ticked from the last attestation to the cutoff; if it
//! flags, they lose on time. When it is the *invoker's* turn, the implicit
//! conventions apply instead (a draw acceptance, or otherwise resignation).
//!
//! `adjudicate` returns `None` when no ruling is possible: the Request is not yet
//! canonically attested (the cutoff is undefined), or the invocation is premature
//! (the opponent is on move, within time, with no draw to accept and no
//! violation to penalize).

use crate::commitment::commitment_violation;
use crate::event::{AdjudicationRequest, Attestation, Ply};
use crate::implicit::implicit_termination;
use crate::natural_state::{natural_state, NaturalState};
use crate::session::SessionParams;
use sashite_sanki_engine::clock::tick;
use sashite_sanki_engine::domain::half_move::Move;
use sashite_sanki_engine::domain::outcome::Verdict;
use sashite_sanki_engine::domain::side::Side;
use sashite_sanki_engine::domain::status::{Outcome3, Status};
use sashite_sanki_engine::domain::time::{Duration, Timestamp};
use sashite_sanki_engine::kernel::step::step;

/// The arbiter's binding verdict: a termination status and a result
/// distribution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Adjudication {
    status: Status,
    result: Outcome3,
}

impl Adjudication {
    /// The termination cause (the Adjudication's `content`).
    #[inline]
    #[must_use]
    pub const fn status(&self) -> Status {
        self.status
    }

    /// The result distribution.
    #[inline]
    #[must_use]
    pub const fn result(&self) -> Outcome3 {
        self.result
    }

    /// The score (`0`, `50`, or `100`) assigned to `side`.
    #[inline]
    #[must_use]
    pub const fn score(&self, side: Side) -> u8 {
        match (self.result, side) {
            (Outcome3::Draw, _) => 50,
            (Outcome3::FirstWins, Side::First) | (Outcome3::SecondWins, Side::Second) => 100,
            _ => 0,
        }
    }

    /// Builds an adjudication from a terminal verdict, or `None` if the verdict
    /// is `Ongoing`.
    #[inline]
    fn from_verdict(verdict: Verdict) -> Option<Self> {
        match verdict {
            Verdict::Terminated { status, result } => Some(Self { status, result }),
            Verdict::Ongoing => None,
        }
    }
}

/// Rules on a session from its public events, cut off at the triggering
/// Request's canonical attestation.
///
/// Returns `None` when no ruling is possible (the Request is not yet attested, or
/// the invocation is premature).
#[must_use]
pub fn adjudicate(
    params: &SessionParams,
    plies: &[Ply],
    attestations: &[Attestation],
    request: &AdjudicationRequest,
) -> Option<Adjudication> {
    // The natural state is also the gate: no canonical Request attestation, no
    // cutoff, no ruling.
    let natural = natural_state(params, plies, attestations, request)?;

    // Candidate 1 — a commitment violation, timed at the violating Ply.
    let commitment =
        commitment_violation(params, plies, attestations, natural.cutoff).map(|violation| {
            (
                Verdict::decisive(Status::IllegalMove, violation.loser),
                violation.at,
            )
        });

    // Candidate 2 — what the play itself produces (replay + post-chain).
    let play = resolve_play(params, &natural, request);

    // The earliest candidate rules; a commitment violation wins an exact tie.
    let verdict = match (commitment, play) {
        (Some(commitment), Some(play)) => {
            if commitment.1 <= play.1 {
                commitment.0
            } else {
                play.0
            }
        }
        (Some(commitment), None) => commitment.0,
        (None, Some(play)) => play.0,
        (None, None) => return None,
    };

    Adjudication::from_verdict(verdict)
}

/// Replays the canonical chain through the kernel and, if the game is still
/// ongoing at its end, resolves the invocation (agreement / resignation /
/// abandonment timeout). Returns the verdict and the attestation time that
/// caused it, or `None` when the game is ongoing and the invocation is premature.
fn resolve_play(
    params: &SessionParams,
    natural: &NaturalState<'_>,
    request: &AdjudicationRequest,
) -> Option<(Verdict, Timestamp)> {
    let mut state = params.initial_state();

    for canonical in &natural.chain {
        // A chain Ply whose content does not parse is an illegal move by its
        // signer (the expected signer at that step).
        let Ok(mv) = Move::parse(&canonical.ply.content) else {
            let loser = params.expected_side(canonical.ply.step);
            return Some((Verdict::decisive(Status::IllegalMove, loser), canonical.at));
        };

        let outcome = step(state, &mv, canonical.at);
        match outcome.next {
            Some(next) => state = next,
            // This Ply terminates the game (illegal move, rule-system ending, or
            // a played-ply timeout).
            None => return Some((outcome.outcome.verdict, canonical.at)),
        }
    }

    // The chain replayed without terminating. Resolve the invocation.
    if let Some(verdict) = implicit_termination(params, natural, request) {
        return Some((verdict, natural.cutoff));
    }

    // Abandonment timeout: it is the opponent's turn and they let their clock run
    // out before the cutoff.
    let on_move = state.position().active_side();
    let elapsed = natural
        .cutoff
        .duration_since(state.last_attestation())
        .unwrap_or(Duration::ZERO);
    if tick(params.time_control(), state.clocks().get(on_move), elapsed).is_flagged() {
        return Some((Verdict::decisive(Status::Timeout, on_move), natural.cutoff));
    }

    None
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use super::adjudicate;
    use crate::event::{AdjudicationRequest, Attestation, EventId, Ply, PublicKey};
    use crate::session::SessionParams;
    use sashite_sanki_engine::domain::status::{Outcome3, Status};
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

    fn ply_draw(id: u8, signer: u8, step: u32, content: &str) -> Ply {
        Ply::new(
            eid(id),
            pk(signer),
            eid(SESSION),
            step,
            true,
            content.to_owned(),
        )
    }

    fn att(id: u8, attests: u8, at: i64) -> Attestation {
        Attestation::new(eid(id), pk(TIMESTAMPER), eid(attests), ts(at))
    }

    fn request(signer: u8) -> AdjudicationRequest {
        AdjudicationRequest::new(eid(REQUEST), pk(signer), eid(SESSION), pk(2))
    }

    fn params(feen: &str, tc_secs: u64, anchor: i64) -> SessionParams {
        let period = Period::new(Duration::from_secs(tc_secs), None, None).expect("period");
        SessionParams::new(
            eid(SESSION),
            pk(2),
            pk(TIMESTAMPER),
            pk(FIRST),
            pk(SECOND),
            TimeControl::new(period, Vec::new()),
            Position::parse(feen).expect("valid FEEN"),
            ts(anchor),
        )
    }

    #[test]
    fn mate_by_chain_replay() {
        // Ra1-a8 mates the walled-in black King: checkmate, first player wins.
        let plies = [ply(1, FIRST, 1, "[\"a1\",\"a8\",null]")];
        let atts = [att(101, 1, 100), att(171, REQUEST, 1000)];
        let p = params("7k^/6pp/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        let adj = adjudicate(&p, &plies, &atts, &request(SECOND)).expect("verdict");
        assert_eq!(adj.status(), Status::Checkmate);
        assert_eq!(adj.result(), Outcome3::FirstWins);
    }

    #[test]
    fn illegal_move_in_the_chain() {
        // No piece on a1: illegal move by the first player.
        let plies = [ply(1, FIRST, 1, "[\"a1\",\"a4\",null]")];
        let atts = [att(101, 1, 100), att(171, REQUEST, 1000)];
        let p = params("4k^3/8/8/8/8/8/8/4K^3 / W/w", 600, 0);
        let adj = adjudicate(&p, &plies, &atts, &request(SECOND)).expect("verdict");
        assert_eq!(adj.status(), Status::IllegalMove);
        assert_eq!(adj.result(), Outcome3::SecondWins);
    }

    #[test]
    fn early_commitment_violation_outranks_play() {
        // The first player equivocates at step 1 (canonical a4 @100, divergent a5
        // @200); the game would continue (resignation at cutoff 1000), but the
        // violation (@200), being earlier, wins: illegalmove against the first player.
        let plies = [
            ply(1, FIRST, 1, "[\"a1\",\"a4\",null]"),
            ply(2, FIRST, 1, "[\"a1\",\"a5\",null]"),
        ];
        let atts = [att(101, 1, 100), att(102, 2, 200), att(171, REQUEST, 1000)];
        let p = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        let adj = adjudicate(&p, &plies, &atts, &request(SECOND)).expect("verdict");
        assert_eq!(adj.status(), Status::IllegalMove);
        assert_eq!(adj.result(), Outcome3::SecondWins);
    }

    #[test]
    fn implicit_resignation() {
        // The second player invokes on their own turn (step 2) without playing:
        // they resign.
        let plies = [ply(1, FIRST, 1, "[\"a1\",\"a4\",null]")];
        let atts = [att(101, 1, 100), att(171, REQUEST, 1000)];
        let p = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        let adj = adjudicate(&p, &plies, &atts, &request(SECOND)).expect("verdict");
        assert_eq!(adj.status(), Status::Resignation);
        assert_eq!(adj.result(), Outcome3::FirstWins);
    }

    #[test]
    fn draw_by_agreement() {
        // The first player offers the draw (draw flag); the second accepts it by
        // invoking.
        let plies = [ply_draw(1, FIRST, 1, "[\"a1\",\"a4\",null]")];
        let atts = [att(101, 1, 100), att(171, REQUEST, 1000)];
        let p = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        let adj = adjudicate(&p, &plies, &atts, &request(SECOND)).expect("verdict");
        assert_eq!(adj.status(), Status::Agreement);
        assert_eq!(adj.result(), Outcome3::Draw);
    }

    #[test]
    fn abandonment_timeout() {
        // The first player moves (elapsed 100 ≤ 600), then the second lets their
        // clock run to the cutoff (elapsed 900 > 600); the first player invokes.
        let plies = [ply(1, FIRST, 1, "[\"a1\",\"a4\",null]")];
        let atts = [att(101, 1, 100), att(171, REQUEST, 1000)];
        let p = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        let adj = adjudicate(&p, &plies, &atts, &request(FIRST)).expect("verdict");
        assert_eq!(adj.status(), Status::Timeout);
        assert_eq!(adj.result(), Outcome3::FirstWins);
    }

    #[test]
    fn unattested_request_no_verdict() {
        let plies = [ply(1, FIRST, 1, "[\"a1\",\"a4\",null]")];
        let atts = [att(101, 1, 100)]; // no attestation for the Request
        let p = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        assert!(adjudicate(&p, &plies, &atts, &request(SECOND)).is_none());
    }

    #[test]
    fn premature_invocation_no_verdict() {
        // The first player invokes on the second's turn, who still has time
        // (elapsed 100 ≤ 600): no termination cause.
        let plies = [ply(1, FIRST, 1, "[\"a1\",\"a4\",null]")];
        let atts = [att(101, 1, 100), att(171, REQUEST, 200)];
        let p = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        assert!(adjudicate(&p, &plies, &atts, &request(FIRST)).is_none());
    }

    #[test]
    fn score_per_side() {
        let plies = [ply(1, FIRST, 1, "[\"a1\",\"a8\",null]")];
        let atts = [att(101, 1, 100), att(171, REQUEST, 1000)];
        let p = params("7k^/6pp/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        let adj = adjudicate(&p, &plies, &atts, &request(SECOND)).expect("verdict");
        assert_eq!(
            adj.score(sashite_sanki_engine::domain::side::Side::First),
            100
        );
        assert_eq!(
            adj.score(sashite_sanki_engine::domain::side::Side::Second),
            0
        );
    }
}
