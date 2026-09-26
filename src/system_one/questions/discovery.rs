//! The wording of requirement discovery (§17 of the System One
//! orchestration revision).
//!
//! Discovery asks what a prompt *requires* of an operation. Code has
//! already enumerated what could be required — which transactions
//! mutate state, which values identify what they touch, which input
//! identity could key a retry — so nothing here asks the model to find
//! a requirement, only whether the prompt states one.
//!
//! Every question names its state by path. The state is:
//!
//! ```text
//! prompt        the run's prompt, verbatim
//! operation     id, description, triggered_by, work[] (one entry per
//!               transaction: what it reads and changes, in words)
//! obligations[] the explicit obligations the decomposer extracted
//!               for this operation: intent, and the quoted source
//! ```

use serde_json::json;

use crate::system_one::{Json, Question, QuestionSpec};

pub const IDEMPOTENCY: QuestionSpec = QuestionSpec {
    id: "discovery.idempotency",
    version: 1,
    decides: "whether an idempotency requirement is proposed for the operation",
};

pub const RESULT_REPLAY: QuestionSpec = QuestionSpec {
    id: "discovery.result_replay",
    version: 1,
    decides: "whether a proposed idempotency requirement also demands a replay-consistent result",
};

pub const RECOVERABILITY: QuestionSpec = QuestionSpec {
    id: "discovery.recoverability",
    version: 1,
    decides: "whether a recoverability requirement is proposed for the operation",
};

pub const GUARANTEED_COMPLETION: QuestionSpec = QuestionSpec {
    id: "discovery.guaranteed_completion",
    version: 1,
    decides: "whether proposed recoverability is `guaranteed` rather than `resumable`",
};

pub const SERIALIZABILITY: QuestionSpec = QuestionSpec {
    id: "discovery.serializability",
    version: 1,
    decides: "whether SerializableBy is proposed on a transaction",
};

pub const SERIALIZABILITY_KEY: QuestionSpec = QuestionSpec {
    id: "discovery.serializability_key",
    version: 1,
    decides: "which enumerated value keys a proposed SerializableBy",
};

pub const ORDERING: QuestionSpec = QuestionSpec {
    id: "discovery.ordering",
    version: 1,
    decides: "whether OrderedBy is proposed on a transaction that already guards a position",
};

pub const OBLIGATION: QuestionSpec = QuestionSpec {
    id: "discovery.obligation",
    version: 1,
    decides: "which enumerated requirement an explicit prompt obligation maps to",
};

/// The option a choice carries when its right answer may be missing
/// from what code enumerated (§12).
pub const NONE_OF_THESE: &str = "none_of_these";

/// Must handling the same trigger twice have the effect of handling it
/// once?
pub fn idempotency() -> Question {
    Question::noul_with_criteria(
        json!({
            "question": "Does `prompt` require that handling the same `operation.triggered_by` \
                         more than once has the same effect as handling it once?",
            "focus": "Judge only what `prompt` says about retries, duplicates or repeated \
                      deliveries reaching `operation`. Do not infer it from the operation \
                      merely being important.",
        }),
        json!({
            "what": "The prompt says or clearly implies that a retried request or a duplicate \
                     or redelivered message must not repeat this operation's effects.",
            "examples": [
                "clients may retry, and must never be charged twice",
                "messages can be delivered more than once",
                "creating the same order twice must produce one order",
            ],
        }),
        json!({
            "what": "The prompt says nothing about retries, duplicates or repeated deliveries \
                     that would reach this operation.",
            "not_for": "A prompt that only says the operation must be correct, fast or reliable \
                        in general.",
        }),
    )
}

/// Must a retried request see the original's response? Asked only of a
/// request-triggered operation, and read only when idempotency is
/// proposed.
pub fn result_replay() -> Question {
    Question::noul_with_criteria(
        "Assuming a retried request must not repeat the effects of `operation`: does `prompt` \
         also require that the retry receives the same response the original request received?",
        "The prompt says a retry returns the same result, the same order, the same confirmation, \
         or otherwise the original outcome.",
        "The prompt does not say what a retried request should receive in response.",
    )
}

/// Must work the operation started be finishable after an interruption?
pub fn recoverability() -> Question {
    Question::noul_with_criteria(
        json!({
            "question": "Does `prompt` require that once `operation` has started, its remaining \
                         work is still completed after a crash, restart or interruption?",
            "focus": "Judge only what `prompt` says about failures part-way through this \
                      operation, or about work that must not be left half-done.",
        }),
        json!({
            "what": "The prompt says or clearly implies the operation must not be left half-done: \
                     work that was started must eventually finish or be resumable.",
            "examples": [
                "a payment that was captured must always lead to a confirmed order",
                "if the service crashes mid-way the order must not be stuck",
            ],
        }),
        json!({
            "what": "The prompt says nothing about crashes, interruptions or half-finished work \
                     for this operation.",
        }),
    )
}

