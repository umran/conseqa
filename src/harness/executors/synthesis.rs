//! The operation synthesis builder: programs by archetype (§20 of the
//! System One orchestration revision).
//!
//! An archetype is a typed program template. Code enumerates what the
//! templates could be filled with — the records the operation's input
//! identifies, their lifecycle transitions, the errors its result
//! declares — and fills the chosen template with deterministic ids. A
//! System One decider is asked, in one request, which template matches
//! what the operation is described to do, and which of the enumerated
//! pieces it acts on. It writes nothing.
//!
//! The filled program is judged by the analyzer before it is submitted
//! ([`evaluate_candidate`](crate::confluence::ConfluenceEngine::evaluate_candidate)):
//! a program that does not validate is never submitted, and the task
//! goes to a session with the diagnostics. A program says what an
//! operation does, which only the description can settle, so every
//! judgment here is gated — a template or a piece chosen below
//! `select`, or a changed field judged in between the bands, is
//! escalated rather than guessed.
//!
//! The seed catalogue, drawn from the fixtures and the benchmark:
//!
//! - **keyed update** — one transaction reads and writes fields of one
//!   record the input identifies;
//! - **keyed insert** — one transaction inserts one record whose
//!   identity the input carries;
//! - **transition** — one transaction applies one lifecycle transition,
//!   without side effects, to a record the input identifies; a request
//!   returns a declared error when the record is in the wrong state.
//!
//! Each ends with a `return` of the request's `ok` result, or
//! `complete` for a subscription. What the requirements of the program
//! are, and how they are proven — isolation, locks, keyed commits — is
//! discovery's and repair's to decide afterwards.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::json;
use uuid::Uuid;

use crate::confluence::sketch;
use crate::confluence::{
    AssemblyGap, BundleSpec, CommitRequest, DraftOperation, EngineError, Mutation, PatchId,
    SearchSpec, SpecPatch, SymbolKey, SymbolKind, WriteGrant,
};
use crate::spec::{
    Branch, BumpVersion, Condition, DataObject, Derivation, EstablishTransactionOutput,
    ExecuteTransaction, FieldPath, FieldSelection, Id, IdempotencyGuarantee, IdempotencyKey, Input,
    Insert, Literal, MessageSelector, ObjectSelector, OperationBlock, OperationStep, Read,
    RequestIdentity, ResultOutcome, Return, Schema, SelectorPredicate, SelectorValue, StateMachine,
    StateMachineSubject, StateTransition, Transaction, TransactionIsolation, TransactionStep,
    ValueRef, ValueSource, Write,
};
use crate::system_one::DecisionRequest;
use crate::system_one::questions::synthesis as wording;

use super::{Abstention, BuildContext, Built};

/// What synthesis acts on. Provisional, as every threshold is (§13.3).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SynthesisPolicy {
    /// At or above, a field is changed.
    pub act: f64,

    /// At or below, it is not. Between the two the builder abstains.
    pub dismiss: f64,

    /// The least probability at which a chosen template or piece is
    /// accepted.
    pub select: f64,
}

impl Default for SynthesisPolicy {
    fn default() -> Self {
        Self {
            act: 0.8,
            dismiss: 0.25,
            select: 0.6,
        }
    }
}

pub(super) async fn build(context: &BuildContext<'_>, policy: &SynthesisPolicy) -> Built {
    match synthesize(context, policy).await {
        Ok(built) => built,

        Err(error) => Built::Abstained(Abstention::because(format!(
            "the engine refused a read: {error}"
        ))),
    }
}

fn abstain(reason: impl Into<String>) -> Built {
    Built::Abstained(Abstention::because(reason))
}

/// A record the operation's input identifies.
#[derive(Debug, Clone)]
struct Record {
    data_model: Id,
    object: Id,
    data: DataObject,

    /// The record's fields, from its schema, by name.
    fields: BTreeSet<String>,

    /// Selects the one instance the input identifies.
    selector: ObjectSelector,
}

impl Record {
    fn identity_names(&self) -> BTreeSet<String> {
        self.data
            .identity
            .iter()
            .filter_map(|path| path.0.last().cloned())
            .collect()
    }

    fn version_field(&self) -> Option<String> {
        self.data
            .version
            .as_ref()
            .and_then(|version| version.field.0.last().cloned())
    }

    /// The fields an update could change: neither identity nor the
    /// version, which only the version protocol advances.
    fn changeable(&self) -> Vec<String> {
        let identity = self.identity_names();
        let version = self.version_field();

        self.fields
            .iter()
            .filter(|field| !identity.contains(*field) && Some(*field) != version.as_ref())
            .cloned()
            .collect()
    }
}

/// A side-effect-free transition of a record the input identifies.
#[derive(Debug, Clone)]
struct Lifecycle {
    machine: Id,
    transition: Id,
    from: BTreeSet<Id>,
    to: Id,
    record: usize,

    /// The record's field holding its lifecycle state.
    state: FieldPath,
}

impl Lifecycle {
    fn option(&self) -> String {
        format!("{}.{}", self.machine, self.transition).replace(['.', '-'], "_")
    }
}

/// Everything code found that a template could be filled with.
struct Enumerated {
    operation: Id,
    draft: DraftOperation,
    input: Id,
    request: bool,

    /// The fields the input carries, by name.
    carries: BTreeSet<String>,

    ok: Option<(Id, BTreeSet<String>)>,

    /// The request's declared errors: class, schema fields.
    errors: BTreeMap<Id, BTreeSet<String>>,

    /// The request's declared identity, as a commit key, when it has
    /// one: what lets a retry recover rather than re-decide.
    identity: Option<IdempotencyKey>,

    records: Vec<Record>,
    lifecycles: Vec<Lifecycle>,
}

