//! The local-server conformance suite (§11.2, §32 of the System One
//! orchestration revision).
//!
//! Reading option probabilities from a stock model's logits reproduces
//! the shape and the speed of a System One model. It does not
//! reproduce its training, and an implementation can reproduce the
//! shape while inventing the numbers: flooring a confidence, handing
//! the mass it did not compute out evenly, or answering a question it
//! could not resolve with its first option.
//!
//! So a server is admitted on how it behaves, never on what its
//! documentation says. Every probe here is black-box, and each fails a
//! server that reproduces the wire shape without the behaviour. The
//! hosted service is the reference: a probe it fails is a wrong probe.
//!
//! The suite says nothing about how *good* a server's judgments are on
//! Conseqa's own questions. That is what shadow mode measures (§11.5).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::questions::conformance::{
    self as wording, BILLING, CRITERIA_ARE_READ, DELIVERY, EVERY_OPTION_IS_OFFERED, MESSAGES,
    PROBABILITIES_ARE_THE_MODELS, QUESTIONS_ARE_ISOLATED, TYPED_SHAPES,
};
use super::{
    Answer, Decider, DeciderError, DeciderIdentity, Decision, DecisionRequest, QuestionSpec,
    SystemOneHttpDecider,
};

/// How far one question's probabilities may move when an unrelated
/// question is asked beside it.
const ISOLATION_TOLERANCE: f64 = 0.05;

/// How far the same request may move when it is simply asked again.
const CONSISTENCY_TOLERANCE: f64 = 0.02;

/// The least by which a rule-assigned winner stands clear of the rest.
const FLOOR_MARGIN: f64 = 0.2;

const EQUAL: f64 = 1e-6;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Pass,

    /// Worth knowing, and no bar to admission.
    Warn,

    Fail,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeResult {
    pub probe: String,
    pub verdict: Verdict,
    pub detail: String,
}

/// What the suite found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConformanceReport {
    pub server: DeciderIdentity,
    pub probes: Vec<ProbeResult>,

    /// Requests sent, for the cost of having asked.
    pub requests: u32,
    pub input_tokens: u64,
}

impl ConformanceReport {
    /// Whether the server may be used as a decider: no probe failed.
    pub fn admissible(&self) -> bool {
        self.probes
            .iter()
            .all(|probe| probe.verdict != Verdict::Fail)
    }
}

struct Run<'a> {
    server: &'a SystemOneHttpDecider,
    probes: Vec<ProbeResult>,
    requests: u32,
    input_tokens: u64,
}

impl Run<'_> {
    fn record(&mut self, probe: &str, verdict: Verdict, detail: impl Into<String>) {
        self.probes.push(ProbeResult {
            probe: probe.to_string(),
            verdict,
            detail: detail.into(),
        });
    }

    async fn ask(
        &mut self,
        spec: QuestionSpec,
        request: DecisionRequest,
    ) -> Result<Decision, DeciderError> {
        self.requests += 1;

        let decision = self.server.decide(&request.tag("spec", spec.tag())).await?;

        self.input_tokens += decision.usage.map_or(0, |usage| usage.input_tokens);

        Ok(decision)
    }
}

/// Runs every probe against `server`.
///
/// An answer that does not conform to its question — an unreported
/// option, a distribution that is not one, a choice that is not the
/// most probable — is refused by the decider itself, and fails the
/// probe that met it.
pub async fn run(server: &SystemOneHttpDecider) -> ConformanceReport {
    let mut run = Run {
        server,
        probes: Vec::new(),
        requests: 0,
        input_tokens: 0,
    };

    criteria_are_read(&mut run).await;
    every_option_is_offered(&mut run).await;
    probabilities_are_the_models(&mut run).await;
    unaskable_is_an_error(&mut run).await;
    questions_are_isolated(&mut run).await;
    typed_shapes(&mut run).await;

    ConformanceReport {
        server: server.identity(),
        probes: run.probes,
        requests: run.requests,
        input_tokens: run.input_tokens,
    }
}

/// Opaque option keys, distinguishable only by their criteria, asked
/// under two rotations: every message must be given its own
/// description both times.
async fn criteria_are_read(run: &mut Run<'_>) {
    const PROBE: &str = "criteria_are_read";

    let mut wrong = Vec::new();

    for (index, message) in MESSAGES.into_iter().enumerate() {
        let (first, first_key) = wording::topic_choice(message, 0);
        let (second, second_key) = wording::topic_choice(message, 1);

        let request = DecisionRequest::new(wording::message_state(message))
            .ask("first", first)
            .ask("second", second);

        let decision = match run.ask(CRITERIA_ARE_READ, request).await {
            Ok(decision) => decision,

            Err(error) => return run.record(PROBE, Verdict::Fail, error.to_string()),
        };

        for (id, expected) in [("first", first_key), ("second", second_key)] {
            let chosen = decision.choice(id).map_or("", |(choice, _)| choice);

            if chosen != expected {
                wrong.push(format!(
                    "message {index}, {id} rotation: chose `{chosen}`, its description is under `{expected}`"
                ));
            }
        }
    }

    if wrong.is_empty() {
        run.record(
            PROBE,
            Verdict::Pass,
            "every message was matched to its description under both rotations",
        );
    } else {
        run.record(PROBE, Verdict::Fail, wrong.join("; "));
    }
}

