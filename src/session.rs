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
//! timestamper, and computing the expected signer of a step under Sanki's strict
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

    /// The side expected to sign the given 1-based step, under Sanki's strict
    /// alternation: side `first` plays the odd steps, side `second` the even
    /// ones.
    #[inline]
    #[must_use]
    pub const fn expected_side(&self, step: u32) -> Side {
        if step & 1 == 1 {
            Side::First
        } else {
            Side::Second
        }
    }

    /// The player expected to sign the given 1-based step.
    #[inline]
    #[must_use]
    pub const fn expected_signer(&self, step: u32) -> PublicKey {
        self.player(self.expected_side(step))
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
    fn expected_signer_by_step_parity() {
        let p = params();
        // Strict alternation: odd → first, even → second.
        assert_eq!(p.expected_side(1), Side::First);
        assert_eq!(p.expected_side(2), Side::Second);
        assert_eq!(p.expected_side(3), Side::First);
        assert_eq!(p.expected_side(4), Side::Second);
        assert_eq!(p.expected_signer(1), pk(10));
        assert_eq!(p.expected_signer(2), pk(20));
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