async fn synthesize(
    context: &BuildContext<'_>,
    policy: &SynthesisPolicy,
) -> Result<Built, EngineError> {
    let task = context.engine.task_context(context.task)?;

    let Some(operation) = task
        .write_scope
        .grants
        .iter()
        .find_map(|grant| match grant {
            WriteGrant::OperationProgram(operation) => Some(operation.clone()),
            _ => None,
        })
    else {
        return Ok(abstain("the task's scope names no program to write"));
    };

    // The author's sketch, when there is one, is compiled; templates are
    // for operations without one.
    let interface = context.engine.read_symbol(
        context.task,
        &SymbolKey::OperationInterface(operation.clone()),
    )?;

    if let Ok(interface) =
        serde_json::from_value::<crate::confluence::OperationInterfaceDraft>(interface.content)
        && interface.sketch.is_some()
    {
        let prompt: String = task
            .prompt_evidence
            .iter()
            .map(|evidence| evidence.excerpt.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");

        return from_sketch(
            context,
            policy,
            task.snapshot_revision,
            &operation,
            &DraftOperation::planned(interface),
            &prompt,
        )
        .await;
    }

    let enumerated = match enumerate(context, &operation)? {
        Ok(enumerated) => enumerated,
        Err(reason) => return Ok(abstain(reason)),
    };

    let offered = offered(&enumerated);

    if offered.is_empty() {
        return Ok(abstain(
            "no archetype applies: the input identifies no record the catalogue can act on",
        ));
    }

    let prompt: String = task
        .prompt_evidence
        .iter()
        .map(|evidence| evidence.excerpt.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");

    let request = questions(context, &enumerated, &offered, &prompt);

    let decision = match context.decider.decide(&request).await {
        Ok(decision) => decision,
        Err(error) => return Ok(abstain(format!("the decider gave no answer: {error}"))),
    };

    let chosen = |question: &str| -> Result<String, String> {
        let (choice, probabilities) = decision
            .choice(question)
            .ok_or_else(|| format!("`{question}` went unanswered"))?;

        let probability = probabilities.get(choice).copied().unwrap_or(0.0);

        if choice == wording::NONE_OF_THESE || probability < policy.select {
            return Err(format!(
                "`{question}` settled on nothing clearly: `{choice}` at {probability:.2}"
            ));
        }

        Ok(choice.to_string())
    };

    let archetype = match chosen("archetype") {
        Ok(archetype) => archetype,
        Err(reason) => {
            return Ok(abstain(format!(
                "no archetype matches the operation: {reason}"
            )));
        }
    };

    let program = match archetype.as_str() {
        wording::KEYED_UPDATE | wording::KEYED_INSERT => {
            let candidates = record_candidates(&enumerated, &archetype);

            let record = match candidates.as_slice() {
                [only] => *only,
                _ => match chosen("record") {
                    Ok(object) => {
                        match candidates.iter().find(|record| record.object.0 == object) {
                            Some(record) => *record,
                            None => return Ok(abstain(format!("`{object}` is not a candidate"))),
                        }
                    }
                    Err(reason) => return Ok(abstain(reason)),
                },
            };

            if archetype == wording::KEYED_UPDATE {
                let mut changed = BTreeSet::new();

                for field in record.changeable() {
                    let question = changes_id(&record.object, &field);

                    let Some(probability) = decision.noul(&question) else {
                        return Ok(abstain(format!("`{question}` went unanswered")));
                    };

                    if probability >= policy.act {
                        changed.insert(field);
                    } else if probability > policy.dismiss {
                        return Ok(abstain(format!(
                            "it is uncertain whether the operation changes `{field}` of \
                             `{}` ({probability:.2})",
                            record.object
                        )));
                    }
                }

                if changed.is_empty() {
                    return Ok(abstain(format!(
                        "the operation changes no field of `{}` clearly",
                        record.object
                    )));
                }

                keyed_update(&enumerated, record, &changed)
            } else {
                keyed_insert(&enumerated, record)
            }
        }

        wording::TRANSITION_APPLIED => {
            let lifecycle = match chosen("transition") {
                Ok(option) => match enumerated
                    .lifecycles
                    .iter()
                    .find(|lifecycle| lifecycle.option() == option)
                {
                    Some(lifecycle) => lifecycle,
                    None => return Ok(abstain(format!("`{option}` is not a candidate"))),
                },
                Err(reason) => return Ok(abstain(reason)),
            };

            let refusal = if enumerated.request {
                match chosen("refusal") {
                    Ok(error) => Some(Id(error)),
                    Err(reason) => {
                        return Ok(abstain(format!(
                            "no declared error is clearly the one a wrong-state record \
                             returns: {reason}"
                        )));
                    }
                }
            } else {
                None
            };

            transition(&enumerated, lifecycle, refusal.as_ref())
        }

        other => return Ok(abstain(format!("`{other}` is not an archetype"))),
    };

    submit_program(
        context,
        task.snapshot_revision,
        &operation,
        program,
        &format!("a {archetype} program"),
    )
    .await
}

/// Judges a program and submits it: the shared end of the template and
/// sketch paths.
async fn submit_program(
    context: &BuildContext<'_>,
    base_revision: crate::spec::Revision,
    operation: &Id,
    program: OperationBlock,
    what: &str,
) -> Result<Built, EngineError> {
    let patch = SpecPatch {
        mutations: vec![Mutation::ReplaceOperationProgram {
            operation: operation.clone(),
            program,
        }],
    };

    let verdict = context
        .engine
        .evaluate_candidate(context.task, &patch, &[])
        .await?;

    // Mid-fanout the model cannot assemble: sibling operations have no
    // program yet. What can be judged then is exactly what the gate
    // judges of any session's program — the draft checks, which run
    // the validator's operation-local passes (§8.1) — and it is judged
    // again at commit. Whole-model verification follows once every
    // program is in.
    let only_siblings_missing = verdict.scope_violation.is_none()
        && verdict.draft_diagnostics.is_empty()
        && !verdict.assembly_gaps.is_empty()
        && verdict.assembly_gaps.iter().all(|gap| {
            matches!(gap, AssemblyGap::MissingProgram { operation: sibling } if sibling != operation)
        });

    if verdict.verified().is_none() && !only_siblings_missing {
        return Ok(Built::Abstained(
            Abstention::because(format!("{what} was refused")).with_findings(vec![
                verdict
                    .refusal()
                    .unwrap_or_else(|| "it was not verified".to_string()),
            ]),
        ));
    }

    let outcome = context
        .engine
        .submit(CommitRequest {
            task: context.task,
            patch_id: PatchId::fresh(),
            base_revision,
            patch,
            client_nonce: Uuid::new_v4(),
        })
        .await?;

    Ok(match outcome {
        Ok(_) => Built::Committed {
            summary: format!("{operation}: {what}"),
        },

        Err(rejection) if rejection.is_stale_context() => Built::Stale,

        Err(rejection) => Built::Abstained(
            Abstention::because("the gate rejected a program the analyzer had admitted")
                .with_findings(vec![format!("{rejection:?}")]),
        ),
    })
}

// ---------------------------------------------------------------------
// Sketches
// ---------------------------------------------------------------------

/// Everything a sketch compiles against, read through the task so that
/// each read is tracked.
fn tracked_symbols(context: &BuildContext<'_>) -> Result<sketch::Symbols, EngineError> {
    let mut symbols = sketch::Symbols::default();

    let all = |kind| {
        context.engine.search_symbols(
            context.task,
            &SearchSpec {
                kind: Some(kind),
                ..Default::default()
            },
        )
    };

    for key in all(SymbolKind::DataObject)? {
        if let SymbolKey::DataObject { data_model, object } = &key
            && let Ok(data) = serde_json::from_value::<DataObject>(
                context.engine.read_symbol(context.task, &key)?.content,
            )
        {
            symbols
                .objects
                .insert(object.clone(), (data_model.clone(), data));
        }
    }

    for key in all(SymbolKind::Schema)? {
        if let SymbolKey::Schema(id) = &key
            && let Ok(schema) = serde_json::from_value::<Schema>(
                context.engine.read_symbol(context.task, &key)?.content,
            )
        {
            symbols
                .schemas
                .insert(id.clone(), sketch::schema_fields(&schema));
        }
    }

    for key in all(SymbolKind::StateMachine)? {
        if let SymbolKey::StateMachine(id) = &key
            && let Ok(machine) = serde_json::from_value::<StateMachine>(
                context.engine.read_symbol(context.task, &key)?.content,
            )
        {
            symbols.machines.insert(id.clone(), machine);
        }
    }

    Ok(symbols)
}

/// The program, in words: what a fidelity judgment compares with the
/// description.
fn in_words(program: &OperationBlock, symbols: &sketch::Symbols) -> Vec<String> {
    let mut words: Vec<String> = program
        .transactions()
        .into_iter()
        .filter_map(|(_, transaction)| super::describe::summarize(transaction))
        .collect();

    // The facts behind the guarantees a description states, so a
    // guarantee the program keeps is not read as work it leaves out.
    for (_, transaction) in program.transactions() {
        if let IdempotencyGuarantee::DeduplicatedBy { key } = &transaction.idempotency {
            words.push(format!(
                "a retry with the same {} resolves the earlier commit instead of acting again",
                key.components
                    .iter()
                    .map(|component| format!("`{}`", component.path.0.join(".")))
                    .collect::<Vec<_>>()
                    .join(" and ")
            ));
        }

        for step in &transaction.steps {
            // What a transition sends, by its declaration.
            if let TransactionStep::Transition(applied) = step
                && let Some(declared) = symbols
                    .machines
                    .get(&applied.machine)
                    .and_then(|machine| machine.transitions.get(&applied.transition))
            {
                for effect in declared.effects.values() {
                    let crate::spec::TransitionEffect::OutboxWrite(write) = effect;

                    words.push(format!(
                        "applying `{}` admits a `{}` message to `{}`",
                        applied.transition, write.schema, write.outbox
                    ));
                }

                for effect in declared.side_effects.values() {
                    if let crate::spec::TransitionSideEffect::Publication(publication) = effect {
                        words.push(format!(
                            "applying `{}` publishes a `{}` message to `{}`",
                            applied.transition, publication.schema, publication.topic
                        ));
                    }
                }
            }

            if let TransactionStep::Insert(insert) = step
                && let Some((_, data)) = symbols.objects.get(&insert.object)
            {
                words.push(format!(
                    "a `{}` is identified by {}, so a second one with the same values cannot \
                     be created",
                    insert.object,
                    data.identity
                        .iter()
                        .map(|field| format!("`{}`", field.0.join(".")))
                        .collect::<Vec<_>>()
                        .join(" and ")
                ));
            }
        }
    }

    words.dedup();

    fn outcomes(block: &OperationBlock, into: &mut BTreeSet<String>) {
        for step in &block.steps {
            match step {
                OperationStep::Return(Return {
                    outcome: ResultOutcome::Err { error, .. },
                    ..
                }) => {
                    into.insert(format!("returns the error `{error}`"));
                }
                OperationStep::Abandon => {
                    into.insert(
                        "leaves the message for a later attempt, without completing it"
                            .to_string(),
                    );
                }
                OperationStep::Branch(branch) => {
                    outcomes(&branch.then, into);
                    if let Some(otherwise) = &branch.otherwise {
                        outcomes(otherwise, into);
                    }
                }
                OperationStep::Transaction(execute) => {
                    if let Some(rejected) = &execute.rejected {
                        outcomes(rejected, into);
                    }
                }
                OperationStep::MatchResult(matched) => {
                    outcomes(&matched.ok, into);
                    for arm in matched.errors.values() {
                        outcomes(arm, into);
                    }
                }
                _ => {}
            }
        }
    }

    let mut errors = BTreeSet::new();

    outcomes(program, &mut errors);

    words.extend(errors);

    words
}

/// The sketch path: the program is compiled from what its author
/// wrote. A decider settles the one choice a sketch may leave open — a
/// request's refusal error — and guards against a sketch that plainly
/// contradicts its description; it writes nothing.
async fn from_sketch(
    context: &BuildContext<'_>,
    policy: &SynthesisPolicy,
    base_revision: crate::spec::Revision,
    operation: &Id,
    draft: &DraftOperation,
    prompt: &str,
) -> Result<Built, EngineError> {
    let symbols = tracked_symbols(context)?;

    let open = match sketch::open_choices(operation, draft, &symbols) {
        Ok(open) => open,
        Err(error) => return Ok(abstain(format!("the sketch does not compile: {error}"))),
    };

    // A draft compile first: the fidelity question shows what it does.
    let provisional = sketch::Settled {
        refusal: open.iter().find_map(|choice| match choice {
            sketch::Open::Refusal { errors, .. } => errors.first().cloned(),
            sketch::Open::CursorRule { .. } => None,
        }),
        cursor_rule: Some(crate::spec::CursorAdvanceRule::MonotonicAfter),
    };

    let program = match sketch::compile(operation, draft, &symbols, &provisional) {
        Ok(program) => program,
        Err(error) => return Ok(abstain(format!("the sketch does not compile: {error}"))),
    };

    let mut request = DecisionRequest::new(json!({
        "prompt": prompt,
        "operation": { "id": operation, "description": draft.description },
        "program": in_words(&program, &symbols),
        "errors": open.iter().flat_map(|choice| match choice {
            sketch::Open::Refusal { errors, .. } => errors.clone(),
            sketch::Open::CursorRule { .. } => Vec::new(),
        }).collect::<Vec<_>>(),
        "work": program
            .transactions()
            .into_iter()
            .filter_map(|(_, transaction)| super::describe::summarize(transaction))
            .collect::<Vec<_>>(),
    }))
    .tag("task", context.task.to_string())
    .tag("builder", "operation_synthesis")
    .tag("operation", operation.to_string())
    .ask("fidelity", wording::fidelity())
    .tag(
        format!("spec.{}", wording::FIDELITY.id),
        wording::FIDELITY.tag(),
    );

    for choice in &open {
        match choice {
            sketch::Open::Refusal { errors, .. } => {
                let options: Vec<(String, String)> = errors
                    .iter()
                    .map(|class| (class.0.clone(), format!("The error `{class}`.")))
                    .collect();

                request = request.ask("refusal", wording::refusal(&options)).tag(
                    format!("spec.{}", wording::REFUSAL.id),
                    wording::REFUSAL.tag(),
                );
            }

            sketch::Open::CursorRule { .. } => {
                use crate::system_one::questions::repair as preference;

                request = request.ask("gap_free", preference::gap_free()).tag(
                    format!("spec.{}", preference::GAP_FREE.id),
                    preference::GAP_FREE.tag(),
                );
            }
        }
    }

    let decision = match context.decider.decide(&request).await {
        Ok(decision) => Some(decision),

        // The sketch is its author's explicit statement and compiles
        // deterministically: an unavailable decider blocks only a refusal
        // the sketch left open.
        Err(error)
            if open
                .iter()
                .any(|choice| matches!(choice, sketch::Open::Refusal { .. })) =>
        {
            return Ok(abstain(format!("the decider gave no answer: {error}")));
        }

        Err(_) => None,
    };

    if let Some(decision) = &decision
        && let Some(probability) = decision.noul("fidelity")
        && probability <= policy.dismiss
    {
        return Ok(Built::Abstained(
            Abstention::because(format!(
                "the program compiled from the sketch does not do what the operation is \
                 described to do ({probability:.2})"
            ))
            .with_findings(in_words(&program, &symbols)),
        ));
    }

    let mut settled = sketch::Settled::default();

    // A cursor rule is a preference between two shapes that both order
    // what they admit: only a stated "none skipped" moves it off the
    // permissive default, and an unsure answer reorders nothing.
    if open
        .iter()
        .any(|choice| matches!(choice, sketch::Open::CursorRule { .. }))
    {
        let stated = decision
            .as_ref()
            .and_then(|decision| decision.noul("gap_free"))
            .is_some_and(|probability| probability >= policy.act);

        settled.cursor_rule = Some(if stated {
            crate::spec::CursorAdvanceRule::Successor
        } else {
            crate::spec::CursorAdvanceRule::MonotonicAfter
        });
    }

    if open
        .iter()
        .any(|choice| matches!(choice, sketch::Open::Refusal { .. }))
    {
        let Some(decision) = decision.as_ref() else {
            return Ok(abstain("the decider gave no answer for an open refusal"));
        };

        let Some((choice, probabilities)) = decision.choice("refusal") else {
            return Ok(abstain("`refusal` went unanswered"));
        };

        let probability = probabilities.get(choice).copied().unwrap_or(0.0);

        if choice == wording::NONE_OF_THESE || probability < policy.select {
            return Ok(abstain(format!(
                "no declared error is clearly the one a wrong-state record returns: `{choice}` \
                 at {probability:.2}"
            )));
        }

        settled.refusal = Some(Id(choice.to_string()));
    }

    let program = match sketch::compile(operation, draft, &symbols, &settled) {
        Ok(program) => program,
        Err(error) => return Ok(abstain(format!("the sketch does not compile: {error}"))),
    };

    submit_program(
        context,
        base_revision,
        operation,
        program,
        "the program compiled from its sketch",
    )
    .await
}

// ---------------------------------------------------------------------
// Enumeration
// ---------------------------------------------------------------------

fn schema_fields(context: &BuildContext<'_>, schema: &Id) -> Result<BTreeSet<String>, EngineError> {
    let view = context
        .engine
        .read_symbol(context.task, &SymbolKey::Schema(schema.clone()))?;

    Ok(match serde_json::from_value::<Schema>(view.content) {
        Ok(Schema::Canonical(canonical)) => canonical.fields.into_keys().collect(),
        _ => BTreeSet::new(),
    })
}

fn enumerate(
    context: &BuildContext<'_>,
    operation: &Id,
) -> Result<Result<Enumerated, String>, EngineError> {
    let bundle = context.engine.context_bundle(
        context.task,
        &BundleSpec {
            operation: Some(operation.clone()),
            ..Default::default()
        },
    )?;

    let Some(draft) = bundle
        .operation
        .and_then(|draft| serde_json::from_value::<DraftOperation>(draft).ok())
    else {
        return Ok(Err("the operation's draft could not be read".to_string()));
    };

    if draft.program.is_some() {
        return Ok(Err("the operation already has a program".to_string()));
    }

    let [(input_id, input)] = draft.inputs.iter().collect::<Vec<_>>()[..] else {
        return Ok(Err(
            "the catalogue covers operations with exactly one input".to_string(),
        ));
    };

    let (input_id, input) = (input_id.clone(), input.clone());
    let input_id = &input_id;
    let input = &input;

    let mut identity = None;

    let (request, input_schema, ok, errors) = match input {
        Input::Request(request) => {
            let mut errors = BTreeMap::new();

            for (class, error) in &request.result.errors {
                errors.insert(class.clone(), schema_fields(context, &error.schema)?);
            }

            if let RequestIdentity::Keyed(key) = &request.identity {
                identity = Some(IdempotencyKey {
                    components: key
                        .fields
                        .iter()
                        .map(|field| ValueRef {
                            source: ValueSource::Input(input_id.clone()),
                            path: field.clone(),
                        })
                        .collect(),
                });
            }

            (
                true,
                request.schema.clone(),
                Some((
                    request.result.ok.clone(),
                    schema_fields(context, &request.result.ok)?,
                )),
                errors,
            )
        }

        Input::Subscription(subscription) => {
            let MessageSelector::Only(schemas) = &subscription.messages else {
                return Ok(Err(
                    "the subscription consumes every message of its topic".to_string()
                ));
            };

            let [schema] = schemas.iter().collect::<Vec<_>>()[..] else {
                return Ok(Err(
                    "the subscription consumes more than one message schema".to_string(),
                ));
            };

            (false, schema.clone(), None, BTreeMap::new())
        }

        Input::Outbox(_) => {
            return Ok(Err("outbox consumers are not in the catalogue".to_string()));
        }
    };

    let carries = schema_fields(context, &input_schema)?;

    let mut records = Vec::new();

    for key in context.engine.search_symbols(
        context.task,
        &SearchSpec {
            kind: Some(SymbolKind::DataObject),
            ..Default::default()
        },
    )? {
        let SymbolKey::DataObject { data_model, object } = &key else {
            continue;
        };

        let view = context.engine.read_symbol(context.task, &key)?;

        let Ok(data) = serde_json::from_value::<DataObject>(view.content) else {
            continue;
        };

        // The input identifies the record when it carries every
        // identity field under the same name.
        let Some(predicates) = data
            .identity
            .iter()
            .map(|path| {
                let name = path.0.last()?;

                carries.contains(name).then(|| SelectorPredicate::Eq {
                    field: path.clone(),
                    value: SelectorValue::Value(input_ref(input_id, name)),
                })
            })
            .collect::<Option<Vec<_>>>()
        else {
            continue;
        };

        let predicate = match <[_; 1]>::try_from(predicates) {
            Ok([only]) => only,
            Err(many) if !many.is_empty() => SelectorPredicate::And { predicates: many },
            Err(_) => continue,
        };

        let fields = schema_fields(context, &data.schema)?;

        records.push(Record {
            data_model: data_model.clone(),
            object: object.clone(),
            selector: ObjectSelector {
                object: object.clone(),
                predicate,
            },
            data,
            fields,
        });
    }

    let mut lifecycles = Vec::new();

    for key in context.engine.search_symbols(
        context.task,
        &SearchSpec {
            kind: Some(SymbolKind::StateMachine),
            ..Default::default()
        },
    )? {
        let SymbolKey::StateMachine(machine) = &key else {
            continue;
        };

        let view = context.engine.read_symbol(context.task, &key)?;

        let Ok(state_machine) = serde_json::from_value::<StateMachine>(view.content) else {
            continue;
        };

        let StateMachineSubject::Object { object, state } = &state_machine.subject;

        let Some(record) = records.iter().position(|record| &record.object == object) else {
            continue;
        };

        for (id, transition) in &state_machine.transitions {
            // A transition with side effects or outbox effects needs
            // derivations the catalogue does not write.
            if !transition.side_effects.is_empty() || !transition.effects.is_empty() {
                continue;
            }

            lifecycles.push(Lifecycle {
                machine: machine.clone(),
                transition: id.clone(),
                from: transition.from.clone(),
                to: transition.to.clone(),
                record,
                state: state.clone(),
            });
        }
    }

    Ok(Ok(Enumerated {
        operation: operation.clone(),
        draft,
        input: input_id.clone(),
        request,
        carries,
        ok,
        errors,
        identity,
        records,
        lifecycles,
    }))
}

/// The records each archetype could act on. An insert needs its whole
/// identity from the input; an update, something to change.
fn record_candidates<'a>(enumerated: &'a Enumerated, archetype: &str) -> Vec<&'a Record> {
    enumerated
        .records
        .iter()
        .filter(|record| match archetype {
            wording::KEYED_UPDATE => !record.changeable().is_empty(),
            _ => true,
        })
        .collect()
}

