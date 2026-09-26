//! The wording of operation synthesis by archetype (§20 of the System
//! One orchestration revision).
//!
//! Code enumerates what a program could be built from — which records
//! an operation's input identifies, which lifecycle transitions exist,
//! which errors its result declares — and fills a typed template. A
//! model is asked only which template matches what the operation is
//! described to do, and which of the enumerated pieces it acts on. It
//! never writes a step.
//!
//! The state is:
//!
//! ```text
//! prompt      the run's prompt, verbatim
//! operation   id, description, the fields its input carries
//! records     each record the input identifies: its fields
//! transitions each lifecycle transition of such a record: from, to
//! errors      each error the operation's result declares
//! ```

use serde_json::{Value as Json, json};

use crate::system_one::{Question, QuestionSpec};

pub const ARCHETYPE: QuestionSpec = QuestionSpec {
    id: "synthesis.archetype",
    version: 1,
    decides: "which program template matches what an operation is described to do",
};

pub const RECORD: QuestionSpec = QuestionSpec {
    id: "synthesis.record",
    version: 1,
    decides: "which enumerated record an operation acts on",
};

pub const CHANGES: QuestionSpec = QuestionSpec {
    id: "synthesis.changes",
    version: 1,
    decides: "whether an operation changes one field of the record it acts on",
};

pub const TRANSITION: QuestionSpec = QuestionSpec {
    id: "synthesis.transition",
    version: 1,
    decides: "which enumerated lifecycle transition an operation applies",
};

pub const REFUSAL: QuestionSpec = QuestionSpec {
    id: "synthesis.refusal",
    version: 1,
    decides: "which declared error an operation returns when its record is in the wrong state",
};

/// The option every choice carries when its right answer may be missing
/// from what code enumerated.
pub const NONE_OF_THESE: &str = "none_of_these";

/// The archetypes, as the model sees them: what each does, in words.
pub const KEYED_UPDATE: &str = "keyed_update";
pub const KEYED_INSERT: &str = "keyed_insert";
pub const TRANSITION_APPLIED: &str = "transition";

pub fn archetype(offered: &[&str]) -> Question {
    let describe = |archetype: &str| match archetype {
        KEYED_UPDATE => {
            "Changes one or more fields of one existing record, which the input \
                         identifies — and does nothing else: no new record, no lifecycle \
                         change, no message, no call to another service."
        }
        KEYED_INSERT => {
            "Creates one new record from what the input carries — and does \
                         nothing else: no change to an existing record, no message, no call \
                         to another service."
        }
        TRANSITION_APPLIED => {
            "Moves one existing record, which the input identifies, from \
                               one lifecycle state to another (for example from pending to \
                               paid) — and does nothing else: no other field change, no new \
                               record, no call to another service."
        }
        _ => "",
    };

    Question::choice(
        json!({
            "question": "Which description below matches everything `operation.description` \
                         says the operation does?",
            "focus": "Match the whole of the work described. If the operation does anything \
                      the description leaves out — creates a second record, notifies someone, \
                      calls another service, changes both a field and a lifecycle state — none \
                      matches.",
        }),
        offered
            .iter()
            .map(|archetype| {
                (
                    archetype.to_string(),
                    Json::String(describe(archetype).into()),
                )
            })
            .chain([(
                NONE_OF_THESE.to_string(),
                Json::String(
                    "None of the descriptions matches everything the operation does.".into(),
                ),
            )]),
    )
}

/// Which record, when more than one could be meant. Asked under the
/// premise that the operation acts on exactly one of them.
pub fn record(records: &[String]) -> Question {
    Question::choice(
        json!({
            "question": "Suppose `operation` acts on exactly one record in `records`. Which \
                         one?",
            "focus": "Judge from `operation.description` and the fields each record carries.",
        }),
        records
            .iter()
            .map(|record| {
                (
                    record.clone(),
                    Json::String(format!("The record `{record}` in `records`.")),
                )
            })
            .chain([(
                NONE_OF_THESE.to_string(),
                Json::String("It acts on none of the records listed.".into()),
            )]),
    )
}

/// Whether one field is among those the operation changes. Asked under
/// the premise that it acts on that record; a set, so one question per
/// field.
pub fn changes(record: &str, field: &str) -> Question {
    Question::noul_with_criteria(
        json!({
            "question": format!(
                "Suppose `operation` acts on the record `{record}` in `records`. Does it change \
                 that record's `{field}`?"
            ),
            "focus": "Judge only from `operation.description`. A field the operation merely \
                      uses to find the record, or only reads, is not changed.",
        }),
        json!({
            "what": "The description says or clearly implies that this field gets a new value.",
        }),
        json!({
            "what": "The description does not say that this field changes.",
        }),
    )
}

pub fn transition(transitions: &[(String, String)]) -> Question {
    Question::choice(
        json!({
            "question": "Which lifecycle transition in `transitions` does `operation` apply?",
            "focus": "Match the state the record must be in before, and the state it is in \
                      after, as `operation.description` states them.",
        }),
        transitions
            .iter()
            .map(|(option, description)| (option.clone(), Json::String(description.clone())))
            .chain([(
                NONE_OF_THESE.to_string(),
                Json::String("It applies none of the transitions listed.".into()),
            )]),
    )
}

/// The error returned when the record is not in a state the transition
/// applies from. Asked only for a request, and only over its declared
/// errors.
pub fn refusal(errors: &[(String, String)]) -> Question {
    Question::choice(
        json!({
            "question": "When the record `operation` acts on is not in a state its transition \
                         applies from, which error in `errors` does the operation return?",
            "focus": "Judge from the error names and descriptions. An error about a missing \
                      record, bad input or authorization is not about the record's state.",
        }),
        errors
            .iter()
            .map(|(option, description)| (option.clone(), Json::String(description.clone())))
            .chain([(
                NONE_OF_THESE.to_string(),
                Json::String("None of the errors listed is about the record's state.".into()),
            )]),
    )
}
