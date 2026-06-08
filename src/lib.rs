//! `sashite-sanki-arbiter` — adjudication logic for the Sanki game suite, built
//! for Sashité.
//!
//! The L2 layer over the `sashite-sanki-engine` crate: it rules on a session from
//! its attested event chain and emits the Adjudication —
//! `adjudicate(session, attested_chain, request) -> Adjudication`.
//!
//! The event model is **abstract** and carries no Nostr dependency: `Ply`,
//! `Attestation`, and `AdjudicationRequest` are plain values the caller has
//! already received, signature-verified, and parsed. Timing is anchored on the
//! timestamper's attestations, never on an event's own declarative `created_at`.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), warn(missing_docs))]

pub mod commitment;
pub mod event;
pub mod implicit;
pub mod natural_state;
pub mod race_resolution;
pub mod session;
pub mod verdict;