fn offered(enumerated: &Enumerated) -> Vec<&'static str> {
    let mut offered = Vec::new();

    if !record_candidates(enumerated, wording::KEYED_UPDATE).is_empty() {
        offered.push(wording::KEYED_UPDATE);
    }

    if !enumerated.records.is_empty() {
        offered.push(wording::KEYED_INSERT);
    }

    if !enumerated.lifecycles.is_empty() && (!enumerated.request || !enumerated.errors.is_empty()) {
        offered.push(wording::TRANSITION_APPLIED);
    }

    offered
}

fn changes_id(object: &Id, field: &str) -> String {
    format!("changes_{}_{field}", object.0.replace(['.', '-'], "_"))
}

// ---------------------------------------------------------------------
// Questions
// ---------------------------------------------------------------------

fn questions(
    context: &BuildContext<'_>,
    enumerated: &Enumerated,
    offered: &[&str],
    prompt: &str,
) -> DecisionRequest {
    let state = json!({
        "prompt": prompt,
        "operation": {
            "id": enumerated.operation,
            "description": enumerated.draft.description,
            "input_carries": enumerated.carries,
        },
        "records": enumerated
            .records
            .iter()
            .map(|record| (record.object.0.clone(), json!({ "fields": record.fields })))
            .collect::<BTreeMap<_, _>>(),
        "transitions": enumerated
            .lifecycles
            .iter()
            .map(|lifecycle| (lifecycle.option(), json!({
                "record": enumerated.records[lifecycle.record].object,
                "from": lifecycle.from,
                "to": lifecycle.to,
            })))
            .collect::<BTreeMap<_, _>>(),
        "errors": enumerated.errors.keys().collect::<Vec<_>>(),
    });

    let tag = |request: DecisionRequest, spec: crate::system_one::QuestionSpec| {
        request.tag(format!("spec.{}", spec.id), spec.tag())
    };

    let mut request = DecisionRequest::new(state)
        .tag("task", context.task.to_string())
        .tag("builder", "operation_synthesis")
        .tag("operation", enumerated.operation.to_string())
        .ask("archetype", wording::archetype(offered));

    request = tag(request, wording::ARCHETYPE);

    let updates = offered.contains(&wording::KEYED_UPDATE);
    let inserts = offered.contains(&wording::KEYED_INSERT);

    // Asked speculatively: another question costs little, another
    // request a round trip. Read only for the archetype chosen.
    let candidates: BTreeSet<&Id> = [
        (updates, wording::KEYED_UPDATE),
        (inserts, wording::KEYED_INSERT),
    ]
    .into_iter()
    .filter(|(offered, _)| *offered)
    .flat_map(|(_, archetype)| record_candidates(enumerated, archetype))
    .map(|record| &record.object)
    .collect();

    if candidates.len() > 1 {
        request = tag(
            request.ask(
                "record",
                wording::record(&candidates.iter().map(|id| id.0.clone()).collect::<Vec<_>>()),
            ),
            wording::RECORD,
        );
    }

    if updates {
        for record in record_candidates(enumerated, wording::KEYED_UPDATE) {
            for field in record.changeable() {
                request = tag(
                    request.ask(
                        changes_id(&record.object, &field),
                        wording::changes(&record.object.0, &field),
                    ),
                    wording::CHANGES,
                );
            }
        }
    }

    if offered.contains(&wording::TRANSITION_APPLIED) {
        let transitions: Vec<(String, String)> = enumerated
            .lifecycles
            .iter()
            .map(|lifecycle| {
                (
                    lifecycle.option(),
                    format!(
                        "Moves `{}` from {} to `{}`.",
                        enumerated.records[lifecycle.record].object,
                        lifecycle
                            .from
                            .iter()
                            .map(|state| format!("`{state}`"))
                            .collect::<Vec<_>>()
                            .join(" or "),
                        lifecycle.to
                    ),
                )
            })
            .collect();

        request = tag(
            request.ask("transition", wording::transition(&transitions)),
            wording::TRANSITION,
        );

        if enumerated.request {
            let errors: Vec<(String, String)> = enumerated
                .errors
                .keys()
                .map(|class| (class.0.clone(), format!("The error `{class}`.")))
                .collect();

            request = tag(
                request.ask("refusal", wording::refusal(&errors)),
                wording::REFUSAL,
            );
        }
    }

    request
}

