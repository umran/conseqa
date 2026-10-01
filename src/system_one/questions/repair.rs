//! The wording of requirement repair (§18.5 of the System One
//! orchestration revision).
//!
//! Repair asks a model almost nothing. Code synthesizes the candidate
//! repairs and the analyzer proves them; what is left is a preference
//! among specifications that are *all* correct, and the default
//! preference — least invasive first — is code's as well. A model may
//! reorder it only on a fact the prompt states.
//!
//! The state is:
//!
//! ```text
//! prompt     the run's prompt, verbatim
//! operation  id, description
//! work[]     what each repaired transaction reads and changes, in words
//! ```

use serde_json::json;

use crate::system_one::{Question, QuestionSpec};

pub const CONTENTION: QuestionSpec = QuestionSpec {
    id: "repair.contention",
    version: 1,
    decides: "whether exclusive locks are preferred to serializable isolation, both being proven",
};

/// Does the prompt say many executions hit the same record at once?
///
/// Under serializable isolation a conflicting execution is aborted and
/// must be retried; under an exclusive lock it waits its turn. Which is
/// better is a fact about load, which only the prompt can state — and
/// when it states nothing, nothing is reordered.
pub fn contention() -> Question {
    Question::noul_with_criteria(
        json!({
            "question": "Does `prompt` say that many executions of the work described in `work` \
                         are expected to act on the same record at the same moment?",
            "focus": "Judge only what `prompt` states about simultaneous demand for one record: \
                      a flash sale on one item, a hot account, a burst of requests for the same \
                      row. Do not infer it from the system merely being large or busy.",
        }),
        json!({
            "what": "The prompt states or clearly implies heavy simultaneous demand for the same \
                     record.",
            "examples": [
                "thousands of buyers compete for the same item at the moment a sale opens",
                "a few hot accounts receive most of the traffic",
            ],
        }),
        json!({
            "what": "The prompt states nothing about many executions acting on one record at \
                     once.",
            "not_for": "A prompt that only says the system has many users, or must scale.",
        }),
    )
}

pub const GAP_FREE: QuestionSpec = QuestionSpec {
    id: "repair.gap_free",
    version: 1,
    decides: "whether a successor cursor is preferred to a monotonic one, both being proven",
};

/// Must every position be applied, in order, with none skipped?
///
/// A `successor` cursor admits only the next position, so a gap rejects
/// until the missing one arrives; a `monotonic_after` cursor admits any
/// later position and lets gaps through. Both order what they admit.
/// Which the system needs is a fact about the domain — a ledger of
/// sequenced entries needs every one, a high-water mark does not — and
/// when the prompt states nothing, the permissive default stands.
pub fn gap_free() -> Question {
    Question::noul_with_criteria(
        json!({
            "question": "Does `prompt` require that the positions `work` applies — sequence \
                         numbers, offsets, versions — are each applied, in order, with none \
                         skipped?",
            "focus": "Judge only what `prompt` states about every position being applied. A \
                      prompt that only asks for updates to apply in order, or for stale \
                      updates to be ignored, does not require it.",
        }),
        json!({
            "what": "The prompt states that no position may be skipped: every entry, event or \
                     sequence number must be applied, and a missing one must be waited for.",
            "examples": [
                "every ledger entry is applied exactly in sequence, with no gaps",
                "events must be processed one after another without missing any",
            ],
        }),
        json!({
            "what": "The prompt allows a later position to supersede an earlier one, or states \
                     nothing about skipped positions.",
            "not_for": "Last-writer-wins updates, high-water marks, or snapshots.",
        }),
    )
}
