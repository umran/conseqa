//! The wording of the local-server conformance probes (§32).
//!
//! The probes do not measure how clever a server is. Each is a
//! question any competent model answers, asked in a way that only a
//! server which really reads its options, reports its own numbers and
//! isolates its questions can answer correctly. The material is
//! deliberately mundane: three support messages, each unmistakably
//! about one thing.

use serde_json::json;

use crate::system_one::{Json, Question, QuestionSpec};

pub const CRITERIA_ARE_READ: QuestionSpec = QuestionSpec {
    id: "conformance.criteria_are_read",
    version: 1,
    decides: "whether a server reads option criteria, and ignores option order",
};

pub const EVERY_OPTION_IS_OFFERED: QuestionSpec = QuestionSpec {
    id: "conformance.every_option_is_offered",
    version: 1,
    decides: "whether a server shows the model every option of a wide choice",
};

pub const PROBABILITIES_ARE_THE_MODELS: QuestionSpec = QuestionSpec {
    id: "conformance.probabilities_are_the_models",
    version: 1,
    decides: "whether reported probabilities are read from the model or assigned by rule",
};

pub const QUESTIONS_ARE_ISOLATED: QuestionSpec = QuestionSpec {
    id: "conformance.questions_are_isolated",
    version: 1,
    decides: "whether one question's presence changes another's answer",
};

pub const TYPED_SHAPES: QuestionSpec = QuestionSpec {
    id: "conformance.typed_shapes",
    version: 1,
    decides: "whether choice, score and noul are each answered sensibly in their typed shape",
};

/// One message and the one topic it is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Message {
    pub text: &'static str,

    /// The index, into [`TOPICS`], of what the message is about.
    pub topic: usize,
}

pub const DELIVERY: Message = Message {
    text: "My parcel was due last Tuesday and it still has not arrived. \
           Where is my delivery?",
    topic: 0,
};

pub const BILLING: Message = Message {
    text: "I was charged twice for the same invoice this month. \
           Please refund the duplicate payment.",
    topic: 1,
};

pub const PASSWORD: Message = Message {
    text: "I cannot log in because I forgot my password. How do I reset it?",
    topic: 2,
};

pub const MESSAGES: [Message; 3] = [DELIVERY, BILLING, PASSWORD];

/// What a message may be about. The option *keys* these are attached
/// to are opaque, so only a server that reads the descriptions can
/// tell the options apart.
pub const TOPICS: [&str; 3] = [
    "The message is about a late or missing parcel delivery.",
    "The message is about a duplicate charge, an invoice, a payment or a refund.",
    "The message is about a forgotten password, or how to reset a password and log in.",
];

const OPAQUE_KEYS: [&str; 3] = ["k1", "k2", "k3"];

/// The state every message probe is asked of.
pub fn message_state(message: Message) -> Json {
    json!({ "message": message.text })
}

/// "Which topic?" with the topics attached to opaque keys, rotated by
/// `rotation`. Returns the question and the key that is correct.
///
/// Asking under two rotations moves every description to a different
/// key and a different position: a server that reads criteria picks
/// the same *description* both times, and one that favours a position
/// or a key does not.
pub fn topic_choice(message: Message, rotation: usize) -> (Question, &'static str) {
    let key_of = |topic: usize| OPAQUE_KEYS[(topic + rotation) % OPAQUE_KEYS.len()];

    let question = Question::choice(
        "What is `message` about?",
        TOPICS
            .iter()
            .enumerate()
            .map(|(topic, description)| (key_of(topic), *description)),
    );

    (question, key_of(message.topic))
}

/// Subjects no probe message touches, for the distractors of a wide
/// choice.
const UNRELATED_SUBJECTS: [&str; 29] = [
    "gardening and houseplants",
    "astronomy and telescopes",
    "baking sourdough bread",
    "marathon training plans",
    "chess openings",
    "watercolour painting",
    "bird watching",
    "sailing knots",
    "medieval castles",
    "jazz piano chords",
    "volcanic geology",
    "beekeeping",
    "origami folding",
    "film photography",
    "mountain hiking routes",
    "pottery glazing",
    "crossword puzzles",
    "coral reef ecology",
    "vintage bicycles",
    "cheese making",
    "cave exploration",
    "calligraphy pens",
    "model railways",
    "tea ceremonies",
    "rock climbing holds",
    "weather forecasting",
    "stamp collecting",
    "woodworking joints",
    "ancient coins",
];