// ---------------------------------------------------------------------
// Templates
// ---------------------------------------------------------------------

fn input_ref(input: &Id, field: &str) -> ValueRef {
    ValueRef {
        source: ValueSource::Input(input.clone()),
        path: FieldPath(vec![field.to_string()]),
    }
}

/// The operation's short name, for the ids a template introduces.
fn short(operation: &Id) -> &str {
    operation
        .0
        .strip_prefix("operation.")
        .unwrap_or(&operation.0)
}

fn record_short(object: &Id) -> &str {
    object.0.strip_prefix("object.").unwrap_or(&object.0)
}

fn every_input_field(enumerated: &Enumerated) -> Vec<ValueRef> {
    enumerated
        .carries
        .iter()
        .map(|field| input_ref(&enumerated.input, field))
        .collect()
}

/// The values of a result or error payload: every field the input
/// carries under the payload's own name, else the input's identity.
fn payload_from(
    enumerated: &Enumerated,
    fields: &BTreeSet<String>,
    output: Option<&Id>,
) -> Derivation {
    let mut from: Vec<ValueRef> = fields
        .iter()
        .filter_map(|field| {
            if let Some(output) = output {
                return Some(ValueRef {
                    source: ValueSource::TransactionOutput(output.clone()),
                    path: FieldPath(vec![field.clone()]),
                });
            }

            enumerated
                .carries
                .contains(field)
                .then(|| input_ref(&enumerated.input, field))
        })
        .collect();

    if from.is_empty() {
        from = every_input_field(enumerated);
    }

    Derivation::Deterministic { from }
}

