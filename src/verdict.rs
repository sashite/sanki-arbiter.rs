//! Adjudication assembly and the top-level [`adjudicate`] orchestration.
//!
//! [`adjudicate`] turns a session's public events into the arbiter's binding
//! verdict (kind `6425`), per Statuses — Sanki §Verdict resolution: a
//! termination [`Status`] (the event's `content`) and a result distribution
//! ([`Outcome3`], the `result` tags). It composes every layer below it.
//!
//! # Verdict resolution
//!
//! Two candidate families may coexist in the same natural state and are ranked
//! by **attestation time** — each candidate is anchored to the canonical
//! attestation `created_at` of the event that produced it, the earliest cause
//! rules, and an **equivocation wins an exact tie**:
//!
//! - the **equivocation** candidate (`illegalmove`) — a violation of the
//!   single-content rule, anchored at the second-attested differing Ply of a
//!   `(session, signer, step)` slot ([`crate::commitment`]); it may sit
//!   anywhere in the session, including on a pending slot;
//! - the **play-derived** candidate, computed in two stages, mutually
//!   exclusive by construction:
//!   1. **chain replay** — the canonical chain is replayed in the play order,
//!      evaluating the canonical Ply at each successive slot; an illegal or
//!      unparseable evaluated Ply (`illegalmove`), a rule-system ending, or a
//!      played-Ply timeout (`timeout`) terminates at that Ply's attestation;
//!   2. **post-chain resolution** — on a still-ongoing position, the
//!      invocation itself is resolved at the cutoff, in order: draw acceptance
//!      (`agreement`, [`crate::implicit`]); abandonment timeout (`timeout`:
//!      the on-move player's clock, ticked from the chain's last attestation —
//!      or t₀ for an empty chain — to the cutoff, has expired); otherwise
//!      **residual resignation** (`resignation`, decisive against the invoker,
//!      whatever the turn).
//!
//! Because resignation is the residual interpretation, a conforming,
//! canonically attested Request from a session player **always yields a
//! verdict**. [`adjudicate`] returns `None` only when the Request is not yet
//! canonically attested (the cutoff is undefined) or its signer is not a
//! session player (a non-conforming Request, kind `6424` §Semantic
//! constraints).
//!
//! Selecting **which** Request to rule on — several may coexist, and the
//! choice fixes the cutoff, hence the verdict — is the caller's concern:
//! Sashité's arbiter rules on the earliest canonically attested conforming
//! Request not yet adjudicated (Statuses — Sanki §Which Request rules).

use crate::commitment::equivocation;
use crate::event::{AdjudicationRequest, Attestation, Ply};
use crate::implicit::draw_acceptance;
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
    /// is `Ongoing` (unreachable from [`adjudicate`], kept as a defensive seam).
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
/// Returns `None` when no ruling is possible: the Request is not yet
/// canonically attested, or its signer is not a session player (a
/// non-conforming Request).
#[must_use]
pub fn adjudicate(
    params: &SessionParams,
    plies: &[Ply],
    attestations: &[Attestation],
    request: &AdjudicationRequest,
) -> Option<Adjudication> {
    // A Request from a non-player is non-conforming (kind 6424 §Semantic
    // constraints, item 3): no ruling.
    let invoker = params.side_of(request.signer)?;

    // The natural state is also the gate: no canonical Request attestation, no
    // cutoff, no ruling.
    let natural = natural_state(params, plies, attestations, request)?;

    // Candidate 1 — an equivocation, anchored at the violating Ply.
    let violation = equivocation(params, plies, attestations, natural.cutoff).map(|violation| {
        (
            Verdict::decisive(Status::IllegalMove, violation.loser),
            violation.at,
        )
    });

    // Candidate 2 — what the play itself produces (replay + post-chain).
    let play = resolve_play(params, &natural, request, invoker);

    // The earliest candidate rules; an equivocation wins an exact tie.
    let verdict = match violation {
        Some(violation) if violation.1 <= play.1 => violation.0,
        _ => play.0,
    };

    Adjudication::from_verdict(verdict)
}

