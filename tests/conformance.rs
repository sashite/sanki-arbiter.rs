//! Shared conformance vectors — selection (ADR-0002 forgiving premoves).
//!
//! Drives `tests/conformance/selection.json` (a vendored copy of the shared corpus
//! at `web-specs.md/nostr/conformance`) through the arbiter's pure selection
//! primitive [`select_candidate`]. Each candidate carries its own `legal` (a given,
//! established by category A — legality — in the engine crate), so these vectors
//! pin only the *selection algorithm*: anteriority, the first-legal choice, and the
//! `K = 1` anterior cap. The TypeScript client runs the same JSON, so the arbiter
//! and the client cannot drift on which Ply is canonical.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use std::path::PathBuf;

use sashite_sanki_arbiter::selection::{select_candidate, Candidate, Selection};
use sashite_sanki_engine::domain::time::Timestamp;

#[derive(serde::Deserialize)]
struct Corpus {
    vectors: Vec<SelectionVector>,
}

#[derive(serde::Deserialize)]
struct SelectionVector {
    id: String,
    anchor: i64,
    candidates: Vec<CandidateVector>,
    expected: Expected,
}

#[derive(serde::Deserialize)]
struct CandidateVector {
    id: String,
    #[serde(rename = "createdAt")]
    created_at: i64,
    legal: bool,
}

#[derive(serde::Deserialize)]
struct Expected {
    result: String,
    selected: Option<String>,
}

/// The vendored selection corpus.
fn corpus_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/conformance/selection.json")
}

/// The `(result, selected)` pair a [`Selection`] maps to, in the corpus' encoding.
fn outcome(selection: &Selection<'_, String>) -> (&'static str, Option<String>) {
    match selection {
        Selection::Applied(candidate) => ("applied", Some(candidate.id.clone())),
        Selection::IllegalMove(candidate) => ("illegalmove", Some(candidate.id.clone())),
        Selection::Unfilled => ("unfilled", None),
    }
}

#[test]
fn selection_conformance() {
    let path = corpus_path();
    let Ok(contents) = std::fs::read_to_string(&path) else {
        eprintln!(
            "conformance corpus absent ({}) — test skipped.",
            path.display()
        );
        return;
    };
    let corpus: Corpus =
        serde_json::from_str(&contents).expect("conformance/selection.json: invalid JSON");
    assert!(!corpus.vectors.is_empty(), "the corpus has no vectors");

    for vector in &corpus.vectors {
        let candidates: Vec<Candidate<String>> = vector
            .candidates
            .iter()
            .map(|candidate| Candidate {
                id: candidate.id.clone(),
                created_at: Timestamp::from_unix(candidate.created_at),
                legal: candidate.legal,
            })
            .collect();

        let anchor = Timestamp::from_unix(vector.anchor);
        let (result, selected) = outcome(&select_candidate(anchor, &candidates));

        assert_eq!(
            result,
            vector.expected.result.as_str(),
            "vector {}: result mismatch",
            vector.id
        );
        assert_eq!(
            selected, vector.expected.selected,
            "vector {}: selected candidate mismatch",
            vector.id
        );
    }
}