fn transaction(id: String, record: &Record, steps: Vec<TransactionStep>) -> Transaction {
    Transaction {
        id: Id(id),
        data_model: Some(record.data_model.clone()),
        isolation: TransactionIsolation::ReadCommitted,
        idempotency: IdempotencyGuarantee::Unspecified,
        requirements: Default::default(),
        steps,
    }
}

/// The end of the program: the request's `ok` result, or `complete`.
fn finish(enumerated: &Enumerated, output: Option<&Id>) -> OperationStep {
    match &enumerated.ok {
        Some((_, fields)) if enumerated.request => OperationStep::Return(Return {
            request: enumerated.input.clone(),
            outcome: ResultOutcome::Ok {
                values: payload_from(enumerated, fields, output),
            },
        }),

        _ => OperationStep::Complete,
    }
}

/// When the result carries a field of the record the input does not,
/// it can only come from inside the transaction: export it.
fn output_for(
    enumerated: &Enumerated,
    record: &Record,
    read: Option<&Id>,
) -> Option<(Id, TransactionStep)> {
    let (schema, fields) = enumerated.ok.as_ref().filter(|_| enumerated.request)?;

    let from_record: Vec<&String> = fields
        .iter()
        .filter(|field| !enumerated.carries.contains(*field) && record.fields.contains(*field))
        .collect();

    let read = read?;

    if from_record.is_empty() {
        return None;
    }

    let bind = Id(format!("output.{}.result", short(&enumerated.operation)));

    let from = fields
        .iter()
        .map(|field| {
            if enumerated.carries.contains(field) {
                input_ref(&enumerated.input, field)
            } else {
                ValueRef {
                    source: ValueSource::TransactionRead(read.clone()),
                    path: FieldPath(vec![field.clone()]),
                }
            }
        })
        .filter(|value| match &value.source {
            ValueSource::TransactionRead(_) => value
                .path
                .0
                .last()
                .is_some_and(|field| record.fields.contains(field)),
            _ => true,
        })
        .collect();

    Some((
        bind.clone(),
        TransactionStep::EstablishTransactionOutput(EstablishTransactionOutput {
            bind,
            schema: schema.clone(),
            values: Derivation::Deterministic { from },
        }),
    ))
}

