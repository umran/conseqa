//! The requirement discovery builder (§17 of the System One
//! orchestration revision).
//!
//! Discovery decides which correctness requirements an operation
//! declares. Almost all of it is enumeration, which is code's job:
//!
//! - the *transactions* that mutate state, each with the input values
//!   that pin the identity of what it touches — the only values that
//!   can key a `SerializableBy`;
//! - the cursor and fence steps, whose incoming position is the only
//!   thing an `OrderedBy` can order by;
//! - the declared identity of the triggering input, the only value
//!   that can key an idempotency or recoverability requirement.
//!
//! What is left for a System One decider is what code cannot know:
//! which enumerated requirement an explicit prompt obligation is
//! asking for, and — under a policy that adopts implied requirements —
//! whether the prompt states one.
//!
//! The builder decides a discovery task whole or not at all. A task
//! covers every family of its operation, and the workflow schedules it
//! only while the operation declares nothing, so a builder that
//! proposed part of the answer would leave the rest undiscovered.
//!
//! It proposes only what the run's policy adopts. A proposal that is
//! merely recorded moves the head without changing what the operation
//! declares, and would be proposed again on every pass of the fixpoint.

use std::collections::BTreeMap;

use serde_json::json;
use uuid::Uuid;

use crate::confluence::{
    BundleSpec, CommitRequest, DraftOperation, EngineError, EvidenceRef, Mutation, PatchId,
    PromptObligation, PromptObligationId, PromptObligationStatus, ProposedRequirement,
    RequirementOrigin, RequirementSubmission, SearchSpec, SpecPatch, SymbolKey, SymbolKind,
    SymbolView, WriteGrant,
};
use crate::spec::{
    CompletionRequirement, DataObject, FieldPath, Id, IdempotencyKey, IdempotencyRequirement,
    Input, MessageIdentity, MessageSelector, ObjectSelector, RecoverabilityRequirement,
    RequestIdentity, ResultReplayRequirement, SelectorValue, Topic, Transaction,
    TransactionOrderingRequirement, TransactionSerializabilityRequirement, TransactionStep,
    ValueRef, ValueSource,
};
use crate::system_one::questions::discovery as wording;
use crate::system_one::{Decision, DecisionRequest, QuestionSpec};

use super::describe::{is_input, label, pins, summarize};
use super::{Abstention, BuildContext, Built};

/// The probabilities discovery acts on. Provisional: to be chosen from
/// the decision log, per question and per backend (§13.3).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DiscoveryPolicy {
    /// At or above, the prompt states the requirement.
    pub act: f64,

    /// At or below, it does not. Between the two the builder abstains:
    /// an uncertain judgment is escalated, never guessed.
    pub dismiss: f64,

    /// The least probability at which a chosen option — a key, or the
    /// requirement an obligation maps to — is accepted.
    pub select: f64,
}

