# Changelog

All notable changes to this crate are documented in this file. The format is
based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this
crate adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.4.0] — 2026-07-06

Revises the forgiving-premove model to the **two-window** selection: a
slot's premoves and live moves are ranked separately around the predecessor's
timing — the *latest* legal premove binds, else the *earliest* legal live move —
and an illegal candidate (premove or live) is always skipped, so the `illegalmove`
termination is gone.

### Changed — breaking

- **`selection` module — two-window rule.** `select_candidate` now takes the
  slot's `boundary` and a per-window `cap`: `select_candidate(boundary,
  candidates, cap) -> Applied | Unfilled`. A candidate timed **before** the
  boundary is *anterior* (a premove); one **at or after** it is *informed* (a live
  move). Among the `cap` most-recent anterior candidates the **latest legal**
  wins; failing that, among the `cap` earliest informed candidates the **earliest
  legal** wins. The `Selection::IllegalMove` variant is removed (leaving
  `Applied | Unfilled`), and the `ANTERIOR_CAP = 1` constant becomes the
  per-window `CANDIDATE_CAP = 8`.
- **No `illegalmove` termination.** An illegal candidate — premove or live — is
  always skipped, never a loss. `natural_state` no longer produces an
  `illegalmove` verdict; `Conclusion::Terminal` now carries only a rule-system
  ending or a played-Ply timeout, and a slot with no legal candidate in either
  window leaves the chain ongoing. `verdict` drops the informed-illegal cause.

### Removed — breaking

- **`Selection::IllegalMove`** and the **`ANTERIOR_CAP`** constant — superseded by
  the two-window `Selection` (`Applied | Unfilled`) and `CANDIDATE_CAP`.

### Changed

- **Conformance corpus (v3).** The vendored vectors and `tests/conformance.rs`
  track the shared set: `selection.json` gains `boundary` and a per-window `cap`
  (17 vectors), `scenarios.json` uses `timedAt` and adds the re-premove and
  premove-over-live cases (8 vectors) — kept bit-for-bit with the TypeScript
  client.

### Unchanged

- The `adjudicate` entry point and `Adjudication` are source-compatible (same
  signatures and results for legal play).
- Race resolution (`canonical_attestation`, `canonical_ply`) and its tiebreaks.
- The post-chain resolution order (agreement → timeout → resignation) and the
  rule-system / timeout terminations.
- `Status::IllegalMove` remains the engine's internal legality signal (consumed by
  `natural_state::is_legal`); the arbiter simply never emits it as a verdict.

## [0.3.0] — 2026-06-27

Adopts the **forgiving-premove** model: a slot's candidate Plies are
resolved by legality and anteriority — an illegal *blind* premove is forgiven
(skipped), not sanctioned — and the equivocation sanction is removed entirely.

### Added

- **`selection` module** — the pure `select_candidate(anchor, candidates) ->
  Applied | IllegalMove | Unfilled`, generic over the candidate id, implementing
  the selection rule with the normative `K = 1` anterior cap (one premove per
  slot, no re-pre-play). Mirrors the TypeScript client's `selectCandidate`.
- **Selection conformance test** (`tests/conformance.rs`) driving the shared
  `selection.json` vectors (vendored at `tests/conformance/`) through
  `select_candidate`, pinning bit-for-bit parity with the TypeScript client.
  Adds `serde` / `serde_json` as dev-dependencies.

### Changed — breaking

- **Forgiving natural-state replay.** `natural_state` now selects each slot's
  canonical Ply by the forgiving rule and applies it through the engine in a
  single pass (legality is judged on the replayed board). `NaturalState` gains a
  `conclusion: Conclusion` field — `Conclusion::Terminal(verdict, at)` for an
  in-replay ending (informed illegal move, rule-system ending, or played-Ply
  timeout) or `Conclusion::Ongoing(Box<SessionState>)` for the post-chain
  resolution. The chain no longer includes a terminating *informed-illegal* Ply.
- **Play-derived verdict only.** `verdict` drops the equivocation candidate
  family and the separate second replay; the verdict is the natural state's
  terminal conclusion, else the invocation resolved at the cutoff (draw
  acceptance → abandonment timeout → residual resignation).

### Removed — breaking

- **`commitment` module** — the single-content / equivocation / mutual-
  equivocation sanction. Differing contents for a slot are no longer a violation
  but ordinary candidates resolved by `selection`; a misfired blind premove is
  forgiven rather than ruled `illegalmove`.

### Unchanged

- The `adjudicate` entry point and `Adjudication` are source-compatible (same
  signatures and results for legal play).
- Race resolution (`canonical_attestation`, `canonical_ply`) and its tiebreaks.
- The post-chain resolution order (agreement → timeout → resignation) and
  rule-system / timeout terminations.

## [0.2.1] — 2026-06-13

### Changed

- Depend on `sashite-sanki-engine = "0.2"` (was `"0.1"`), tracking the engine's
  rename of `SessionState::step` to `half_move`. No change to this crate's own
  public API or behaviour; only the internal kernel-state accessor call and a
  test assertion are updated.

## [0.2.0] — 2026-06-13

Aligns the crate with the revised Sanki adjudication specifications
(per-player step semantics, residual resignation, equivocation-only
violations, ordered post-chain resolution).

### Changed — breaking

- **Per-player step semantics.** A Ply's `step` is now the signer's own move
  ordinal (kind `6423` §Step semantics and play order); the slot is
  `(session, signer, step)` and the natural-state chain consumes slots in the
  interleaved play order — within each step value, side `first` before side
  `second`. `SessionParams::expected_side` / `expected_signer` are replaced by
  `side_at(half_move)`, `step_at(half_move)`, and `player_at(half_move)`;
  `NaturalState::next_step` is renamed `next_half_move`.
- **Residual, turn-independent resignation.** A conforming, canonically
  attested Request from a session player now always yields a verdict: the
  post-chain resolution is ordered draw acceptance (`agreement`) → abandonment
  timeout (`timeout`, the on-move player's clock) → residual `resignation`
  (decisive against the invoker, whatever the turn). There is no "premature"
  invocation anymore; `adjudicate` returns `None` only for an unattested
  Request or a non-player signer. `implicit::implicit_termination` is replaced
  by `implicit::draw_acceptance`.
- **Equivocation-only violations.** The step-ownership violation is
  structurally inexpressible under per-player steps and is removed.
  `commitment::commitment_violation` / `Violation` / `ViolationKind` become
  `commitment::equivocation` / `Equivocation` (single-content rule only,
  applicable to every slot including pending ones, anchored at the
  second-attested differing Ply).

### Unchanged

- Race resolution (canonical attestation, canonical ply) and its tiebreaks.
- Chain-replay terminations: an illegal or unparseable evaluated Ply rules
  `illegalmove`; rule-system endings and played-Ply timeouts carry their Ply's
  attestation as anchor.
- The candidate ranking by attestation time, an equivocation winning an exact
  tie.

## [0.1.0] — 2026-06-08

Initial release: abstract event model (`Ply`, `Attestation`,
`AdjudicationRequest`), race resolution, natural state, commitment violations,
implicit terminations, and the `adjudicate` orchestration over
`sashite-sanki-engine`.