fn keyed_update(
    enumerated: &Enumerated,
    record: &Record,
    changed: &BTreeSet<String>,
) -> OperationBlock {
    let name = short(&enumerated.operation);
    let read = Id(format!("read.{name}.{}", record_short(&record.object)));

    let result_fields: BTreeSet<String> = enumerated
        .ok
        .iter()
        .flat_map(|(_, fields)| fields.iter())
        .filter(|field| !enumerated.carries.contains(*field) && record.fields.contains(*field))
        .cloned()
        .collect();

    let observed: BTreeSet<FieldPath> = changed
        .iter()
        .chain(&result_fields)
        .map(|field| FieldPath(vec![field.clone()]))
        .collect();

    let identity = record.identity_names();

    let mut from: Vec<ValueRef> = changed
        .iter()
        .map(|field| ValueRef {
            source: ValueSource::TransactionRead(read.clone()),
            path: FieldPath(vec![field.clone()]),
        })
        .collect();

    from.extend(
        enumerated
            .carries
            .iter()
            .filter(|field| !identity.contains(*field))
            .map(|field| input_ref(&enumerated.input, field)),
    );

    let mut steps = vec![
        TransactionStep::Read(Read {
            bind: read.clone(),
            target: record.selector.clone(),
            fields: FieldSelection::Only(observed),
        }),
        TransactionStep::Write(Write {
            target: record.selector.clone(),
            fields: changed
                .iter()
                .map(|field| FieldPath(vec![field.clone()]))
                .collect(),
            values: Derivation::Deterministic { from },
        }),
    ];

    if record.data.version.is_some() {
        steps.push(TransactionStep::BumpVersion(BumpVersion {
            target: record.selector.clone(),
        }));
    }

    let output = output_for(enumerated, record, Some(&read));

    if let Some((_, step)) = &output {
        steps.push(step.clone());
    }

    OperationBlock {
        steps: vec![
            OperationStep::Transaction(ExecuteTransaction {
                transaction: transaction(format!("tx.{name}.update"), record, steps),
                rejected: None,
            }),
            finish(enumerated, output.as_ref().map(|(bind, _)| bind)),
        ],
    }
}

