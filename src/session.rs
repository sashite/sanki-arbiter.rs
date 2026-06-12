//! `SessionParams` — the session-constant configuration the arbiter rules on.
//!
//! A session's invariant parameters are spread across several founding events;
//! the application assembles them, after cross-event validation, into one
//! aggregate:
//!
//! - from the **Game Session** (kind `6422`): the two players and their sides,
//!   the per-player variants (carried by the initial position's styles), the
//!   initial position, the arbiter (its signer), and the session's event id;
//! - from the **founding** (kinds `6420`/`6421` or `6418`/`6419`): the time
//!   control and the designated timestamper;
//! - from the **Session Start Attestation** (kind `1041` attesting the Game
//!   Session): t₀, the canonical session start.
//!
//! This module is a pure aggregate plus the lookups the arbiter layers need:
//! mapping a signer to its side, naming the player on a side, recognizing the
//! timestamper, and mapping a **play-order position** (1-based half-move index)
//! to its slot — the side on move and that side's `step` (the signer's own move
//! ordinal, kind `6423` §Step semantics and play order) — under Sanki's strict
//! alternation. The per-player variants are not duplicated here — they are read
//! from the [`Position`] (its style field), via [`SessionParams::initial_state`]
//! and the kernel.

use crate::event::{EventId, PublicKey};
use sashite_sanki_engine::domain::side::Side;
use sashite_sanki_engine::domain::time::Timestamp;
use sashite_sanki_engine::domain::time_control::TimeControl;
use sashite_sanki_engine::kernel::state::SessionState;
use sashite_sanki_engine::position::Position;

/// The invariant parameters of an arbitered session.
#[derive(Debug, Clone)]
pub struct SessionParams {
    session: EventId,
    arbiter: PublicKey,
    timestamper: PublicKey,
    first: PublicKey,
    second: PublicKey,
    time_control: TimeControl,
    initial_position: Position,
    anchor: Timestamp,
}

impl SessionParams {
    /// Assembles the session parameters. `first` and `second` are the players
    /// assigned to the corresponding sides by the Game Session; `anchor` is t₀,
    /// the canonical attestation timing of the Game Session.
    // A faithful constructor for an 8-field aggregate; grouping the fields would
    // only obscure them.
    #[allow(clippy::too_many_arguments)]
    #[inline]
    #[must_use]
    pub const fn new(
        session: EventId,
        arbiter: PublicKey,
        timestamper: PublicKey,
        first: PublicKey,
        second: PublicKey,
        time_control: TimeControl,
        initial_position: Position,
        anchor: Timestamp,
    ) -> Self {
        Self {
            session,
            arbiter,
            timestamper,
            first,
            second,
            time_control,
            initial_position,
            anchor,
        }
    }

    /// The Game Session event id this session is scoped to.
    #[inline]
    #[must_use]
    pub const fn session(&self) -> EventId {
        self.session
    }

    /// The designated arbiter (the Game Session's signer).
    #[inline]
    #[must_use]
    pub const fn arbiter(&self) -> PublicKey {
        self.arbiter
    }

    /// The designated timestamper (whose attestations are authoritative).
    #[inline]
    #[must_use]
    pub const fn timestamper(&self) -> PublicKey {
        self.timestamper
    }

    /// The session's time control.
    #[inline]
    #[must_use]
    pub const fn time_control(&self) -> &TimeControl {
        &self.time_control
    }

    /// The initial position.
    #[inline]
    #[must_use]
    pub const fn initial_position(&self) -> &Position {
        &self.initial_position
    }

    /// t₀, the canonical session start.
    #[inline]
    #[must_use]
    pub const fn anchor(&self) -> Timestamp {
        self.anchor
    }

    /// The player assigned to `side`.
    #[inline]
    #[must_use]
    pub const fn player(&self, side: Side) -> PublicKey {
        match side {
            Side::First => self.first,
            Side::Second => self.second,
        }
    }

    /// The side a pubkey plays, or `None` if it is not one of the two players.
    #[inline]
    #[must_use]
    pub fn side_of(&self, pubkey: PublicKey) -> Option<Side> {
        if pubkey == self.first {
            Some(Side::First)
        } else if pubkey == self.second {
            Some(Side::Second)
        } else {
            None
        }
    }

    /// Whether `pubkey` is one of the two players.
    #[inline]
    #[must_use]
    pub fn is_player(&self, pubkey: PublicKey) -> bool {
        pubkey == self.first || pubkey == self.second
    }

    /// Whether `pubkey` is the designated timestamper.
    #[inline]
    #[must_use]
    pub fn is_timestamper(&self, pubkey: PublicKey) -> bool {
        pubkey == self.timestamper
    }