/// Replays the canonical chain through the kernel and, if the game is still
/// ongoing at its end, resolves the invocation at the cutoff — in order: draw
/// acceptance, abandonment timeout, residual resignation. Returns the verdict
/// and the attestation time that caused it.
fn resolve_play(
    params: &SessionParams,
    natural: &NaturalState<'_>,
    request: &AdjudicationRequest,
    invoker: Side,
) -> (Verdict, Timestamp) {
    let mut state = params.initial_state();

    for canonical in &natural.chain {
        // An evaluated Ply whose content does not parse is an illegal move by
        // its signer — the side on move at this point of the replay.
        let Ok(mv) = Move::parse(&canonical.ply.content) else {
            let loser = state.position().active_side();
            return (Verdict::decisive(Status::IllegalMove, loser), canonical.at);
        };

        let outcome = step(state, &mv, canonical.at);
        match outcome.next {
            Some(next) => state = next,
            // This Ply terminates the game (illegal move, rule-system ending, or
            // a played-ply timeout).
            None => return (outcome.outcome.verdict, canonical.at),
        }
    }

    // The chain replayed to a still-ongoing position. Resolve the invocation,
    // in order (Statuses — Sanki §Verdict resolution, stage 2).

    // 2a. Draw acceptance: a standing offer accepted by the offeree.
    if let Some(verdict) = draw_acceptance(params, natural, request) {
        return (verdict, natural.cutoff);
    }

    // 2b. Abandonment timeout: the player on move let their clock run out
    // before the cutoff (whether or not they are the invoker).
    let on_move = state.position().active_side();
    let elapsed = natural
        .cutoff
        .duration_since(state.last_attestation())
        .unwrap_or(Duration::ZERO);
    if tick(params.time_control(), state.clocks().get(on_move), elapsed).is_flagged() {
        return (Verdict::decisive(Status::Timeout, on_move), natural.cutoff);
    }

    // 2c. Residual resignation: the invocation matches no other cause, so the
    // invoker abandons — whatever the turn.
    (
        Verdict::decisive(Status::Resignation, invoker),
        natural.cutoff,
    )
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
        // No piece on a1: illegal evaluated Ply by the first player.
        let plies = [ply(1, FIRST, 1, "[\"a1\",\"a4\",null]")];
        let atts = [att(101, 1, 100), att(171, REQUEST, 1000)];
        let p = params("4k^3/8/8/8/8/8/8/4K^3 / W/w", 600, 0);
        let adj = adjudicate(&p, &plies, &atts, &request(SECOND)).expect("verdict");
        assert_eq!(adj.status(), Status::IllegalMove);
        assert_eq!(adj.result(), Outcome3::SecondWins);
    }

    #[test]
    fn equivocation_outranks_play() {
        // The first player equivocates at their step 1 (canonical a4 @100,
        // divergent a5 @200); the play-derived candidate (an abandonment timeout
        // against the second player at the cutoff 1000) is later, so the earlier
        // violation rules: illegalmove against the first player.
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
    fn equivocation_wins_an_exact_tie() {
        // The divergent content is attested exactly at the cutoff (1000), where
        // the play-derived candidate is an abandonment timeout against the second
        // player. Without the tie rule the timeout would make the first player
        // win; the equivocation (by the first player) wins the tie instead.
        let plies = [
            ply(1, FIRST, 1, "[\"a1\",\"a4\",null]"),
            ply(2, FIRST, 1, "[\"a1\",\"a5\",null]"),
        ];
        let atts = [att(101, 1, 100), att(102, 2, 1000), att(171, REQUEST, 1000)];
        let p = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        let adj = adjudicate(&p, &plies, &atts, &request(FIRST)).expect("verdict");
        assert_eq!(adj.status(), Status::IllegalMove);
        assert_eq!(adj.result(), Outcome3::SecondWins);
    }

    #[test]
    fn own_turn_invocation_without_cause_is_resignation() {
        // The second player invokes on their own turn without playing, well
        // within their time (elapsed 300 ≤ 600): residual resignation.
        let plies = [ply(1, FIRST, 1, "[\"a1\",\"a4\",null]")];
        let atts = [att(101, 1, 100), att(171, REQUEST, 400)];
        let p = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        let adj = adjudicate(&p, &plies, &atts, &request(SECOND)).expect("verdict");
        assert_eq!(adj.status(), Status::Resignation);
        assert_eq!(adj.result(), Outcome3::FirstWins);
    }

    #[test]
    fn off_turn_invocation_without_cause_is_resignation() {
        // The first player invokes while the second is on move and within time
        // (elapsed 300 ≤ 600): residual resignation against the invoker —
        // invocation is turn-independent.
        let plies = [ply(1, FIRST, 1, "[\"a1\",\"a4\",null]")];
        let atts = [att(101, 1, 100), att(171, REQUEST, 400)];
        let p = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        let adj = adjudicate(&p, &plies, &atts, &request(FIRST)).expect("verdict");
        assert_eq!(adj.status(), Status::Resignation);
        assert_eq!(adj.result(), Outcome3::SecondWins);
    }

    #[test]
    fn draw_by_agreement() {
        // The first player offers the draw (draw flag); the second accepts it by
        // invoking. Checked before the abandonment timeout: even with the
        // second player's clock expired at the cutoff (900 > 600), the
        // acceptance rules.
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
    fn own_expired_clock_is_a_timeout_not_a_resignation() {
        // The second player, on move with their clock expired (900 > 600),
        // invokes: the abandonment timeout is tested before the residual
        // resignation — a loss on time, against the invoker.
        let plies = [ply(1, FIRST, 1, "[\"a1\",\"a4\",null]")];
        let atts = [att(101, 1, 100), att(171, REQUEST, 1000)];
        let p = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        let adj = adjudicate(&p, &plies, &atts, &request(SECOND)).expect("verdict");
        assert_eq!(adj.status(), Status::Timeout);
        assert_eq!(adj.result(), Outcome3::FirstWins);
    }

    #[test]
    fn empty_chain_invocation_is_resignation() {
        // No move played, both within time (cutoff 400 ≤ 600): whoever invokes
        // resigns.
        let plies: [Ply; 0] = [];
        let atts = [att(171, REQUEST, 400)];
        let p = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        let adj = adjudicate(&p, &plies, &atts, &request(SECOND)).expect("verdict");
        assert_eq!(adj.status(), Status::Resignation);
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
    fn non_player_request_no_verdict() {
        // A Request signed by a non-player is non-conforming: no ruling.
        let plies = [ply(1, FIRST, 1, "[\"a1\",\"a4\",null]")];
        let atts = [att(101, 1, 100), att(171, REQUEST, 1000)];
        let p = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        assert!(adjudicate(&p, &plies, &atts, &request(77)).is_none());
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