impl Default for DiscoveryPolicy {
    fn default() -> Self {
        Self {
            act: 0.8,
            dismiss: 0.25,
            select: 0.6,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Band {
    Stated,
    NotStated,
    Uncertain,
}

impl DiscoveryPolicy {
    fn band(&self, probability: f64) -> Band {
        if probability >= self.act {
            Band::Stated
        } else if probability <= self.dismiss {
            Band::NotStated
        } else {
            Band::Uncertain
        }
    }
}

/// One mutating transaction, and what could be required of it.
struct Work {
    transaction: Id,

    /// What it reads and changes, in words a prompt could be matched
    /// against.
    summary: String,

    /// Input values pinning the identity of an accessed object.
    keys: Vec<ValueRef>,

    /// `(key, position)` of each cursor or fence step whose selector is
    /// pinned by, and whose position is, an input value.
    guards: Vec<(ValueRef, ValueRef)>,
}

/// A requirement code enumerated, before anything is decided about it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Candidate {
    Idempotency,
    Recoverability,
    Serializability(usize),
    Ordering(usize),
}

impl Candidate {
    fn option(&self) -> String {
        match self {
            Self::Idempotency => "idempotency".to_string(),
            Self::Recoverability => "recoverability".to_string(),
            Self::Serializability(work) => format!("serializability_{work}"),
            Self::Ordering(work) => format!("ordering_{work}"),
        }
    }
}

struct Enumerated {
    operation: Id,
    draft: DraftOperation,
    request_triggered: bool,
    triggered_by: String,

    /// The declared identity of the single triggering input.
    input_key: Option<IdempotencyKey>,

    work: Vec<Work>,
    obligations: Vec<(PromptObligationId, PromptObligation)>,
}

pub(super) async fn build(context: &BuildContext<'_>, policy: &DiscoveryPolicy) -> Built {
    match discover(context, policy).await {
        Ok(built) => built,

        // An engine failure is not a judgment about the task; the agent
        // backend meets the same engine and reports it properly.
        Err(error) => Built::Abstained(Abstention::because(format!(
            "the engine refused a read: {error}"
        ))),
    }
}

async fn discover(
    context: &BuildContext<'_>,
    policy: &DiscoveryPolicy,
) -> Result<Built, EngineError> {
    let task = context.engine.task_context(context.task)?;

    let Some(operation) = task
        .write_scope
        .grants
        .iter()
        .find_map(|grant| match grant {
            WriteGrant::OperationRequirements(operation) => Some(operation.clone()),
            _ => None,
        })
    else {
        return Ok(abstain(
            "the task's scope names no operation to discover for",
        ));
    };

    let enumerated = match enumerate(context, &operation)? {
        Ok(enumerated) => enumerated,
        Err(abstention) => return Ok(Built::Abstained(abstention)),
    };

    // Run metadata is fixed for the run; it is not architecture, and
    // reading it observes nothing a commit could make stale.
    let adopts_implied = context
        .engine
        .head_snapshot()
        .workspace
        .run_meta
        .policy
        .strict_requirements;

    if enumerated.obligations.is_empty() && !adopts_implied {
        return Ok(Built::NothingToDo {
            summary: format!(
                "{operation}: no explicit obligation targets it, and the run's policy adopts \
                 only explicit ones"
            ),
        });
    }

    let prompt: String = task
        .prompt_evidence
        .iter()
        .map(|evidence| evidence.excerpt.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");

    if prompt.trim().is_empty() {
        return Ok(abstain(
            "the run carries no prompt to judge requirements against",
        ));
    }

    let evidence: Vec<EvidenceRef> = task
        .prompt_evidence
        .iter()
        .map(|evidence| evidence.source.clone())
        .collect();

    let request = questions(context, &enumerated, &prompt, adopts_implied);

    let decision = match context.decider.decide(&request).await {
        Ok(decision) => decision,

        Err(error) => {
            return Ok(abstain(format!("the decider gave no answer: {error}")));
        }
    };

    let proposals = match decide(&enumerated, &decision, policy, adopts_implied, &evidence) {
        Ok(proposals) => proposals,
        Err(abstention) => return Ok(Built::Abstained(abstention)),
    };

    if proposals.is_empty() {
        return Ok(Built::NothingToDo {
            summary: format!("{operation}: the prompt states no requirement the policy adopts"),
        });
    }

    let described: Vec<String> = proposals.iter().map(describe).collect();

    let outcome = context
        .engine
        .submit(CommitRequest {
            task: context.task,
            patch_id: PatchId::fresh(),
            base_revision: task.snapshot_revision,
            patch: SpecPatch {
                mutations: vec![Mutation::ProposeRequirements {
                    operation: operation.clone(),
                    proposals,
                }],
            },
            client_nonce: Uuid::new_v4(),
        })
        .await?;

    Ok(match outcome {
        Ok(_) => Built::Committed {
            summary: format!("{operation}: proposed {}", described.join("; ")),
        },

        Err(rejection) if rejection.is_stale_context() => Built::Stale,

        // The gate's own validation refused the patch. The builder has
        // no second idea; the session is told what was tried.
        Err(rejection) => Built::Abstained(
            Abstention::because(format!("the gate rejected the proposals: {rejection:?}"))
                .with_findings(described),
        ),
    })
}

fn abstain(reason: impl Into<String>) -> Built {
    Built::Abstained(Abstention::because(reason))
}

// ---------------------------------------------------------------------
// Enumeration
// ---------------------------------------------------------------------

fn enumerate(
    context: &BuildContext<'_>,
    operation: &Id,
) -> Result<Result<Enumerated, Abstention>, EngineError> {
    // The bundle is the tracked read: the operation's draft and the
    // shared symbols it depends on are all recorded as observed.
    let bundle = context.engine.context_bundle(
        context.task,
        &BundleSpec {
            operation: Some(operation.clone()),
            requirements: Vec::new(),
            include: Vec::new(),
            peers: Vec::new(),
        },
    )?;

    let Some(draft) = bundle
        .operation
        .and_then(|draft| serde_json::from_value::<DraftOperation>(draft).ok())
    else {
        return Ok(Err(Abstention::because(
            "the operation's draft could not be read",
        )));
    };

    let Some(program) = &draft.program else {
        return Ok(Err(Abstention::because("the operation has no program yet")));
    };

    if draft.inputs.len() != 1 {
        return Ok(Err(Abstention::because(format!(
            "the operation has {} inputs; a requirement key is enumerated for exactly one",
            draft.inputs.len()
        ))));
    }

    let (input_id, input) = draft.inputs.iter().next().expect("one input");

    let (triggered_by, input_key) = trigger(input_id, input, &bundle.shared_symbols);

    let identities: BTreeMap<Id, Vec<FieldPath>> = bundle
        .shared_symbols
        .iter()
        .filter_map(|view| match &view.key {
            SymbolKey::DataObject { object, .. } => {
                serde_json::from_value::<DataObject>(view.content.clone())
                    .ok()
                    .map(|data| (object.clone(), data.identity))
            }
            _ => None,
        })
        .collect();

    let work: Vec<Work> = program
        .transactions()
        .into_iter()
        .filter_map(|(_, transaction)| work_of(transaction, &identities))
        .collect();

    let mut obligations = Vec::new();

    for key in context.engine.search_symbols(
        context.task,
        // Only the obligations aimed here are read, so a peer mapping
        // its own does not invalidate this task.
        &SearchSpec {
            kind: Some(SymbolKind::PromptObligation),
            targets: Some(operation.clone()),
            ..Default::default()
        },
    )? {
        let SymbolKey::PromptObligation(id) = &key else {
            continue;
        };

        let view = context.engine.read_symbol(context.task, &key)?;

        let Ok(obligation) = serde_json::from_value::<PromptObligation>(view.content) else {
            continue;
        };

        if obligation.status == PromptObligationStatus::Unmapped {
            obligations.push((id.clone(), obligation));
        }
    }

    Ok(Ok(Enumerated {
        operation: operation.clone(),
        request_triggered: matches!(input, Input::Request(_)),
        draft,
        triggered_by,
        input_key,
        work,
        obligations,
    }))
}

/// How the operation is triggered, in words, and the declared identity
/// of its trigger as a requirement key.
fn trigger(
    input_id: &Id,
    input: &Input,
    shared: &[SymbolView],
) -> (String, Option<IdempotencyKey>) {
    let key_of = |fields: &[FieldPath]| {
        (!fields.is_empty()).then(|| IdempotencyKey {
            components: fields
                .iter()
                .map(|field| ValueRef {
                    source: ValueSource::Input(input_id.clone()),
                    path: field.clone(),
                })
                .collect(),
        })
    };

    match input {
        Input::Request(request) => (
            "a synchronous request from a client".to_string(),
            match &request.identity {
                RequestIdentity::Keyed(key) => key_of(&key.fields),
                RequestIdentity::Unspecified => None,
            },
        ),

        Input::Subscription(subscription) => {
            let topic = shared.iter().find_map(|view| match &view.key {
                SymbolKey::Topic(id) if id == &subscription.topic => {
                    serde_json::from_value::<Topic>(view.content.clone()).ok()
                }
                _ => None,
            });

            // One identity for the input only when every schema it
            // admits maps the same fields: a key's path must resolve in
            // every payload the input can carry.
            let identity = topic.and_then(|topic| {
                let MessageIdentity::Keyed(key) = &topic.message_identity else {
                    return None;
                };

                let admitted: Vec<&Id> = match &subscription.messages {
                    MessageSelector::All => topic.messages.iter().collect(),
                    MessageSelector::Only(schemas) => schemas.iter().collect(),
                };

                let mut mapped = admitted.iter().map(|schema| key.mapping.get(*schema));

                let first = mapped.next()??.clone();

                mapped.all(|fields| fields == Some(&first)).then_some(first)
            });

            (
                format!("a message delivered from `{}`", subscription.topic),
                identity.as_deref().and_then(key_of),
            )
        }

        Input::Outbox(outbox) => (
            format!("a message consumed from outbox `{}`", outbox.outbox),
            None,
        ),
    }
}

fn work_of(transaction: &Transaction, identities: &BTreeMap<Id, Vec<FieldPath>>) -> Option<Work> {
    // A transaction that changes nothing has no committed history to
    // constrain.
    let summary = summarize(transaction)?;

    let mut keys: Vec<ValueRef> = Vec::new();
    let mut guards = Vec::new();

    let mut note_keys = |selector: &ObjectSelector| -> Vec<ValueRef> {
        let found = identity_inputs(selector, identities);

        for key in &found {
            if !keys.contains(key) {
                keys.push(key.clone());
            }
        }

        found
    };

    for step in &transaction.steps {
        match step {
            TransactionStep::Read(read) => {
                note_keys(&read.target);
            }

            TransactionStep::Update(update) => {
                note_keys(&update.target);
            }

            TransactionStep::CompareAndSet(cas) => {
                note_keys(&cas.target);
            }

            TransactionStep::Upsert(upsert) => {
                note_keys(&upsert.target);
            }

            TransactionStep::Delete(delete) => {
                note_keys(&delete.target);
            }

            TransactionStep::Transition(transition) => {
                note_keys(&transition.subject);
            }

            TransactionStep::AdvanceCursor(advance) => {
                let pinned = note_keys(&advance.target);

                if let Some(key) = pinned.first()
                    && is_input(&advance.incoming)
                {
                    guards.push((key.clone(), advance.incoming.clone()));
                }
            }

            TransactionStep::Fence(fence) => {
                let pinned = note_keys(&fence.target);

                if let Some(key) = pinned.first()
                    && is_input(&fence.token)
                {
                    guards.push((key.clone(), fence.token.clone()));
                }
            }

            // Locks are not work a prompt would describe, but what they
            // protect is still what the transaction is about: the
            // instance they pin is a key.
            TransactionStep::Lock(lock) => {
                note_keys(&lock.target);
            }

            TransactionStep::Insert(_)
            | TransactionStep::WriteOutbox(_)
            | TransactionStep::EstablishEffectIntent(_)
            | TransactionStep::EstablishTransactionOutput(_) => {}
        }
    }

    Some(Work {
        transaction: transaction.id.clone(),
        summary,
        keys,
        guards,
    })
}

/// The input values that pin an identity field of the selected object.
/// Only an input is available when the transaction begins, which a
/// requirement key must be.
fn identity_inputs(
    selector: &ObjectSelector,
    identities: &BTreeMap<Id, Vec<FieldPath>>,
) -> Vec<ValueRef> {
    let Some(identity) = identities.get(&selector.object) else {
        return Vec::new();
    };

    pins(&selector.predicate)
        .into_iter()
        .filter(|(field, _)| identity.contains(field))
        .filter_map(|(_, value)| match value {
            SelectorValue::Value(reference) if is_input(reference) => Some(reference.clone()),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------------
// Questions
// ---------------------------------------------------------------------

fn candidates(enumerated: &Enumerated) -> Vec<(Candidate, String)> {
    let mut candidates = Vec::new();

    if enumerated.input_key.is_some() {
        candidates.push((
            Candidate::Idempotency,
            wording::IDEMPOTENCY_GUARANTEES.to_string(),
        ));

        candidates.push((
            Candidate::Recoverability,
            wording::RECOVERABILITY_GUARANTEES.to_string(),
        ));
    }

    for (index, work) in enumerated.work.iter().enumerate() {
        if !work.keys.is_empty() {
            candidates.push((
                Candidate::Serializability(index),
                wording::serializability_guarantees(&work.summary),
            ));
        }

        if !work.guards.is_empty() {
            candidates.push((
                Candidate::Ordering(index),
                wording::ordering_guarantees(&work.summary),
            ));
        }
    }

    candidates
}

fn key_options(work: &Work) -> Vec<(String, String)> {
    work.keys
        .iter()
        .enumerate()
        .map(|(index, key)| {
            (
                format!("key_{index}"),
                format!("The value `{}`.", label(key)),
            )
        })
        .collect()
}

fn questions(
    context: &BuildContext<'_>,
    enumerated: &Enumerated,
    prompt: &str,
    adopts_implied: bool,
) -> DecisionRequest {
    let state = json!({
        "prompt": prompt,
        "operation": {
            "id": enumerated.operation,
            "description": enumerated.draft.description,
            "triggered_by": enumerated.triggered_by,
            "work": enumerated
                .work
                .iter()
                .map(|work| json!({ "transaction": work.transaction, "does": work.summary }))
                .collect::<Vec<_>>(),
        },
        "obligations": enumerated
            .obligations
            .iter()
            .map(|(_, obligation)| json!({
                "intent": obligation.normalized_intent,
                "quoted_from_prompt": obligation.source_span,
            }))
            .collect::<Vec<_>>(),
    });

    let mut request = DecisionRequest::new(state)
        .tag("task", context.task.to_string())
        .tag("builder", "requirement_discovery")
        .tag("operation", enumerated.operation.to_string());

    // Each ask is tagged with the reviewed wording it used, so the
    // decision log can be read per question and per version.
    let ask = |request: DecisionRequest, id: String, spec: QuestionSpec, question| {
        request
            .ask(id, question)
            .tag(format!("spec.{}", spec.id), spec.tag())
    };

    // Which requirements an obligation asks for is a set — "an order
    // ships at most once" needs both idempotency and serializability —
    // so each pairing is its own judgment, not one option of a choice.
    let options: Vec<(String, String)> = candidates(enumerated)
        .into_iter()
        .map(|(candidate, guarantees)| (candidate.option(), guarantees))
        .collect();

    for index in 0..enumerated.obligations.len() {
        for (option, guarantees) in &options {
            request = ask(
                request,
                format!("obligation_{index}_{option}"),
                wording::OBLIGATION,
                wording::obligation(index, guarantees),
            );
        }
    }

    // A key is chosen only where code found more than one: with one
    // candidate there is nothing to judge.
    for (index, work) in enumerated.work.iter().enumerate() {
        if work.keys.len() > 1 {
            request = ask(
                request,
                format!("serializability_key_{index}"),
                wording::SERIALIZABILITY_KEY,
                wording::serializability_key(index, &key_options(work)),
            );
        }
    }

    // Whether the prompt states a requirement matters only under a
    // policy that adopts implied ones. They are asked even where no
    // key was enumerated: a stated requirement code cannot express is
    // the session's to handle, not a requirement to drop.
    if adopts_implied {
        request = ask(
            request,
            "idempotency".to_string(),
            wording::IDEMPOTENCY,
            wording::idempotency(),
        );

        request = ask(
            request,
            "recoverability".to_string(),
            wording::RECOVERABILITY,
            wording::recoverability(),
        );

        for (index, work) in enumerated.work.iter().enumerate() {
            request = ask(
                request,
                format!("serializability_{index}"),
                wording::SERIALIZABILITY,
                wording::serializability(index),
            );

            if !work.guards.is_empty() {
                request = ask(
                    request,
                    format!("ordering_{index}"),
                    wording::ORDERING,
                    wording::ordering(index),
                );
            }
        }
    }

    // Read only where the requirement they refine is proposed; asked
    // now because another question costs little and another request
    // costs a round trip.
    if enumerated.input_key.is_some() {
        if enumerated.request_triggered {
            request = ask(
                request,
                "result_replay".to_string(),
                wording::RESULT_REPLAY,
                wording::result_replay(),
            );
        }

        request = ask(
            request,
            "guaranteed_completion".to_string(),
            wording::GUARANTEED_COMPLETION,
            wording::guaranteed_completion(),
        );
    }

    request
}

// ---------------------------------------------------------------------
// Decision
// ---------------------------------------------------------------------

fn decide(
    enumerated: &Enumerated,
    decision: &Decision,
    policy: &DiscoveryPolicy,
    adopts_implied: bool,
    evidence: &[EvidenceRef],
) -> Result<Vec<RequirementSubmission>, Abstention> {
    let candidates = candidates(enumerated);

    let mut origins: BTreeMap<Candidate, RequirementOrigin> = BTreeMap::new();

    // Further obligations discharged by a requirement another obligation
    // already maps to. Each is submitted again after the first, with its
    // own origin: the gate records it as a duplicate of the requirement
    // just adopted and maps the obligation to it.
    let mut also: Vec<(Candidate, RequirementOrigin)> = Vec::new();

    // Pairings that were neither clearly asked for nor clearly not.
    // Settled at the end: one is harmless when its requirement is
    // proposed anyway — by another obligation, or as stated by the
    // prompt — because nothing the obligation might need is then
    // dropped, only the provenance left narrower. Otherwise it is a
    // requirement that might be needed and would not be proposed, and
    // the task is escalated.
    let mut unsure: Vec<(Candidate, String)> = Vec::new();

    // Explicit obligations first: each must map to at least one
    // enumerated requirement, or the run cannot succeed without the
    // session. It maps to every requirement it clearly asks for; any
    // pairing that is unclear either way is escalated, never guessed.
    for (index, (id, obligation)) in enumerated.obligations.iter().enumerate() {
        let mut asked_for = Vec::new();

        for (candidate, _) in &candidates {
            let question = format!("obligation_{index}_{}", candidate.option());

            let probability = decision
                .noul(&question)
                .ok_or_else(|| Abstention::because(format!("`{question}` went unanswered")))?;

            match policy.band(probability) {
                Band::Stated => asked_for.push(candidate.clone()),
                Band::NotStated => {}
                Band::Uncertain => unsure.push((
                    candidate.clone(),
                    format!(
                        "it is uncertain whether the obligation \"{}\" asks for {} \
                         ({probability:.2})",
                        obligation.normalized_intent,
                        candidate.option()
                    ),
                )),
            }
        }

        if asked_for.is_empty() {
            return Err(Abstention::because(format!(
                "the obligation \"{}\" maps to no enumerated requirement",
                obligation.normalized_intent
            ))
            .with_findings(vec![format!(
                "none of {} is what it asks for",
                candidates
                    .iter()
                    .map(|(candidate, _)| candidate.option())
                    .collect::<Vec<_>>()
                    .join(", ")
            )]));
        }

        for candidate in asked_for {
            let origin = RequirementOrigin::ExplicitPrompt {
                obligation: id.clone(),
            };

            match origins.entry(candidate) {
                std::collections::btree_map::Entry::Occupied(taken) => {
                    also.push((taken.key().clone(), origin));
                }

                std::collections::btree_map::Entry::Vacant(free) => {
                    free.insert(origin);
                }
            }
        }
    }

    if adopts_implied {
        let stated = |question: &str, asks: &str| -> Result<bool, Abstention> {
            let probability = decision
                .noul(question)
                .ok_or_else(|| Abstention::because(format!("`{question}` went unanswered")))?;

            match policy.band(probability) {
                Band::Stated => Ok(true),
                Band::NotStated => Ok(false),
                Band::Uncertain => Err(Abstention::because(format!(
                    "it is uncertain whether the prompt requires {asks} ({probability:.2})"
                ))),
            }
        };

        let implied = |question: &str, probability: f64| RequirementOrigin::StronglyImplied {
            rationale: format!(
                "System One judged that the prompt states this requirement: `{question}` = \
                 {probability:.2}."
            ),
            evidence: evidence.to_vec(),
        };

        let mut consider = |candidate: Option<Candidate>, question: String, asks: &str| {
            // An explicit obligation already asks for it: whether the
            // prompt also implies it changes nothing, and uncertainty
            // on a branch that is not taken is ignored.
            if candidate
                .as_ref()
                .is_some_and(|candidate| origins.contains_key(candidate))
            {
                return Ok(());
            }

            if !stated(&question, asks)? {
                return Ok(());
            }

            // Stated, and code enumerated nothing that could express
            // it: a key the model declares nowhere, for instance.
            let Some(candidate) = candidate else {
                return Err(Abstention::because(format!(
                    "the prompt requires {asks}, and nothing enumerated can express it"
                )));
            };

            let probability = decision.noul(&question).unwrap_or(0.0);

            origins
                .entry(candidate)
                .or_insert_with(|| implied(&question, probability));

            Ok(())
        };

        let keyed = enumerated.input_key.is_some();

        consider(
            keyed.then_some(Candidate::Idempotency),
            "idempotency".to_string(),
            "idempotency",
        )?;

        consider(
            keyed.then_some(Candidate::Recoverability),
            "recoverability".to_string(),
            "recoverability",
        )?;

        for (index, work) in enumerated.work.iter().enumerate() {
            consider(
                (!work.keys.is_empty()).then_some(Candidate::Serializability(index)),
                format!("serializability_{index}"),
                "serializability",
            )?;

            if !work.guards.is_empty() {
                consider(
                    Some(Candidate::Ordering(index)),
                    format!("ordering_{index}"),
                    "ordering",
                )?;
            }
        }
    }

    if let Some((_, why)) = unsure
        .into_iter()
        .find(|(candidate, _)| !origins.contains_key(candidate))
    {
        return Err(Abstention::because(why));
    }

    origins
        .into_iter()
        .chain(also)
        .map(|(candidate, origin)| {
            Ok(RequirementSubmission {
                requirement: requirement(enumerated, &candidate, decision, policy)?,
                origin,
            })
        })
        .collect()
}

fn requirement(
    enumerated: &Enumerated,
    candidate: &Candidate,
    decision: &Decision,
    policy: &DiscoveryPolicy,
) -> Result<ProposedRequirement, Abstention> {
    let input_key = || {
        enumerated
            .input_key
            .clone()
            .ok_or_else(|| Abstention::because("the triggering input declares no identity"))
    };

    // A refinement is read only here, where the requirement it refines
    // is proposed; its uncertainty elsewhere is ignored.
    let refined = |question: &str| {
        decision
            .noul(question)
            .is_some_and(|noul| noul >= policy.act)
    };

    Ok(match candidate {
        Candidate::Idempotency => ProposedRequirement::Idempotency(IdempotencyRequirement {
            key: input_key()?,
            result: if refined("result_replay") {
                ResultReplayRequirement::ReplayConsistent
            } else {
                ResultReplayRequirement::Unspecified
            },
        }),

        Candidate::Recoverability => {
            ProposedRequirement::Recoverability(RecoverabilityRequirement {
                key: input_key()?,
                completion: if refined("guaranteed_completion") {
                    CompletionRequirement::Guaranteed
                } else {
                    CompletionRequirement::Resumable
                },
            })
        }

        Candidate::Serializability(index) => {
            let work = &enumerated.work[*index];

            ProposedRequirement::TransactionSerializability {
                transaction: work.transaction.clone(),
                requirement: TransactionSerializabilityRequirement {
                    key: serializability_key(*index, work, decision, policy)?,
                },
            }
        }

        Candidate::Ordering(index) => {
            let work = &enumerated.work[*index];

            // One guard fixes both; several would need a judgment no
            // question here makes.
            let [(key, position)] = work.guards.as_slice() else {
                return Err(Abstention::because(format!(
                    "`{}` guards {} positions; which one orders it is not enumerated",
                    work.transaction,
                    work.guards.len()
                )));
            };

            ProposedRequirement::TransactionOrdering {
                transaction: work.transaction.clone(),
                requirement: TransactionOrderingRequirement {
                    key: key.clone(),
                    position: position.clone(),
                },
            }
        }
    })
}

fn serializability_key(
    index: usize,
    work: &Work,
    decision: &Decision,
    policy: &DiscoveryPolicy,
) -> Result<ValueRef, Abstention> {
    if let [only] = work.keys.as_slice() {
        return Ok(only.clone());
    }

    let (choice, probabilities) = decision
        .choice(&format!("serializability_key_{index}"))
        .ok_or_else(|| Abstention::because("a key choice went unanswered"))?;

    let probability = probabilities.get(choice).copied().unwrap_or(0.0);

    key_options(work)
        .iter()
        .position(|(option, _)| option == choice)
        .filter(|_| probability >= policy.select)
        .map(|position| work.keys[position].clone())
        .ok_or_else(|| {
            Abstention::because(format!(
                "no enumerated value clearly keys the serializability of `{}` (chose `{choice}` \
                 at {probability:.2})",
                work.transaction
            ))
        })
}

fn describe(proposal: &RequirementSubmission) -> String {
    let origin = match &proposal.origin {
        RequirementOrigin::ExplicitPrompt { obligation } => {
            format!("for obligation {}", obligation.0)
        }
        RequirementOrigin::StronglyImplied { .. } => "as strongly implied".to_string(),
        RequirementOrigin::Recommended { .. } => "as a recommendation".to_string(),
    };

    let what = match &proposal.requirement {
        ProposedRequirement::TransactionSerializability {
            transaction,
            requirement,
        } => format!(
            "SerializableBy({}) on {transaction}",
            label(&requirement.key)
        ),

        ProposedRequirement::TransactionOrdering {
            transaction,
            requirement,
        } => format!(
            "OrderedBy({}, {}) on {transaction}",
            label(&requirement.key),
            label(&requirement.position)
        ),

        ProposedRequirement::Idempotency(requirement) => format!(
            "idempotency by ({})",
            requirement
                .key
                .components
                .iter()
                .map(label)
                .collect::<Vec<_>>()
                .join(", ")
        ),

        ProposedRequirement::Recoverability(requirement) => format!(
            "recoverability by ({})",
            requirement
                .key
                .components
                .iter()
                .map(label)
                .collect::<Vec<_>>()
                .join(", ")
        ),
    };

    format!("{what} {origin}")
}
