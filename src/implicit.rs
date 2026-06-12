//! Implicit draw by agreement (Statuses — Sanki §Implicit draw by agreement).
//!
//! A player offers a draw by attaching the `draw` flag to their Ply; the
//! opponent accepts by invoking the arbiter while that offer is the last
//! half-move of the chain. Playing the next half-move instead implicitly
//! declines (the chain extends past the offer, and the condition below fails).
//!
//! - **Implicit draw by agreement** — the last Ply in the consecutive chain
//!   carries the `draw` flag (an offer by its signer), and the triggering
//!   Adjudication Request is signed by that signer's **opponent**: the
//!   invocation accepts the offer. Result: `50/50`.
//!
//! The other implicit convention — **residual resignation** — needs no
//! detection of its own: per Statuses — Sanki §Verdict resolution, the
//! post-chain resolution is ordered `agreement` → abandonment `timeout` →
//! `resignation`, and resignation is simply the fall-through, decisive against
//! the invoker, whatever the turn. That ordering lives in [`crate::verdict`];
//! this module only detects the acceptance.

use crate::event::AdjudicationRequest;
use crate::natural_state::NaturalState;
use crate::session::SessionParams;
use sashite_sanki_engine::domain::outcome::Verdict;
use sashite_sanki_engine::domain::status::Status;

/// The `agreement` verdict, if the invocation accepts a standing draw offer.
///
/// Returns `Some(agreement)` when the last chain Ply offers a draw and the
/// Request is signed by its signer's opponent; `None` otherwise (no offer, an
/// offer extended past by play, an offerer invoking on their own offer, or a
/// non-player invoker).
#[must_use]
pub fn draw_acceptance(
    params: &SessionParams,
    natural: &NaturalState<'_>,
    request: &AdjudicationRequest,
) -> Option<Verdict> {
    let invoker = params.side_of(request.signer)?;
    let last = natural.chain.last()?;
    if !last.ply.draw {
        return None;
    }
    let offerer = params.side_of(last.ply.signer)?;
    (invoker == offerer.flip()).then(|| Verdict::drawn(Status::Agreement))
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use super::draw_acceptance;
    use crate::event::{AdjudicationRequest, EventId, Ply, PublicKey};
    use crate::natural_state::NaturalState;
    use crate::race_resolution::CanonicalPly;
    use crate::session::SessionParams;
    use sashite_sanki_engine::domain::outcome::Verdict;
    use sashite_sanki_engine::domain::status::Status;
    use sashite_sanki_engine::domain::time::{Duration, Timestamp};
    use sashite_sanki_engine::domain::time_control::{Period, TimeControl};
    use sashite_sanki_engine::position::Position;

    const FIRST: u8 = 10;
    const SECOND: u8 = 20;
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

    fn ply(id: u8, signer: u8, step: u32, draw: bool) -> Ply {
        Ply::new(eid(id), pk(signer), eid(SESSION), step, draw, String::new())
    }

    fn params() -> SessionParams {
        let period = Period::new(Duration::from_secs(600), None, None).expect("valid period");
        SessionParams::new(
            eid(SESSION),
            pk(2),
            pk(99),
            pk(FIRST),
            pk(SECOND),
            TimeControl::new(period, Vec::new()),
            Position::parse("4k^3/8/8/8/8/8/8/4K^3 / W/w").expect("valid FEEN"),
            ts(0),
        )
    }

    fn request(signer: u8) -> AdjudicationRequest {
        AdjudicationRequest::new(eid(170), pk(signer), eid(SESSION), pk(2))
    }

    #[test]
    fn agreement_when_opponent_accepts_the_draw() {
        // Last half-move (first, step 1) marked `draw`; `second` (the opponent)
        // invokes: acceptance.
        let p1 = ply(1, FIRST, 1, true);
        let natural = NaturalState {
            chain: vec![CanonicalPly {
                ply: &p1,
                at: ts(100),
            }],
            cutoff: ts(1000),
        };
        let verdict = draw_acceptance(&params(), &natural, &request(SECOND));
        assert_eq!(verdict, Some(Verdict::drawn(Status::Agreement)));
    }

    #[test]
    fn no_acceptance_without_a_draw_flag() {
        let p1 = ply(1, FIRST, 1, false);
        let natural = NaturalState {
            chain: vec![CanonicalPly {
                ply: &p1,
                at: ts(100),
            }],
            cutoff: ts(1000),
        };
        assert!(draw_acceptance(&params(), &natural, &request(SECOND)).is_none());
    }

    #[test]
    fn offer_extended_past_by_play_is_declined() {
        // `first` offers at (first, 1); `second` answers at (second, 1) instead of
        // invoking: the offer is no longer the last half-move.
        let p1 = ply(1, FIRST, 1, true);
        let p2 = ply(2, SECOND, 1, false);
        let natural = NaturalState {
            chain: vec![
                CanonicalPly {
                    ply: &p1,
                    at: ts(100),
                },
                CanonicalPly {
                    ply: &p2,
                    at: ts(200),
                },
            ],
            cutoff: ts(1000),
        };
        assert!(draw_acceptance(&params(), &natural, &request(FIRST)).is_none());
    }

    #[test]
    fn offerer_cannot_accept_their_own_offer() {
        // `first` offers the draw then invokes: not an acceptance (the residual
        // resolution in `verdict` decides what the invocation means).
        let p1 = ply(1, FIRST, 1, true);
        let natural = NaturalState {
            chain: vec![CanonicalPly {
                ply: &p1,
                at: ts(100),
            }],
            cutoff: ts(1000),
        };
        assert!(draw_acceptance(&params(), &natural, &request(FIRST)).is_none());
    }

    #[test]
    fn empty_chain_has_no_offer() {
        let natural = NaturalState {
            chain: Vec::new(),
            cutoff: ts(1000),
        };
        assert!(draw_acceptance(&params(), &natural, &request(SECOND)).is_none());
    }

    #[test]
    fn non_player_invoker_does_not_accept() {
        let p1 = ply(1, FIRST, 1, true);
        let natural = NaturalState {
            chain: vec![CanonicalPly {
                ply: &p1,
                at: ts(100),
            }],
            cutoff: ts(1000),
        };
        assert!(draw_acceptance(&params(), &natural, &request(77)).is_none());
    }
}
