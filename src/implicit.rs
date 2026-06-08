//! Implicit termination conventions (Statuses — Sanki §Implicit termination).
//!
//! Two statuses do not arise from position evaluation but from how a player
//! invokes the arbiter on the natural state:
//!
//! - **Implicit draw by agreement** — the last Ply in the consecutive chain
//!   carries the `draw` flag (an offer by its signer), and the triggering
//!   Adjudication Request is signed by that signer's **opponent**: the invocation
//!   accepts the offer. Result: `50/50`.
//! - **Implicit resignation** — it is the invoker's turn at the step following
//!   the chain (the chain stopped because they have not played it), so invoking
//!   instead of moving is read as resigning. Result: decisive against the
//!   invoker.
//!
//! The resignation conditions 2 and 3 of the spec are subsumed by the natural
//! state: the chain stops at step N+1 precisely because no canonical Ply by the
//! expected signer exists there within the cutoff; when N+1 is the invoker's
//! turn, that absence *is* condition 3. Hence only the natural state and the
//! Request are needed.
//!
//! **Agreement takes precedence over resignation.** When the last Ply offers a
//! draw and the opponent invokes, both positional patterns coincide (it is the
//! opponent's turn), but the draw offer makes the invocation an acceptance, not
//! an abandonment.
//!
//! This function assumes the game is otherwise ongoing: the orchestration
//! evaluates commitment violations and rule-system terminations first, and only
//! falls back to the implicit conventions when neither applies.

use crate::event::AdjudicationRequest;
use crate::natural_state::NaturalState;
use crate::session::SessionParams;
use sashite_sanki_engine::domain::outcome::Verdict;
use sashite_sanki_engine::domain::status::Status;

/// The implicit verdict for the session, if the invocation matches a convention.
///
/// Returns `agreement` (draw) when the last chain Ply offers a draw and the
/// Request is signed by its opponent; otherwise `resignation` (decisive against
/// the invoker) when the step following the chain is the invoker's turn; `None`
/// when neither convention applies.
#[must_use]
pub fn implicit_termination(
    params: &SessionParams,
    natural: &NaturalState<'_>,
    request: &AdjudicationRequest,
) -> Option<Verdict> {
    let invoker = params.side_of(request.signer)?;

    // Implicit draw by agreement: the last Ply offers a draw and the invoker is
    // its opponent (acceptance). Checked first — it overrides resignation when
    // both positional patterns coincide.
    if let Some(last) = natural.chain.last() {
        if last.ply.draw {
            if let Some(offerer) = params.side_of(last.ply.signer) {
                if invoker == offerer.flip() {
                    return Some(Verdict::drawn(Status::Agreement));
                }
            }
        }
    }

    // Implicit resignation: the step after the chain is the invoker's turn, and
    // they invoked instead of playing it.
    if params.expected_side(natural.next_step()) == invoker {
        return Some(Verdict::decisive(Status::Resignation, invoker));
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

    use super::implicit_termination;
    use crate::event::{AdjudicationRequest, EventId, Ply, PublicKey};
    use crate::natural_state::NaturalState;
    use crate::race_resolution::CanonicalPly;
    use crate::session::SessionParams;
    use sashite_sanki_engine::domain::outcome::Verdict;
    use sashite_sanki_engine::domain::side::Side;
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
    fn resignation_on_empty_chain() {
        // No move played: it is `first`'s turn (step 1); first invokes.
        let natural = NaturalState {
            chain: Vec::new(),
            cutoff: ts(1000),
        };
        let verdict = implicit_termination(&params(), &natural, &request(FIRST));
        assert_eq!(
            verdict,
            Some(Verdict::decisive(Status::Resignation, Side::First))
        );
    }

    #[test]
    fn resignation_after_a_few_moves() {
        // Chain up to step 2; step 3 is `first`'s turn; first invokes.
        let p1 = ply(1, FIRST, 1, false);
        let p2 = ply(2, SECOND, 2, false);
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
        let verdict = implicit_termination(&params(), &natural, &request(FIRST));
        assert_eq!(
            verdict,
            Some(Verdict::decisive(Status::Resignation, Side::First))
        );
    }

    #[test]
    fn no_resignation_off_the_invoker_turn() {
        // Chain at step 1; step 2 is `second`'s turn, but `first` invokes.
        let p1 = ply(1, FIRST, 1, false);
        let natural = NaturalState {
            chain: vec![CanonicalPly {
                ply: &p1,
                at: ts(100),
            }],
            cutoff: ts(1000),
        };
        assert!(implicit_termination(&params(), &natural, &request(FIRST)).is_none());
    }

    #[test]
    fn agreement_when_opponent_accepts_the_draw() {
        // Last move (first) marked `draw`; `second` (the opponent) invokes.
        let p1 = ply(1, FIRST, 1, true);
        let natural = NaturalState {
            chain: vec![CanonicalPly {
                ply: &p1,
                at: ts(100),
            }],
            cutoff: ts(1000),
        };
        let verdict = implicit_termination(&params(), &natural, &request(SECOND));
        assert_eq!(verdict, Some(Verdict::drawn(Status::Agreement)));
    }

    #[test]
    fn agreement_outranks_resignation() {
        // Same positional configuration, but without the `draw` flag: it is a
        // resignation by `second` (their turn at step 2) — and with the flag, it is
        // an agreement. We check both.
        let without = ply(1, FIRST, 1, false);
        let natural_without = NaturalState {
            chain: vec![CanonicalPly {
                ply: &without,
                at: ts(100),
            }],
            cutoff: ts(1000),
        };
        assert_eq!(
            implicit_termination(&params(), &natural_without, &request(SECOND)),
            Some(Verdict::decisive(Status::Resignation, Side::Second))
        );

        let with = ply(1, FIRST, 1, true);
        let natural_with = NaturalState {
            chain: vec![CanonicalPly {
                ply: &with,
                at: ts(100),
            }],
            cutoff: ts(1000),
        };
        assert_eq!(
            implicit_termination(&params(), &natural_with, &request(SECOND)),
            Some(Verdict::drawn(Status::Agreement))
        );
    }

    #[test]
    fn draw_offer_by_the_offerer_itself_does_not_terminate() {
        // `first` offers the draw then invokes itself: neither agreement (it is not
        // the opponent) nor resignation (step 2 is `second`'s turn).
        let p1 = ply(1, FIRST, 1, true);
        let natural = NaturalState {
            chain: vec![CanonicalPly {
                ply: &p1,
                at: ts(100),
            }],
            cutoff: ts(1000),
        };
        assert!(implicit_termination(&params(), &natural, &request(FIRST)).is_none());
    }

    #[test]
    fn non_player_invoker_does_not_terminate() {
        let natural = NaturalState {
            chain: Vec::new(),
            cutoff: ts(1000),
        };
        assert!(implicit_termination(&params(), &natural, &request(77)).is_none());
    }
}