/// A thirty-option choice whose correct option sorts last.
async fn every_option_is_offered(run: &mut Run<'_>) {
    const PROBE: &str = "every_option_is_offered";

    let (question, expected) = wording::wide_choice();

    let request = DecisionRequest::new(wording::message_state(DELIVERY)).ask("wide", question);

    match run.ask(EVERY_OPTION_IS_OFFERED, request).await {
        Err(error) => run.record(PROBE, Verdict::Fail, error.to_string()),

        Ok(decision) => match decision.choice("wide") {
            Some((chosen, _)) if chosen == expected => run.record(
                PROBE,
                Verdict::Pass,
                "the last of thirty options was chosen when it was the right one",
            ),

            Some((chosen, _)) => run.record(
                PROBE,
                Verdict::Fail,
                format!("chose `{chosen}` of thirty options; the right one, `{expected}`, is listed last"),
            ),

            None => run.record(PROBE, Verdict::Fail, "no choice was returned"),
        },
    }
}

/// Three choices the state says nothing about. A floor with a uniform
/// remainder is the same distribution every time; a model's is not.
async fn probabilities_are_the_models(run: &mut Run<'_>) {
    const PROBE: &str = "probabilities_are_the_models";

    let mut request = DecisionRequest::new(wording::neutral_state());

    for (id, question) in wording::unanswerable_choices() {
        request = request.ask(id, question);
    }

    let decision = match run.ask(PROBABILITIES_ARE_THE_MODELS, request).await {
        Ok(decision) => decision,

        Err(error) => return run.record(PROBE, Verdict::Fail, error.to_string()),
    };

    let shapes: Vec<Shape> = decision
        .answers
        .values()
        .filter_map(|answer| answer.as_choice())
        .map(|(choice, probabilities)| Shape::of(choice, probabilities))
        .collect();

    let Some(first) = shapes.first().map(|shape| shape.winner) else {
        return run.record(PROBE, Verdict::Fail, "no choice was returned");
    };

    // The questions differ in how many options they offer, so only a
    // rule gives all three the same winner over an even remainder.
    let assigned_by_rule = shapes.iter().all(|shape| {
        shape.uniform_remainder
            && shape.remainder > EQUAL
            && shape.winner - shape.remainder > FLOOR_MARGIN
            && (shape.winner - first).abs() < EQUAL
    });

    if assigned_by_rule {
        return run.record(
            PROBE,
            Verdict::Fail,
            format!(
                "three unrelated questions with nothing to go on all returned a winner at \
                 {first:.4} over an exactly uniform remainder: the numbers are assigned by \
                 rule, not read from the model"
            ),
        );
    }

    if shapes.iter().all(|shape| shape.winner >= 0.9) {
        return run.record(
            PROBE,
            Verdict::Warn,
            "every question the state cannot answer was answered at 0.9 or above: the \
             probabilities are the model's, but they do not express its uncertainty",
        );
    }

    run.record(
        PROBE,
        Verdict::Pass,
        "distributions over unanswerable questions vary as a model's do",
    );
}

struct Shape {
    winner: f64,

    /// The common probability of the options not chosen, when they
    /// share one.
    remainder: f64,
    uniform_remainder: bool,
}

impl Shape {
    fn of(choice: &str, probabilities: &BTreeMap<String, f64>) -> Self {
        let others: Vec<f64> = probabilities
            .iter()
            .filter(|(option, _)| option.as_str() != choice)
            .map(|(_, probability)| *probability)
            .collect();

        let remainder = others.first().copied().unwrap_or(0.0);

        Self {
            winner: probabilities.get(choice).copied().unwrap_or(0.0),
            remainder,
            uniform_remainder: others.iter().all(|other| (other - remainder).abs() < EQUAL),
        }
    }
}

/// Requests that cannot be answered must be refused.
async fn unaskable_is_an_error(run: &mut Run<'_>) {
    const PROBE: &str = "unaskable_is_an_error";

    let model = run.server.identity().model;

    let mut answered = Vec::new();

    for (what, body) in wording::unaskable_requests(&model) {
        run.requests += 1;

        match run.server.post_raw(&body).await {
            Ok((status, _)) if (200..300).contains(&status) => answered.push(what),

            Ok(_) => {}

            Err(error) => return run.record(PROBE, Verdict::Fail, error.to_string()),
        }
    }

    if answered.is_empty() {
        run.record(
            PROBE,
            Verdict::Pass,
            "every request that cannot be answered was refused",
        );
    } else {
        run.record(
            PROBE,
            Verdict::Fail,
            format!(
                "answered instead of refusing: {}. A server that answers what it cannot \
                 resolve will also answer when it has merely failed",
                answered.join("; ")
            ),
        );
    }
}