fn keyed_insert(enumerated: &Enumerated, record: &Record) -> OperationBlock {
    let name = short(&enumerated.operation);

    let from: Vec<ValueRef> = enumerated
        .carries
        .iter()
        .filter(|field| record.fields.contains(*field))
        .map(|field| input_ref(&enumerated.input, field))
        .collect();

    OperationBlock {
        steps: vec![
            OperationStep::Transaction(ExecuteTransaction {
                transaction: transaction(
                    format!("tx.{name}.insert"),
                    record,
                    vec![TransactionStep::Insert(Insert {
                        object: record.object.clone(),
                        values: Derivation::Deterministic {
                            from: if from.is_empty() {
                                every_input_field(enumerated)
                            } else {
                                from
                            },
                        },
                    })],
                ),
                rejected: None,
            }),
            finish(enumerated, None),
        ],
    }
}

fn transition(
    enumerated: &Enumerated,
    lifecycle: &Lifecycle,
    refusal: Option<&Id>,
) -> OperationBlock {
    match (refusal, &enumerated.identity) {
        (Some(refusal), Some(key)) => inspected_transition(enumerated, lifecycle, refusal, key),
        _ => direct_transition(enumerated, lifecycle, refusal),
    }
}

fn transition_steps(record: &Record, lifecycle: &Lifecycle) -> Vec<TransactionStep> {
    let mut steps = vec![TransactionStep::Transition(StateTransition {
        machine: lifecycle.machine.clone(),
        transition: lifecycle.transition.clone(),
        subject: record.selector.clone(),
        effect_intents: BTreeMap::new(),
        effects: BTreeMap::new(),
    })];

    if record.data.version.is_some() {
        steps.push(TransactionStep::BumpVersion(BumpVersion {
            target: record.selector.clone(),
        }));
    }

    steps
}