    /// The side on move at the 1-based position `half_move` of the play order,
    /// under Sanki's strict alternation: within each step value, side `first`
    /// moves before side `second` — so odd positions belong to `first`, even
    /// ones to `second`.
    #[inline]
    #[must_use]
    pub const fn side_at(&self, half_move: u32) -> Side {
        if half_move & 1 == 1 {
            Side::First
        } else {
            Side::Second
        }
    }

    /// The mover's `step` — their own move ordinal (kind `6423` §Step semantics
    /// and play order) — at the 1-based position `half_move` of the play order:
    /// position 1 → step 1 of `first`, position 2 → step 1 of `second`,
    /// position 3 → step 2 of `first`, …
    #[inline]
    #[must_use]
    pub const fn step_at(&self, half_move: u32) -> u32 {
        half_move.div_ceil(2)
    }

    /// The player on move at the 1-based position `half_move` of the play order.
    #[inline]
    #[must_use]
    pub const fn player_at(&self, half_move: u32) -> PublicKey {
        self.player(self.side_at(half_move))
    }

    /// Builds the initial kernel state: clocks started from the time control, the
    /// FEEN history seeded with the initial position, and t₀ as the timing
    /// anchor.
    #[inline]
    #[must_use]
    pub fn initial_state(&self) -> SessionState {
        SessionState::start(
            self.initial_position.clone(),
            self.time_control.clone(),
            self.anchor,
        )
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

    use super::SessionParams;
    use crate::event::{EventId, PublicKey};
    use sashite_sanki_engine::domain::side::Side;
    use sashite_sanki_engine::domain::time::{Duration, Timestamp};
    use sashite_sanki_engine::domain::time_control::{Period, TimeControl};
    use sashite_sanki_engine::position::Position;

    fn pk(byte: u8) -> PublicKey {
        PublicKey::from_bytes([byte; 32])
    }

    fn id(byte: u8) -> EventId {
        EventId::from_bytes([byte; 32])
    }

    fn time_control() -> TimeControl {
        let period = Period::new(Duration::from_secs(600), None, None).expect("valid period");
        TimeControl::new(period, Vec::new())
    }

    const START_FEEN: &str = "4k^3/8/8/8/8/8/8/4K^3 / W/w";

    fn params() -> SessionParams {
        SessionParams::new(
            id(1),
            pk(2),
            pk(3),
            pk(10),
            pk(20),
            time_control(),
            Position::parse(START_FEEN).expect("valid Sanki FEEN"),
            Timestamp::from_unix(1000),
        )
    }

    #[test]
    fn maps_pubkey_to_side() {
        let p = params();
        assert_eq!(p.side_of(pk(10)), Some(Side::First));
        assert_eq!(p.side_of(pk(20)), Some(Side::Second));
        assert_eq!(p.side_of(pk(99)), None); // neither one
    }

    #[test]
    fn maps_side_to_player() {
        let p = params();
        assert_eq!(p.player(Side::First), pk(10));
        assert_eq!(p.player(Side::Second), pk(20));
    }

    #[test]
    fn recognizes_player_and_timestamper() {
        let p = params();
        assert!(p.is_player(pk(10)));
        assert!(p.is_player(pk(20)));
        assert!(!p.is_player(pk(3))); // the timestamper is not a player
        assert!(p.is_timestamper(pk(3)));
        assert!(!p.is_timestamper(pk(10)));
    }

    #[test]
    fn play_order_positions_map_to_slots() {
        let p = params();
        // Strict alternation: (1,first),(1,second),(2,first),(2,second), …
        assert_eq!(p.side_at(1), Side::First);
        assert_eq!(p.side_at(2), Side::Second);
        assert_eq!(p.side_at(3), Side::First);
        assert_eq!(p.side_at(4), Side::Second);
        assert_eq!(p.step_at(1), 1);
        assert_eq!(p.step_at(2), 1);
        assert_eq!(p.step_at(3), 2);
        assert_eq!(p.step_at(4), 2);
        assert_eq!(p.step_at(5), 3);
        assert_eq!(p.player_at(1), pk(10));
        assert_eq!(p.player_at(2), pk(20));
        assert_eq!(p.player_at(3), pk(10));
    }

    #[test]
    fn initial_kernel_state() {
        let p = params();
        let state = p.initial_state();
        assert_eq!(state.step(), 1);
        assert_eq!(state.last_attestation(), Timestamp::from_unix(1000));
        assert_eq!(state.position().to_feen(), START_FEEN);
        assert!(!state.move_limit_reached());
    }

    #[test]
    fn accessors() {
        let p = params();
        assert_eq!(p.session(), id(1));
        assert_eq!(p.arbiter(), pk(2));
        assert_eq!(p.anchor(), Timestamp::from_unix(1000));
        assert_eq!(p.initial_position().to_feen(), START_FEEN);
    }
}