/// The same question, alone and beside an unrelated one.
async fn questions_are_isolated(run: &mut Run<'_>) {
    const PROBE: &str = "questions_are_isolated";

    let (question, _) = wording::topic_choice(DELIVERY, 0);

    let alone = DecisionRequest::new(wording::message_state(DELIVERY)).ask("topic", question);

    let beside = alone.clone().ask("unrelated", wording::unrelated_noul());

    let (alone, beside) = match (
        run.ask(QUESTIONS_ARE_ISOLATED, alone).await,
        run.ask(QUESTIONS_ARE_ISOLATED, beside).await,
    ) {
        (Ok(alone), Ok(beside)) => (alone, beside),

        (Err(error), _) | (_, Err(error)) => {
            return run.record(PROBE, Verdict::Fail, error.to_string());
        }
    };

    let moved = distance(alone.answer("topic"), beside.answer("topic"));

    if moved <= ISOLATION_TOLERANCE {
        run.record(
            PROBE,
            Verdict::Pass,
            format!("an unrelated question moved the answer by {moved:.4}"),
        );
    } else {
        run.record(
            PROBE,
            Verdict::Fail,
            format!(
                "an unrelated question moved the answer by {moved:.4}, over the tolerance of \
                 {ISOLATION_TOLERANCE}: one question is context for another"
            ),
        );
    }
}

/// A plainly true noul, a plainly false one, and a score at the top of
/// its rubric — then the same request again.
async fn typed_shapes(run: &mut Run<'_>) {
    const PROBE: &str = "typed_shapes";

    let request = DecisionRequest::new(wording::message_state(BILLING))
        .ask("true", wording::true_noul())
        .ask("false", wording::false_noul())
        .ask("refund", wording::refund_score());

    let decision = match run.ask(TYPED_SHAPES, request.clone()).await {
        Ok(decision) => decision,

        Err(error) => return run.record(PROBE, Verdict::Fail, error.to_string()),
    };

    let mut wrong = Vec::new();

    match decision.noul("true") {
        Some(noul) if noul >= 0.5 => {}
        other => wrong.push(format!("a plainly true noul came back as {other:?}")),
    }

    match decision.noul("false") {
        Some(noul) if noul < 0.5 => {}
        other => wrong.push(format!("a plainly false noul came back as {other:?}")),
    }

    match decision.score("refund") {
        Some(score) if score >= 1.0 => {}
        other => wrong.push(format!(
            "a message that explicitly asks for a refund scored {other:?} of 2"
        )),
    }

    if !wrong.is_empty() {
        return run.record(PROBE, Verdict::Fail, wrong.join("; "));
    }

    run.record(
        PROBE,
        Verdict::Pass,
        "choice, score and noul were each answered sensibly in their typed shape",
    );

    // Recordings stand in for a server only if the server would have
    // said the same thing again.
    const CONSISTENCY: &str = "answers_are_repeatable";

    match run.ask(TYPED_SHAPES, request).await {
        Err(error) => run.record(CONSISTENCY, Verdict::Warn, error.to_string()),

        Ok(again) => {
            let moved = decision
                .answers
                .iter()
                .map(|(id, answer)| distance(Some(answer), again.answers.get(id)))
                .fold(0.0, f64::max);

            if moved <= CONSISTENCY_TOLERANCE {
                run.record(
                    CONSISTENCY,
                    Verdict::Pass,
                    format!("asking again moved the answers by {moved:.4}"),
                );
            } else {
                run.record(
                    CONSISTENCY,
                    Verdict::Warn,
                    format!(
                        "asking again moved the answers by {moved:.4}: recordings of this \
                         server will not reproduce it"
                    ),
                );
            }
        }
    }
}

/// The largest movement between two answers to one question; one when
/// they cannot be compared.
fn distance(first: Option<&Answer>, second: Option<&Answer>) -> f64 {
    match (first, second) {
        (
            Some(Answer::Choice { probabilities, .. }),
            Some(Answer::Choice {
                probabilities: other,
                ..
            }),
        )
        | (
            Some(Answer::Score { probabilities, .. }),
            Some(Answer::Score {
                probabilities: other,
                ..
            }),
        ) => probabilities
            .iter()
            .map(|(key, probability)| (probability - other.get(key).copied().unwrap_or(0.0)).abs())
            .fold(0.0, f64::max),

        (Some(Answer::Noul { noul }), Some(Answer::Noul { noul: other })) => (noul - other).abs(),

        _ => 1.0,
    }
}
