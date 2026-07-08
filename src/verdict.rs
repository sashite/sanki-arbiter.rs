//! Adjudication assembly and the top-level [`adjudicate`] orchestration.
//!
//! [`adjudicate`] turns a session's public events into the arbiter's binding
//! verdict (kind `6425`), per Statuses — Sanki §Verdict resolution: a
//! termination [`Status`] (the event's `content`) and a result distribution
//! ([`Outcome3`], the `result` tags). It composes every layer below it.
//!
//! # Verdict resolution
//!
//! Under the forgiving-premove model the verdict is **entirely play-derived** —
//! there is no separate equivocation sanction. The natural-state replay
//! ([`crate::natural_state`]) selects and applies the canonical Ply of each slot
//! and yields one of two conclusions:
//!
//! - a **terminal** verdict reached during replay — a rule-system ending
//!   (checkmate, …) or a played-Ply timeout — which is the verdict directly; or
//! - a still-**ongoing** end position, on which the invocation is resolved at the
//!   cutoff, in order: draw acceptance (`agreement`, [`crate::implicit`]);
//!   abandonment timeout (`timeout`: the on-move player's clock, ticked from the
//!   chain's last attestation — or t₀ for an empty chain — to the cutoff, has
//!   expired); otherwise **residual resignation** (`resignation`, decisive
//!   against the invoker, whatever the turn).
//!
//! An illegal candidate — premove or live — is never a cause: it is skipped during
//! selection (never a loss), so there is no `illegalmove` termination. Because
//! resignation is the residual interpretation, a conforming, canonically attested
//! Request
//! from a session player **always yields a verdict**. [`adjudicate`] returns
//! `None` only when the Request is not yet canonically attested (the cutoff is
//! undefined) or its signer is not a session player (a non-conforming Request,
//! kind `6424` §Semantic constraints).
//!
//! Selecting **which** Request to rule on — several may coexist, and the
//! choice fixes the cutoff, hence the verdict — is the caller's concern:
//! Sashité's arbiter rules on the earliest canonically attested conforming
//! Request not yet adjudicated (Statuses — Sanki §Which Request rules).

use crate::event::{AdjudicationRequest, Attestation, Ply};
use crate::implicit::draw_acceptance;
use crate::natural_state::{natural_state, Conclusion, NaturalState};
use crate::session::SessionParams;
use sashite_sanki_engine::clock::tick;
use sashite_sanki_engine::domain::outcome::Verdict;
use sashite_sanki_engine::domain::side::Side;
use sashite_sanki_engine::domain::status::{Outcome3, Status};
use sashite_sanki_engine::domain::time::Duration;

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
    // cutoff, no ruling. The replay has already selected and applied the chain.
    let natural = natural_state(params, plies, attestations, request)?;

    let verdict = resolve_play(params, &natural, request, invoker);
    Adjudication::from_verdict(verdict)
}

/// The verdict the play produces: the natural state's terminal verdict if the
/// replay reached one, otherwise the invocation resolved at the cutoff on the
/// ongoing end position — in order: draw acceptance, abandonment timeout,
/// residual resignation.
fn resolve_play(
    params: &SessionParams,
    natural: &NaturalState<'_>,
    request: &AdjudicationRequest,
    invoker: Side,
) -> Verdict {
    let state = match &natural.conclusion {
        // The replay terminated (a rule-system ending or a played-Ply timeout):
        // that is the verdict.
        Conclusion::Terminal(verdict, _at) => return *verdict,
        // Still ongoing: resolve the invocation at the cutoff.
        Conclusion::Ongoing(state) => state,
    };

    // 2a. Draw acceptance: a standing offer accepted by the offeree.
    if let Some(verdict) = draw_acceptance(params, natural, request) {
        return verdict;
    }

    // 2b. Abandonment timeout: the player on move let their clock run out
    // before the cutoff (whether or not they are the invoker).
    let on_move = state.position().active_side();
    let elapsed = natural
        .cutoff
        .duration_since(state.last_attestation())
        .unwrap_or(Duration::ZERO);
    if tick(params.time_control(), state.clocks().get(on_move), elapsed).is_flagged() {
        return Verdict::decisive(Status::Timeout, on_move);
    }

    // 2c. Residual resignation: the invocation matches no other cause, so the
    // invoker abandons — whatever the turn.
    Verdict::decisive(Status::Resignation, invoker)
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
            ts(0),
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
            ts(0),
        )
    }

    fn att(id: u8, attests: u8, at: i64) -> Attestation {
        Attestation::new(eid(id), pk(TIMESTAMPER), eid(attests), ts(at))
    }

    fn request(signer: u8) -> AdjudicationRequest {
        AdjudicationRequest::new(eid(REQUEST), pk(signer), eid(SESSION), pk(2), ts(0))
    }

    fn params(feen: &str, tc_secs: u64, anchor: i64) -> SessionParams {
        let period = Period::new(Duration::from_secs(tc_secs), None, None).expect("period");
        SessionParams::new(
            eid(SESSION),
            pk(2),
            Some(pk(TIMESTAMPER)),
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
    fn illegal_move_in_the_chain_is_skipped_not_a_loss() {
        // No piece on a1: the first player's only Ply (a1-a4 @100) is illegal. Under
        // the two-window forgiving rule it is skipped (no `illegalmove`), leaving the
        // chain empty and the first player still on move. The second player invokes
        // within time (cutoff 400 ≤ 600): a residual resignation against the invoker —
        // the illegal move is NOT a loss for the first player.
        let plies = [ply(1, FIRST, 1, "[\"a1\",\"a4\",null]")];
        let atts = [att(101, 1, 100), att(171, REQUEST, 400)];
        let p = params("4k^3/8/8/8/8/8/8/4K^3 / W/w", 600, 0);
        let adj = adjudicate(&p, &plies, &atts, &request(SECOND)).expect("verdict");
        assert_eq!(adj.status(), Status::Resignation);
        assert_eq!(adj.result(), Outcome3::FirstWins);
    }

    #[test]
    fn differing_contents_are_not_an_equivocation_loss() {
        // The first player publishes two differing step-1 contents (a4 @100, a5
        // @200). Under the forgiving rule there is no equivocation sanction: the
        // earliest qualifying candidate (a4) simply fills the slot and the later
        // divergent a5 is ignored. The second player's invocation at 400 (within
        // time) is then a residual resignation — not a loss for the first player.
        let plies = [
            ply(1, FIRST, 1, "[\"a1\",\"a4\",null]"),
            ply(2, FIRST, 1, "[\"a1\",\"a5\",null]"),
        ];
        let atts = [att(101, 1, 100), att(102, 2, 200), att(171, REQUEST, 400)];
        let p = params("4k^3/8/8/8/8/8/8/R3K^3 / W/w", 600, 0);
        let adj = adjudicate(&p, &plies, &atts, &request(SECOND)).expect("verdict");
        assert_eq!(adj.status(), Status::Resignation);
        assert_eq!(adj.result(), Outcome3::FirstWins);
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