fn refused(enumerated: &Enumerated, error: &Id) -> OperationStep {
    OperationStep::Return(Return {
        request: enumerated.input.clone(),
        outcome: ResultOutcome::Err {
            error: error.clone(),
            values: payload_from(
                enumerated,
                enumerated.errors.get(error).unwrap_or(&BTreeSet::new()),
                None,
            ),
        },
    })
}

/// The transition applied directly: a subscription completes when it
/// is rejected (redelivery of a message whose record has moved on has
/// nothing left to do), and an unkeyed request returns the error.
fn direct_transition(
    enumerated: &Enumerated,
    lifecycle: &Lifecycle,
    refusal: Option<&Id>,
) -> OperationBlock {
    let name = short(&enumerated.operation);
    let record = &enumerated.records[lifecycle.record];

    let rejected = OperationBlock {
        steps: vec![match refusal {
            Some(error) => refused(enumerated, error),
            None => OperationStep::Complete,
        }],
    };

    OperationBlock {
        steps: vec![
            OperationStep::Transaction(ExecuteTransaction {
                transaction: transaction(
                    format!("tx.{name}.transition"),
                    record,
                    transition_steps(record, lifecycle),
                ),
                rejected: Some(rejected),
            }),
            finish(enumerated, None),
        ],
    }
}

/// The replay-safe shape for a keyed request: inspect, then decide.
///
/// A transition applied directly is rejected or committed by the state
/// the record is in when *this* attempt arrives, so a retry after the
/// record moved — the order paid in between — commits where the first
/// attempt was refused, and returns a different result. Instead a keyed
/// inspection exports the state, and the decision rests on that
/// recovered artifact: every attempt of the request decides the same
/// way. The transition itself is keyed too, so a retry after it
/// committed recovers its commit. Its rejection now means only a race
/// with a concurrent change after the inspection, and completes — the
/// shape the fixtures' authors use.
fn inspected_transition(
    enumerated: &Enumerated,
    lifecycle: &Lifecycle,
    refusal: &Id,
    key: &IdempotencyKey,
) -> OperationBlock {
    let name = short(&enumerated.operation);
    let record = &enumerated.records[lifecycle.record];

    let read = Id(format!("read.{name}.inspect"));
    let lookup = Id(format!("output.{name}.lookup"));

    let mut exported: Vec<ValueRef> = key.components.clone();

    exported.push(ValueRef {
        source: ValueSource::TransactionRead(read.clone()),
        path: lifecycle.state.clone(),
    });

    let keyed = |mut transaction: Transaction| {
        transaction.idempotency = IdempotencyGuarantee::DeduplicatedBy { key: key.clone() };
        transaction
    };

    let inspect = keyed(transaction(
        format!("tx.{name}.inspect"),
        record,
        vec![
            TransactionStep::Read(Read {
                bind: read.clone(),
                target: record.selector.clone(),
                fields: FieldSelection::Only([lifecycle.state.clone()].into()),
            }),
            TransactionStep::EstablishTransactionOutput(EstablishTransactionOutput {
                bind: lookup.clone(),
                schema: record.data.schema.clone(),
                values: Derivation::Deterministic { from: exported },
            }),
        ],
    ));

    let in_state = |state: &Id| Condition::Eq {
        value: ValueRef {
            source: ValueSource::TransactionOutput(lookup.clone()),
            path: lifecycle.state.clone(),
        },
        equals: SelectorValue::Literal(Literal::String(state.0.clone())),
    };

    // In any `from` state: a disjunction, spelled with the vocabulary's
    // `not` and `and`.
    let applies = match lifecycle.from.iter().collect::<Vec<_>>().as_slice() {
        [only] => in_state(only),
        many => Condition::Not {
            condition: Box::new(Condition::And {
                conditions: many
                    .iter()
                    .map(|state| Condition::Not {
                        condition: Box::new(in_state(state)),
                    })
                    .collect(),
            }),
        },
    };

    let apply = keyed(transaction(
        format!("tx.{name}.transition"),
        record,
        transition_steps(record, lifecycle),
    ));

    OperationBlock {
        steps: vec![
            OperationStep::Transaction(ExecuteTransaction {
                transaction: inspect,
                rejected: None,
            }),
            OperationStep::Branch(Branch {
                condition: applies,
                then: OperationBlock {
                    steps: vec![
                        OperationStep::Transaction(ExecuteTransaction {
                            transaction: apply,
                            rejected: Some(OperationBlock {
                                steps: vec![OperationStep::Complete],
                            }),
                        }),
                        finish(enumerated, None),
                    ],
                },
                otherwise: Some(OperationBlock {
                    steps: vec![refused(enumerated, refusal)],
                }),
            }),
        ],
    }
}