/// Resumable or guaranteed? Read only when recoverability is proposed.
pub fn guaranteed_completion() -> Question {
    Question::noul_with_criteria(
        "Assuming `operation` must be completed after an interruption: does `prompt` require the \
         system itself to keep driving it until it finishes, rather than only making it safe for \
         someone to retry later?",
        "The prompt says the system must itself retry, resume or re-drive interrupted work until \
         it finishes.",
        "The prompt at most requires that an interrupted operation can be retried safely; it does \
         not say the system must do the retrying.",
    )
}

/// Must simultaneous executions of one transaction not interfere?
/// `work` indexes `operation.work`.
pub fn serializability(work: usize) -> Question {
    Question::noul_with_criteria(
        json!({
            "question": format!(
                "Does `prompt` require that simultaneous executions of the work described in \
                 `operation.work[{work}]` on the same data do not interfere with one another?"
            ),
            "focus": "Interference means a wrong result that only concurrency can cause: a lost \
                      update, a double booking, overselling, or exceeding a limit.",
        }),
        json!({
            "what": "The prompt says or clearly implies that concurrent executions must behave as \
                     if they ran one at a time.",
            "examples": [
                "never sell more stock than exists",
                "two people must not be able to book the same seat",
                "the balance must never go below zero even under concurrent withdrawals",
            ],
        }),
        json!({
            "what": "The prompt states no invariant that concurrent executions of this work \
                     could break.",
        }),
    )
}

/// Which enumerated value keys the serializability of `work`? Each
/// candidate is `(option key, what the value is and what it selects)`.
/// The premise is stated because the question is asked speculatively,
/// beside the one that decides whether it matters.
pub fn serializability_key(work: usize, candidates: &[(String, String)]) -> Question {
    Question::choice(
        format!(
            "Assuming simultaneous executions of the work in `operation.work[{work}]` must not \
             interfere when they concern the same thing: which value identifies that thing?"
        ),
        candidates
            .iter()
            .map(|(option, description)| (option.clone(), Json::String(description.clone())))
            .chain([(
                NONE_OF_THESE.to_string(),
                Json::String(
                    "None of the listed values identifies what must not be worked on \
                     simultaneously."
                        .to_string(),
                ),
            )]),
    )
}

/// Must the changes of a transaction that already guards a position be
/// applied in order?
pub fn ordering(work: usize) -> Question {
    Question::noul_with_criteria(
        format!(
            "Does `prompt` require that the changes made by `operation.work[{work}]` are applied \
             in a meaningful order, so that an older change is never applied after a newer one?"
        ),
        "The prompt says or clearly implies events, updates or messages must be applied in their \
         order — by sequence number, version, or the order in which they happened.",
        "The prompt states no ordering among the changes this work applies.",
    )
}

/// Which enumerated requirement expresses one explicit obligation?
/// `obligation` indexes `obligations`; each candidate is `(option key,
/// what the requirement would guarantee)`.
pub fn obligation(obligation: usize, candidates: &[(String, String)]) -> Question {
    Question::choice(
        json!({
            "question": format!(
                "Which requirement below guarantees what `obligations[{obligation}].intent` asks \
                 for?"
            ),
            "focus": "Match what the obligation protects against, not the words it uses.",
        }),
        candidates
            .iter()
            .map(|(option, description)| (option.clone(), Json::String(description.clone())))
            .chain([(
                NONE_OF_THESE.to_string(),
                Json::String(
                    "None of the listed requirements guarantees what the obligation asks for."
                        .to_string(),
                ),
            )]),
    )
}

/// What an idempotency requirement would guarantee, as an option of
/// [`obligation`].
pub const IDEMPOTENCY_GUARANTEES: &str = "Handling the same request or message more than once — a \
     retry or a duplicate delivery — has the same effect as handling it once.";

/// What a recoverability requirement would guarantee.
pub const RECOVERABILITY_GUARANTEES: &str = "Work the operation has started is completed, or can \
     be resumed, after a crash or interruption; it is never left half-done.";

/// What `SerializableBy` on one transaction would guarantee.
pub fn serializability_guarantees(work: &str) -> String {
    format!(
        "Simultaneous executions of this work behave as if they ran one at a time — no lost \
         update, double booking or overselling: {work}"
    )
}

/// What `OrderedBy` on one transaction would guarantee.
pub fn ordering_guarantees(work: &str) -> String {
    format!(
        "The changes this work applies are applied in order; an older one is never applied after \
         a newer one: {work}"
    )
}