/// A thirty-option choice whose one correct option sorts last. A
/// server that lists only the first few options to its model cannot
/// choose it. Returns the question and the correct key.
pub fn wide_choice() -> (Question, &'static str) {
    const CORRECT: &str = "o30";

    let distractors = UNRELATED_SUBJECTS
        .iter()
        .enumerate()
        .map(|(index, subject)| {
            (
                format!("o{:02}", index + 1),
                format!("The message is about {subject}."),
            )
        });

    let question = Question::choice(
        "What is `message` about?",
        distractors.chain([(CORRECT.to_string(), TOPICS[DELIVERY.topic].to_string())]),
    );

    (question, CORRECT)
}

/// A state that answers none of [`unanswerable_choices`].
pub fn neutral_state() -> Json {
    json!({ "text": "The quick brown fox jumps over the lazy dog." })
}

/// Three choices the state says nothing about, with no no-match
/// option, so a server must spread or place its probability with
/// nothing to go on. What is examined is not the choice but whether
/// the numbers are the model's.
///
/// The three have four, five and six options on purpose. A
/// rule-assigned floor gives the same winning probability whatever the
/// option count. A model does not — not even one that is confidently
/// wrong and rounds its numbers, since the same rounded remainder over
/// more options leaves a different winner.
pub fn unanswerable_choices() -> [(&'static str, Question); 3] {
    fn bare<const N: usize>(
        options: [&'static str; N],
    ) -> impl Iterator<Item = (&'static str, Json)> {
        options.into_iter().map(|option| (option, Json::Null))
    }

    [
        (
            "database",
            Question::choice(
                "Which database engine does `text` say the team migrated to?",
                bare(["postgres", "mysql", "sqlite", "oracle"]),
            ),
        ),
        (
            "city",
            Question::choice(
                "Which city does `text` say the conference is held in?",
                bare(["lisbon", "osaka", "nairobi", "denver", "tallinn"]),
            ),
        ),
        (
            "language",
            Question::choice(
                "Which programming language does `text` say the service is written in?",
                bare(["rust", "kotlin", "elixir", "haskell", "zig", "ocaml"]),
            ),
        ),
    ]
}

/// A question unrelated to [`topic_choice`], added beside it to see
/// whether its presence moves the other's answer.
pub fn unrelated_noul() -> Question {
    Question::noul("Does `message` mention a refund?")
}

/// A noul that is plainly true of [`BILLING`].
pub fn true_noul() -> Question {
    Question::noul_with_criteria(
        "Does `message` say the customer was charged twice?",
        "The message says the customer was charged twice, or charged a duplicate amount.",
        "The message does not say the customer was charged twice.",
    )
}

/// A noul that is plainly false of [`BILLING`].
pub fn false_noul() -> Question {
    Question::noul_with_criteria(
        "Does `message` say a parcel was lost in transit?",
        "The message says a parcel was lost, late or never arrived.",
        "The message does not mention a parcel at all.",
    )
}

/// A score [`BILLING`] belongs at the top of.
pub fn refund_score() -> Question {
    Question::score(
        "How directly does `message` ask for money back?",
        [
            "The message does not ask for money back at all.",
            "The message complains about a charge without asking for anything.",
            "The message explicitly asks for a refund.",
        ],
    )
}

/// Requests no server should answer, as raw bodies. Each is one the
/// wire format's own reference says fails validation, so the hosted
/// service refuses all three. The right response is an error: a server
/// that answers anyway will also answer when it has merely failed.
pub fn unaskable_requests(model: &str) -> [(&'static str, Json); 3] {
    let state = message_state(DELIVERY);

    [
        (
            "a request with no state",
            json!({
                "model": model,
                "questions": { "q": {
                    "type": "noul",
                    "instructions": "Does `message` mention a parcel?",
                } },
            }),
        ),
        (
            "a score with a single level",
            json!({
                "state": state,
                "model": model,
                "questions": { "q": {
                    "type": "score",
                    "instructions": "How urgent is `message`?",
                    "criteria": ["Not urgent."],
                } },
            }),
        ),
        (
            "a question of an unknown type",
            json!({
                "state": state,
                "model": model,
                "questions": { "q": {
                    "type": "essay",
                    "instructions": "Describe `message`.",
                } },
            }),
        ),
    ]
}
