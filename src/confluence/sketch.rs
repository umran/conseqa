//! Operation sketches, and their compilation into programs.
//!
//! A sketch is what an operation does, as a short list of typed business
//! actions over the skeleton's symbols — find a record, change it,
//! create one, apply a lifecycle transition, advance a position, admit
//! or publish a message, call an outside service. The coordinator writes
//! it with the operation's interface, where it already knows what the
//! operation is for. Code compiles it into a program: transaction
//! grouping, ids, observed-version guards or strict locks, keyed commits
//! from the trigger's identity, key propagation into messages, the
//! inspect-then-decide shape a guarded transition needs to replay,
//! effect intents executed after commit, result matching, outputs,
//! returns and rejection arms. No session writes the program, and no
//! model writes a step.
//!
//! Compilation is a pure function of the sketch and the symbols it
//! references ([`Symbols`]), so the commit gate dry-compiles every
//! sketch it is given and rejects one that does not compile, with the
//! reason, while its author can still fix it.
//!
//! Value references are written compactly, as their authors think of
//! them: `input.quantity` is a field of the operation's input,
//! `product.stock` a field of the record found as `product`.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::spec::{
    AdvanceCursor, Branch, CompareAndSet, CompareCondition, Condition, CursorAdvanceRule,
    DataObject, Derivation, Effect, ErrorDisposition, ErrorResultType, EstablishEffectIntent,
    EstablishTransactionOutput, ExecuteEffect, ExecuteEffectIntent, ExecuteTransaction,
    ExternalEffect, ExternalIdempotency, ExternalIdentity, ExternalIdentityKey,
    ExternalResultReplay, FieldPath, FieldSelection, Id, IdempotencyGuarantee, IdempotencyKey,
    IdempotencyKeyPropagation, Input, Insert, Literal, Lock, LockMode, LockOrder, MatchResult,
    MessageIdentity, MessageSelector, ObjectSelector, OperationBlock, OperationStep, Outbox,
    OutboxWriteEffect, PublicationEffect, Read, RequestIdentity, ResultOutcome, ResultType, Return,
    SelectorPredicate, SelectorValue, StateMachine, StateMachineSubject, StateTransition, Topic,
    Transaction, TransactionIsolation, TransactionStep, TransitionSideEffect, Update, ValueRef,
    ValueSource, WriteOutboxEffect,
};

/// What an operation does, in order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationSketch {
    pub steps: Vec<SketchStep>,

    /// What a request's `ok` result is made of, when its fields are not
    /// simply found by name in the input and the records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub returns: Option<Vec<String>>,

    /// The isolation every transaction declares; `read_committed` when
    /// omitted, and repair strengthens it where a proof needs it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isolation: Option<TransactionIsolation>,

    /// The values every transaction's commit is deduplicated by; the
    /// trigger's identity when omitted, and `[]` for no deduplication.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_key: Option<Vec<String>>,

    /// With several request inputs, the one the program's result is for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub returns_for: Option<Id>,

    /// What a transaction refused by its guards — a moved version, a
    /// stale position or token, a transition from the wrong state with
    /// no `otherwise` — does instead of completing: steps that end the
    /// operation (a request's must reject).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub on_rejected: Vec<SketchStep>,
}

/// One business action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SketchStep {
    /// Find the one instance of `record` whose fields equal the given
    /// values, and call it `as` for the steps after.
    Find {
        #[serde(rename = "as")]
        alias: String,
        record: Id,
        /// Record field → value reference or literal, e.g. `product_id:
        /// input.product_id`. Together they must pin one instance,
        /// unless `all` is said.
        #[serde(default)]
        by: BTreeMap<String, serde_json::Value>,

        /// Select every instance `by` matches (all of them when `by` is
        /// empty), not one.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        all: bool,

        /// Hold the selection under a lock from before it is read:
        /// `shared` or `exclusive`. A record that changes is held
        /// exclusively, or its change conditioned on the version read,
        /// whether or not this is said.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lock: Option<LockMode>,

        /// The order in which a multi-instance lock is acquired.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        lock_order: Vec<LockOrderTerm>,

        /// The isolation of the transaction this step belongs to.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        isolation: Option<TransactionIsolation>,
    },

    /// Give fields of a found record new values, derived from `from`.
    Update {
        record: String,
        /// The fields that change; the map form of `from` names them
        /// itself, and `set` may then be omitted.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        set: Vec<String>,
        #[serde(default)]
        from: Values,
    },

    /// Create one instance of `record` from `from`.
    Create {
        record: Id,
        from: Values,

        /// The isolation of the transaction this step belongs to.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        isolation: Option<TransactionIsolation>,
    },

    /// Insert the one instance of `record` that `by` identifies, made
    /// from `from`, or — when it already exists — give its `set` fields
    /// new values from `update_from`: one atomic insert-or-update,
    /// arbitrated on the record's identity. Nothing is read first, and
    /// the upsert protects no read.
    Upsert {
        record: Id,
        /// Record field → value reference or literal, pinning the whole
        /// identity.
        by: BTreeMap<String, serde_json::Value>,
        from: Values,
        /// The fields an existing instance changes; the map form of
        /// `update_from` names them itself.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        set: Vec<String>,
        #[serde(default)]
        update_from: Values,

        /// The isolation of the transaction this step belongs to.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        isolation: Option<TransactionIsolation>,
    },

    /// Apply a lifecycle transition to a found record.
    Transition {
        record: String,
        transition: Id,

        /// The declared error a request returns when the record is not
        /// in a state the transition applies from.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        otherwise: Option<Id>,

        /// When the record is already in the transition's target state,
        /// return the request's `ok` result instead of the error:
        /// repeating a completed action has no further effect.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        already_ok: bool,

        /// What the transition's declared side effects are made of;
        /// everything the trigger carries when omitted.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        effects_from: Option<Vec<String>>,
    },

    /// Advance a found record's position field to the trigger's
    /// position, rejecting a stale or out-of-order one.
    Advance {
        record: String,
        field: String,
        to: String,

        /// `successor` (every position, in order, none skipped) or
        /// `monotonic_after` (any later position). Left open when
        /// omitted, for the prompt to settle.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rule: Option<CursorAdvanceRule>,

        /// The declared error a request returns for a stale position.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        otherwise: Option<Id>,
    },

    /// Admit a message to a transactional outbox, atomically with the
    /// records changed.
    Enqueue {
        outbox: Id,
        schema: Id,
        from: Values,

        /// The values carried into the message's (or request's) own
        /// identity; the trigger's identity when omitted.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<Vec<String>>,
    },

    /// Publish a message to a topic: after the commit when the
    /// operation changes records, directly otherwise.
    Publish {
        topic: Id,
        schema: Id,
        from: Values,

        /// Start it without waiting for it to complete.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        detached: bool,

        /// Arrange it durably in a transaction and make it once that
        /// commits — implied when the operation changes records first.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        durable: bool,

        /// The values carried into the message's (or request's) own
        /// identity; the trigger's identity when omitted.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<Vec<String>>,
    },

    /// Call an outside service, and act on what it answers.
    Call {
        name: String,

        #[serde(rename = "as", default, skip_serializing_if = "Option::is_none")]
        alias: Option<String>,

        /// The values that identify one interaction, when the service
        /// declares a key; none when it does not.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        identity: Option<Vec<String>>,

        /// What a duplicate call does: `unspecified`, `distinguishable`,
        /// `identical_per_identity` or `side_effect_free`.
        #[serde(default = "unspecified_duplicates")]
        duplicates: ExternalIdempotency,

        #[serde(default = "unspecified_replay")]
        result_replay: ExternalResultReplay,

        /// The answer's `ok` schema and error schemas, when it answers.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result: Option<CallResult>,

        #[serde(default)]
        from: Values,

        /// Steps on an `ok` answer.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        on_ok: Vec<SketchStep>,

        /// Steps on each declared error.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        on_error: BTreeMap<Id, Vec<SketchStep>>,

        /// Arrange the call durably in the transaction before it, and
        /// make it once that commits.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        durable: bool,

        /// Start it without waiting for it to complete.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        detached: bool,
    },

    /// Delete a found record.
    Delete { record: String },

    /// Accept a fencing token for a found record, rejecting a stale one.
    Fence {
        record: String,
        field: String,
        token: String,

        /// The declared error a request returns for a stale token.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        otherwise: Option<Id>,
    },

    /// Invoke another operation's request input, and act on its answer.
    Request {
        operation: Id,

        /// The target's request input; its only one when omitted.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input: Option<Id>,

        #[serde(rename = "as", default, skip_serializing_if = "Option::is_none")]
        alias: Option<String>,

        /// `unspecified`, `never` or `may_repeat`.
        #[serde(default = "unspecified_retry")]
        retry: crate::spec::RetrySemantics,

        #[serde(default)]
        from: Values,

        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        on_ok: Vec<SketchStep>,

        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        on_error: BTreeMap<Id, Vec<SketchStep>>,

        /// Arrange the request durably in the transaction before it, and
        /// make it once that commits.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        durable: bool,

        /// Start it without waiting for it to complete.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        detached: bool,

        /// The values carried into the message's (or request's) own
        /// identity; the trigger's identity when omitted.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<Vec<String>>,
    },

    /// Act on the answer a `parallel` member bound with its `as`.
    Answer {
        of: String,

        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        on_ok: Vec<SketchStep>,

        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        on_error: BTreeMap<Id, Vec<SketchStep>>,
    },

    /// Decide between two courses on values known at this point: the
    /// input, records found in finished transactions, and answers.
    When {
        #[serde(rename = "if")]
        condition: SketchCondition,
        #[serde(default)]
        then: Vec<SketchStep>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        otherwise: Vec<SketchStep>,
    },

    /// End a request with one of its declared errors.
    Reject {
        error: Id,
        #[serde(default)]
        from: Values,
    },

    /// End a message's attempt without completing it: the message is
    /// left unacknowledged, or pending, for another attempt. How a
    /// consumer passes a retryable error up.
    Abandon,

    /// Start every effect at once and wait for all of them.
    Parallel { steps: Vec<SketchStep> },

    /// Start every effect at once and wait for the first to complete,
    /// acting on its answer.
    Race {
        steps: Vec<SketchStep>,

        #[serde(rename = "as", default, skip_serializing_if = "Option::is_none")]
        alias: Option<String>,

        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        on_ok: Vec<SketchStep>,

        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        on_error: BTreeMap<Id, Vec<SketchStep>>,
    },

    /// Start an effect and do not wait for it.
    Start { step: Box<SketchStep> },
}

fn unspecified_retry() -> crate::spec::RetrySemantics {
    crate::spec::RetrySemantics::Unspecified
}

/// A condition of a `when`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum SketchCondition {
    /// `[value, value-or-literal]`: a value reference first, then a
    /// value reference or a literal (string, integer, boolean).
    Equals(Vec<serde_json::Value>),
    Present(String),
    Not(Box<SketchCondition>),
    All(Vec<SketchCondition>),
    Any(Vec<SketchCondition>),

    /// A decision the model states no fact about — a business rule the
    /// DSL cannot express — described in words for its readers.
    Unspecified(String),
}

/// One term of a lock acquisition order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockOrderTerm {
    pub field: String,
    pub direction: crate::spec::OrderingDirection,
}

fn unspecified_duplicates() -> ExternalIdempotency {
    ExternalIdempotency::Unspecified
}

fn unspecified_replay() -> ExternalResultReplay {
    ExternalResultReplay::Unspecified
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallResult {
    pub ok: Id,
    #[serde(default)]
    pub errors: BTreeMap<Id, CallError>,
}

/// A declared error of an answer: its schema, and — when known — whether
/// observing it ends the interaction (`terminal`) or admits another
/// attempt (`retryable`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CallError {
    Schema(Id),
    Declared {
        schema: Id,
        disposition: ErrorDisposition,
    },
}

impl CallError {
    fn declared(&self) -> ErrorResultType {
        match self {
            Self::Schema(schema) => ErrorResultType {
                schema: schema.clone(),
                disposition: ErrorDisposition::Unspecified,
            },
            Self::Declared {
                schema,
                disposition,
            } => ErrorResultType {
                schema: schema.clone(),
                disposition: *disposition,
            },
        }
    }
}

/// The values a step derives from: a list of value references, or —
/// to say which target field each feeds — a map from target field to
/// value reference, whose fields are checked against the target's
/// schema. Both compile to the same provenance: the DSL records which
/// values a write derives from, not a per-field mapping.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Values {
    List(Vec<String>),
    Fields(BTreeMap<String, String>),
}

impl Default for Values {
    fn default() -> Self {
        Self::List(Vec::new())
    }
}

impl Values {
    pub fn refs(&self) -> Vec<String> {
        match self {
            Self::List(values) => values.clone(),
            Self::Fields(fields) => fields.values().cloned().collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        match self {
            Self::List(values) => values.is_empty(),
            Self::Fields(fields) => fields.is_empty(),
        }
    }

    /// The target fields named, when the map form is used.
    pub fn fields(&self) -> Option<BTreeSet<String>> {
        match self {
            Self::List(_) => None,
            Self::Fields(fields) => Some(fields.keys().cloned().collect()),
        }
    }

    /// Fails when the map form names a field `target` lacks.
    fn check(&self, target: &BTreeSet<String>, what: &str) -> Result<(), CompileError> {
        if let Some(fields) = self.fields() {
            for field in fields {
                if !target.contains(&field) {
                    return fail(format!("{what} has no field `{field}`"));
                }
            }
        }

        Ok(())
    }
}

/// What compilation reads from the skeleton.
#[derive(Debug, Clone, Default)]
pub struct Symbols {
    /// Data object → its data model and declaration.
    pub objects: BTreeMap<Id, (Id, DataObject)>,

    /// Schema → its top-level field names.
    pub schemas: BTreeMap<Id, BTreeSet<String>>,

    pub machines: BTreeMap<Id, StateMachine>,

    pub topics: BTreeMap<Id, Topic>,

    /// Outbox → its data model and declaration.
    pub outboxes: BTreeMap<Id, (Id, Outbox)>,

    /// Operation → its request inputs, for requests into it.
    pub requests: BTreeMap<Id, BTreeMap<Id, crate::spec::RequestInput>>,
}

impl Symbols {
    /// Everything a workspace declares.
    pub fn of(workspace: &super::WorkspaceState) -> Self {
        let mut objects = BTreeMap::new();
        let mut outboxes = BTreeMap::new();

        for (model, data) in &workspace.data_models {
            for (object, declared) in &data.objects {
                objects.insert(object.clone(), (model.clone(), declared.clone()));
            }

            for (outbox, declared) in &data.outboxes {
                outboxes.insert(outbox.clone(), (model.clone(), declared.clone()));
            }
        }

        Self {
            objects,
            schemas: workspace
                .schemas
                .iter()
                .map(|(id, schema)| (id.clone(), schema_fields(schema)))
                .collect(),
            machines: workspace.state_machines.clone(),
            topics: workspace.topics.clone(),
            outboxes,
            requests: workspace
                .operations
                .iter()
                .map(|(operation, draft)| {
                    (
                        operation.clone(),
                        draft
                            .inputs
                            .iter()
                            .filter_map(|(id, input)| match input {
                                Input::Request(request) => Some((id.clone(), request.clone())),
                                _ => None,
                            })
                            .collect(),
                    )
                })
                .collect(),
        }
    }

    fn fields(&self, schema: &Id) -> BTreeSet<String> {
        self.schemas.get(schema).cloned().unwrap_or_default()
    }
}

pub fn schema_fields(schema: &crate::spec::Schema) -> BTreeSet<String> {
    match schema {
        crate::spec::Schema::Canonical(canonical) => canonical.fields.keys().cloned().collect(),
        _ => BTreeSet::new(),
    }
}

/// Why a sketch does not compile, in words its author can act on.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct CompileError(pub String);

fn fail<T>(message: impl Into<String>) -> Result<T, CompileError> {
    Err(CompileError(message.into()))
}

/// A choice compilation leaves open, which a caller must settle before
/// compiling again: none means the program is fully determined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Open {
    /// A request's guarded transition names no `otherwise` error: which
    /// declared error a wrong-state record returns.
    Refusal { transition: Id, errors: Vec<Id> },

    /// An advance names no rule: whether every position must be
    /// applied, none skipped.
    CursorRule { record: String, field: String },
}

/// Settled answers to [`Open`] choices.
#[derive(Debug, Clone, Default)]
pub struct Settled {
    pub refusal: Option<Id>,
    pub cursor_rule: Option<CursorAdvanceRule>,
}

/// The trigger's identity, as the values that pin it.
fn message_key(identity: &MessageIdentity, schema: &Id, input: &Id) -> Option<IdempotencyKey> {
    let MessageIdentity::Keyed(key) = identity else {
        return None;
    };

    Some(IdempotencyKey {
        components: key
            .mapping
            .get(schema)?
            .iter()
            .map(|field| ValueRef {
                source: ValueSource::Input(input.clone()),
                path: field.clone(),
            })
            .collect(),
    })
}

/// What a sketch compiles against: the operation and its one input.
struct Context<'a> {
    operation: &'a Id,
    name: String,
    input: &'a Id,
    request: bool,
    carries: BTreeSet<String>,
    ok: Option<(Id, BTreeSet<String>)>,
    errors: BTreeMap<Id, BTreeSet<String>>,

    /// The trigger's identity: the request's, or the message's.
    key: Option<IdempotencyKey>,

    /// Every input, with the fields it carries.
    inputs: BTreeMap<Id, BTreeSet<String>>,

    /// Whether the operation has one input, so `input.<field>` is
    /// unambiguous.
    single: bool,
}

/// A record found by a `find` step.
struct Found {
    record: Id,
    data_model: Id,
    data: DataObject,
    fields: BTreeSet<String>,
    selector: ObjectSelector,
    read: Id,

    /// Whether its selector rests only on the input: what an inspection
    /// before the main transaction can select.
    input_only: bool,

    /// Whether it is one instance, pinned by its identity, or a set.
    single: bool,

    /// Whether the transaction that found it is still being built.
    open: bool,

    /// The output it was exported through when that transaction closed,
    /// for the steps after it.
    exported: Option<Id>,
}

/// The open choices of a sketch, if any.
pub fn open_choices(
    operation: &Id,
    draft: &super::DraftOperation,
    symbols: &Symbols,
) -> Result<Vec<Open>, CompileError> {
    let context = context(operation, draft, symbols)?;
    let sketch = sketch_of(operation, draft)?;

    let mut open = Vec::new();

    fn visit(steps: &[SketchStep], context: &Context<'_>, open: &mut Vec<Open>) {
        for step in steps {
            match step {
                SketchStep::Transition {
                    transition,
                    otherwise: None,
                    ..
                } if context.request => open.push(Open::Refusal {
                    transition: transition.clone(),
                    errors: context.errors.keys().cloned().collect(),
                }),

                SketchStep::Advance {
                    record,
                    field,
                    rule: None,
                    ..
                } => open.push(Open::CursorRule {
                    record: record.clone(),
                    field: field.clone(),
                }),

                SketchStep::Call {
                    on_ok, on_error, ..
                }
                | SketchStep::Request {
                    on_ok, on_error, ..
                }
                | SketchStep::Race {
                    on_ok, on_error, ..
                }
                | SketchStep::Answer {
                    on_ok, on_error, ..
                } => {
                    visit(on_ok, context, open);
                    for arm in on_error.values() {
                        visit(arm, context, open);
                    }
                }

                SketchStep::When {
                    then, otherwise, ..
                } => {
                    visit(then, context, open);
                    visit(otherwise, context, open);
                }

                _ => {}
            }
        }
    }

    visit(&sketch.steps, &context, &mut open);

    Ok(open)
}

fn sketch_of<'a>(
    operation: &Id,
    draft: &'a super::DraftOperation,
) -> Result<&'a OperationSketch, CompileError> {
    let sketch = draft
        .sketch
        .as_ref()
        .ok_or_else(|| CompileError(format!("{operation} has no sketch")))?;

    if sketch.steps.is_empty() {
        return fail("the sketch has no steps");
    }

    Ok(sketch)
}

/// Whether a step reads or changes durable state, and so belongs inside
/// the operation's transaction.
fn transactional(step: &SketchStep) -> bool {
    matches!(
        step,
        SketchStep::Find { .. }
            | SketchStep::Update { .. }
            | SketchStep::Create { .. }
            | SketchStep::Upsert { .. }
            | SketchStep::Transition { .. }
            | SketchStep::Advance { .. }
            | SketchStep::Enqueue { .. }
            | SketchStep::Delete { .. }
            | SketchStep::Fence { .. }
    )
}

/// The compiler's working state over one sketch.
struct Compiler<'a> {
    context: Context<'a>,
    symbols: &'a Symbols,
    settled: &'a Settled,

    found: BTreeMap<String, Found>,
    steps: Vec<TransactionStep>,
    mutated: Vec<String>,
    created: BTreeSet<String>,
    created_from: Vec<ValueRef>,
    data_models: BTreeSet<Id>,

    /// The guarded transition: its record, machine, transition,
    /// refusal, and whether a repeat returns ok.
    guarded: Option<(String, Id, Id, Option<Id>, bool)>,

    /// Intents established inside the transaction, executed after it
    /// commits, in order.
    after_commit: Vec<(Id, bool)>,

    effects: usize,

    /// Records deleted in the open transaction.
    deleted: Vec<String>,

    /// Whether several transactions and transitions are allowed: the
    /// general path.
    general: bool,

    /// Transactions sealed so far, for their ids.
    segments: usize,

    /// Every refusal a transition of the open transaction names: what
    /// its rejection returns.
    refusals: Vec<Id>,

    /// Answers in scope: a call or request `as` → where its fields are.
    results: BTreeMap<String, ValueSource>,

    /// The `ok` result's output, when the last transaction established
    /// it.
    final_output: Option<Id>,

    /// Answers a `parallel` bound: `as` → binding, error classes.
    joined: BTreeMap<String, (Id, Vec<Id>)>,

    /// What a rejected transaction does, from the sketch.
    on_rejected: Vec<SketchStep>,

    /// A step's own propagation key, while the step is compiled.
    key_override: Option<IdempotencyKey>,

    /// The isolation a step declared for the open transaction.
    segment_isolation: Option<TransactionIsolation>,

    /// Whether any transaction's rejection ran `on_rejected`.
    on_rejected_used: bool,
}

impl Compiler<'_> {
    fn effect_ids(&mut self, role: &str) -> (Id, Id) {
        self.effects += 1;

        let name = &self.context.name;
        let index = self.effects;

        (
            Id(format!("effect.{name}.{role}_{index}")),
            Id(format!("intent.{name}.{role}_{index}")),
        )
    }

    fn resolve(&self, reference: &str) -> Result<ValueRef, CompileError> {
        resolve(&self.context, &self.found, &self.results, reference)
    }

    /// `from`, checked against `schema` when it names target fields.
    fn checked(&self, from: &Values, schema: &Id) -> Result<Derivation, CompileError> {
        from.check(&self.symbols.fields(schema), &format!("`{schema}`"))?;

        self.derivation(&from.refs())
    }

    fn derivation(&self, from: &[String]) -> Result<Derivation, CompileError> {
        derivation(&self.context, &self.found, &self.results, from)
    }

    /// The trigger's identity carried into a message's own identity, so
    /// a duplicate of one trigger is a duplicate of one message.
    fn propagation(
        &self,
        effect: &Id,
        identity: &MessageIdentity,
        schema: &Id,
    ) -> Vec<IdempotencyKeyPropagation> {
        let source = self.key_override.as_ref().or(self.context.key.as_ref());

        let (Some(key), MessageIdentity::Keyed(target)) = (source, identity) else {
            return Vec::new();
        };

        let Some(fields) = target.mapping.get(schema) else {
            return Vec::new();
        };

        if fields.len() != key.components.len() {
            return Vec::new();
        }

        vec![IdempotencyKeyPropagation {
            source: key.clone(),
            target: IdempotencyKey {
                components: fields
                    .iter()
                    .map(|field| ValueRef {
                        source: ValueSource::Effect(effect.clone()),
                        path: field.clone(),
                    })
                    .collect(),
            },
        }]
    }

    fn publication(
        &mut self,
        topic: &Id,
        schema: &Id,
    ) -> Result<(Id, Id, PublicationEffect), CompileError> {
        let Some(declared) = self.symbols.topics.get(topic) else {
            return fail(format!("`{topic}` is not a declared topic"));
        };

        if !declared.messages.contains(schema) {
            return fail(format!("`{topic}` does not carry `{schema}`"));
        }

        let (effect, intent) = self.effect_ids("publish");

        let publication = PublicationEffect {
            topic: topic.clone(),
            schema: schema.clone(),
            idempotency_key_propagation: self.propagation(
                &effect,
                &declared.message_identity,
                schema,
            ),
        };

        Ok((effect, intent, publication))
    }

    /// A transactional step, into the transaction body.
    fn record_step(&mut self, step: &SketchStep) -> Result<(), CompileError> {
        self.key_override = match step {
            SketchStep::Enqueue { key, .. }
            | SketchStep::Publish { key, .. }
            | SketchStep::Request { key, .. } => self.key_of(key)?,
            _ => None,
        };

        match step {
            SketchStep::Find {
                alias,
                record,
                by,
                all,
                lock,
                lock_order,
                isolation,
            } => {
                self.declare_isolation(*isolation)?;
                self.find(alias, record, by, *all, *lock, lock_order)
            }

            SketchStep::Update { record, set, from } => {
                let Some(target) = self.found.get(record) else {
                    return fail(format!(
                        "update names `{record}`, which no find before it names"
                    ));
                };

                from.check(&target.fields, &format!("`{}`", target.record))?;

                let set: Vec<String> = match (set.is_empty(), from.fields()) {
                    (true, Some(fields)) => fields.into_iter().collect(),
                    _ => set.clone(),
                };

                let set = &set;

                if set.is_empty() {
                    return fail(format!("update of `{record}` sets no field"));
                }

                let identity: BTreeSet<&String> = target
                    .data
                    .identity
                    .iter()
                    .filter_map(|path| path.0.last())
                    .collect();

                for field in set {
                    if !target.fields.contains(field) {
                        return fail(format!("`{}` has no field `{field}`", target.record));
                    }

                    if identity.contains(field) {
                        return fail(format!(
                            "`{field}` is part of `{}`'s identity and cannot change",
                            target.record
                        ));
                    }
                }

                let selector = target.selector.clone();
                let values = self.derivation(&from.refs())?;

                self.steps.push(TransactionStep::Update(Update {
                    target: selector,
                    fields: set
                        .iter()
                        .map(|field| FieldPath(vec![field.clone()]))
                        .collect(),
                    values,
                }));

                self.mutated.push(record.clone());

                Ok(())
            }

            SketchStep::Create {
                record,
                from,
                isolation,
            } => {
                self.declare_isolation(*isolation)?;

                let Some((data_model, data)) = self.symbols.objects.get(record) else {
                    return fail(format!("`{record}` is not a declared data object"));
                };

                let (data_model, data) = (data_model.clone(), data.clone());

                from.check(&self.symbols.fields(&data.schema), &format!("`{record}`"))?;

                let values = self.derivation(&from.refs())?;

                let Derivation::Deterministic { from: roots } = &values else {
                    return fail(format!("create `{record}` derives its values from nothing"));
                };

                self.created_from.extend(roots.iter().cloned());
                self.created.extend(self.symbols.fields(&data.schema));
                self.data_models.insert(data_model);

                self.steps.push(TransactionStep::Insert(Insert {
                    object: record.clone(),
                    values,
                }));

                Ok(())
            }

            SketchStep::Upsert {
                record,
                by,
                from,
                set,
                update_from,
                isolation,
            } => {
                self.declare_isolation(*isolation)?;
                self.upsert(record, by, from, set, update_from)
            }

            SketchStep::Transition {
                record,
                transition,
                otherwise,
                already_ok,
                effects_from,
            } => self.transition(record, transition, otherwise, *already_ok, effects_from),

            SketchStep::Advance {
                record,
                field,
                to,
                rule,
                otherwise,
            } => {
                self.refusal(otherwise)?;

                let Some(target) = self.found.get(record) else {
                    return fail(format!(
                        "advance names `{record}`, which no find before it names"
                    ));
                };

                if !target.fields.contains(field) {
                    return fail(format!("`{}` has no field `{field}`", target.record));
                }

                let Some(rule) = rule.or(self.settled.cursor_rule) else {
                    return fail(format!(
                        "the advance of `{record}.{field}` names no rule: `successor` or \
                         `monotonic_after`"
                    ));
                };

                let selector = target.selector.clone();
                let incoming = self.resolve(to)?;

                self.steps
                    .push(TransactionStep::AdvanceCursor(AdvanceCursor {
                        target: selector,
                        field: FieldPath(vec![field.clone()]),
                        incoming,
                        rule,
                        compare: Vec::new(),
                    }));

                self.mutated.push(record.clone());

                Ok(())
            }

            SketchStep::Enqueue {
                outbox,
                schema,
                from,
                key,
            } => {
                let _ = key;

                let Some((data_model, declared)) = self.symbols.outboxes.get(outbox) else {
                    return fail(format!("`{outbox}` is not a declared outbox"));
                };

                if !declared.messages.contains(schema) {
                    return fail(format!("`{outbox}` does not admit `{schema}`"));
                }

                let (data_model, identity) =
                    (data_model.clone(), declared.message_identity.clone());

                let (effect, _) = self.effect_ids("enqueue");
                let values = self.checked(from, schema)?;

                self.data_models.insert(data_model);

                self.steps
                    .push(TransactionStep::WriteOutbox(WriteOutboxEffect {
                        effect: OutboxWriteEffect {
                            outbox: outbox.clone(),
                            schema: schema.clone(),
                            idempotency_key_propagation: self
                                .propagation(&effect, &identity, schema),
                        },
                        effect_id: effect,
                        values,
                    }));

                Ok(())
            }

            // A publication in a transactional sketch is established as
            // an intent atomically with the commit, and executed after.
            SketchStep::Publish {
                topic,
                schema,
                from,
                detached,
                ..
            } => {
                let (effect, intent, publication) = self.publication(topic, schema)?;
                let values = self.checked(from, schema)?;

                self.steps.push(TransactionStep::EstablishEffectIntent(
                    EstablishEffectIntent {
                        bind: intent.clone(),
                        effect_id: effect,
                        effect: Effect::Publication(publication),
                        values,
                    },
                ));

                self.after_commit.push((intent, *detached));

                Ok(())
            }

            SketchStep::Call { name, .. } => fail(format!(
                "the call to `{name}` is in a sketch that changes records; call outside services \
                 from an operation of their own, triggered by a message"
            )),

            SketchStep::Delete { record } => {
                let Some(target) = self.found.get(record) else {
                    return fail(format!(
                        "delete names `{record}`, which no find before it names"
                    ));
                };

                if !target.open {
                    return fail(format!(
                        "`{record}` was found in an earlier transaction; find it again to delete it"
                    ));
                }

                self.steps
                    .push(TransactionStep::Delete(crate::spec::Delete {
                        target: target.selector.clone(),
                    }));

                self.deleted.push(record.clone());

                Ok(())
            }

            SketchStep::Fence {
                record,
                field,
                token,
                otherwise,
            } => {
                self.refusal(otherwise)?;

                let Some(target) = self.found.get(record) else {
                    return fail(format!(
                        "fence names `{record}`, which no find before it names"
                    ));
                };

                if !target.fields.contains(field) {
                    return fail(format!("`{}` has no field `{field}`", target.record));
                }

                let selector = target.selector.clone();
                let token = self.resolve(token)?;

                self.steps.push(TransactionStep::Fence(crate::spec::Fence {
                    target: selector,
                    field: FieldPath(vec![field.clone()]),
                    token,
                    compare: Vec::new(),
                }));

                self.mutated.push(record.clone());

                Ok(())
            }

            other => fail(format!(
                "{} is not a step of a transaction",
                step_name(other)
            )),
        }
    }

    fn find(
        &mut self,
        alias: &str,
        record: &Id,
        by: &BTreeMap<String, serde_json::Value>,
        all: bool,
        lock: Option<LockMode>,
        lock_order: &[LockOrderTerm],
    ) -> Result<(), CompileError> {
        if self.found.contains_key(alias) || alias == "input" {
            return fail(format!(
                "`{alias}` names two things; give each find its own name"
            ));
        }

        let Some((data_model, data)) = self.symbols.objects.get(record) else {
            return fail(format!("`{record}` is not a declared data object"));
        };

        let fields = self.symbols.fields(&data.schema);

        if by.is_empty() && !all {
            return fail(format!(
                "find `{alias}` gives no field to find it by; say `all` to select every \
                 instance"
            ));
        }

        let mut predicates = Vec::new();
        let mut input_only = true;

        for (field, value) in by {
            if !fields.contains(field) {
                return fail(format!("`{record}` has no field `{field}` to find it by"));
            }

            let value = self.operand(value)?;

            if let SelectorValue::Value(value) = &value {
                input_only &= matches!(value.source, ValueSource::Input(_));
            }

            predicates.push(SelectorPredicate::Eq {
                field: FieldPath(vec![field.clone()]),
                value,
            });
        }

        let identity: BTreeSet<&String> = data
            .identity
            .iter()
            .filter_map(|path| path.0.last())
            .collect();

        let single = identity.iter().all(|field| by.contains_key(*field));

        if !single && !all {
            return fail(format!(
                "find `{alias}` must give every identity field of `{record}` ({}), or say `all` \
                 to select every instance that matches",
                identity
                    .iter()
                    .map(|field| format!("`{field}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }

        let predicate = match <[_; 1]>::try_from(predicates) {
            Ok([only]) => only,
            Err(many) if many.is_empty() => SelectorPredicate::All,
            Err(many) => SelectorPredicate::And { predicates: many },
        };

        let selector = ObjectSelector {
            object: record.clone(),
            predicate,
        };

        for term in lock_order {
            if !fields.contains(&term.field) {
                return fail(format!(
                    "`{record}` has no field `{}` to order locks by",
                    term.field
                ));
            }
        }

        if let Some(mode) = lock {
            self.steps.push(TransactionStep::Lock(Lock {
                target: selector.clone(),
                mode,
                order: if lock_order.is_empty() {
                    LockOrder::Unspecified
                } else {
                    LockOrder::By(
                        lock_order
                            .iter()
                            .map(|term| crate::spec::OrderingTerm {
                                field: FieldPath(vec![term.field.clone()]),
                                direction: term.direction,
                            })
                            .collect(),
                    )
                },
            }));
        } else if !lock_order.is_empty() {
            return fail(format!(
                "find `{alias}` orders locks but takes none: add a `lock`"
            ));
        }

        let read = Id(format!("read.{}.{alias}", self.context.name));

        self.data_models.insert(data_model.clone());

        // Nothing guards a read here: a record that changes is
        // protected where it changes (`protect`), and one that is only
        // read is given no imaginary assertion.
        self.steps.push(TransactionStep::Read(Read {
            bind: read.clone(),
            target: selector.clone(),
            fields: FieldSelection::All,
        }));

        self.found.insert(
            alias.to_string(),
            Found {
                record: record.clone(),
                data_model: data_model.clone(),
                data: data.clone(),
                fields,
                selector,
                read,
                input_only,
                single,
                open: true,
                exported: None,
            },
        );

        Ok(())
    }

    /// One atomic insert-or-update of the instance `by` identifies.
    fn upsert(
        &mut self,
        record: &Id,
        by: &BTreeMap<String, serde_json::Value>,
        from: &Values,
        set: &[String],
        update_from: &Values,
    ) -> Result<(), CompileError> {
        let Some((data_model, data)) = self.symbols.objects.get(record) else {
            return fail(format!("`{record}` is not a declared data object"));
        };

        let (data_model, data) = (data_model.clone(), data.clone());
        let fields = self.symbols.fields(&data.schema);

        let identity: BTreeSet<&String> = data
            .identity
            .iter()
            .filter_map(|path| path.0.last())
            .collect();

        if !identity.iter().all(|field| by.contains_key(*field)) {
            return fail(format!(
                "the upsert of `{record}` must give every identity field ({}) in `by`",
                identity
                    .iter()
                    .map(|field| format!("`{field}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }

        let mut predicates = Vec::new();

        for (field, value) in by {
            if !fields.contains(field) {
                return fail(format!("`{record}` has no field `{field}` to upsert it by"));
            }

            predicates.push(SelectorPredicate::Eq {
                field: FieldPath(vec![field.clone()]),
                value: self.operand(value)?,
            });
        }

        let predicate = match <[_; 1]>::try_from(predicates) {
            Ok([only]) => only,
            Err(many) => SelectorPredicate::And { predicates: many },
        };

        from.check(&fields, &format!("`{record}`"))?;
        update_from.check(&fields, &format!("`{record}`"))?;

        let set: Vec<String> = match (set.is_empty(), update_from.fields()) {
            (true, Some(named)) => named.into_iter().collect(),
            _ => set.to_vec(),
        };

        if set.is_empty() {
            return fail(format!(
                "the upsert of `{record}` sets no field of an existing one"
            ));
        }

        for field in &set {
            if !fields.contains(field) {
                return fail(format!("`{record}` has no field `{field}`"));
            }

            if identity.contains(field) {
                return fail(format!(
                    "`{field}` is part of `{record}`'s identity and cannot change"
                ));
            }
        }

        let insert_values = self.derivation(&from.refs())?;

        if let Derivation::Deterministic { from: roots } = &insert_values {
            self.created_from.extend(roots.iter().cloned());
        }

        self.created.extend(fields);
        self.data_models.insert(data_model);

        self.steps
            .push(TransactionStep::Upsert(crate::spec::Upsert {
                target: ObjectSelector {
                    object: record.clone(),
                    predicate,
                },
                insert_values,
                update_fields: set
                    .iter()
                    .map(|field| FieldPath(vec![field.clone()]))
                    .collect(),
                update_values: self.derivation(&update_from.refs())?,
            }));

        Ok(())
    }

    /// A value reference, or a literal: a string that names no value in
    /// scope, an integer, or a boolean.
    fn operand(&self, value: &serde_json::Value) -> Result<SelectorValue, CompileError> {
        Ok(match value {
            serde_json::Value::String(text) if self.is_reference(text) => {
                SelectorValue::Value(self.resolve(text)?)
            }
            serde_json::Value::String(text) => {
                SelectorValue::Literal(Literal::String(text.clone()))
            }
            serde_json::Value::Bool(flag) => SelectorValue::Literal(Literal::Bool(*flag)),
            serde_json::Value::Number(number) => match number.as_i64() {
                Some(number) => SelectorValue::Literal(Literal::Int(number)),
                None => return fail("literals are strings, integers or booleans"),
            },
            _ => return fail("a value is a reference, a string, an integer or a boolean"),
        })
    }

    fn transition(
        &mut self,
        record: &String,
        transition: &Id,
        otherwise: &Option<Id>,
        already_ok: bool,
        effects_from: &Option<Vec<String>>,
    ) -> Result<(), CompileError> {
        let Some(target) = self.found.get(record) else {
            return fail(format!(
                "transition names `{record}`, which no find before it names"
            ));
        };

        let Some((machine, declared)) = self.symbols.machines.iter().find(|(_, machine)| {
            let StateMachineSubject::Object { object, .. } = &machine.subject;

            object == &target.record && machine.transitions.contains_key(transition)
        }) else {
            return fail(format!(
                "no state machine of `{}` declares `{transition}`",
                target.record
            ));
        };

        let declared = declared.transitions[transition].clone();
        let machine = machine.clone();
        let subject = target.selector.clone();

        if let Some(error) = otherwise
            && !self.context.errors.contains_key(error)
        {
            return fail(format!("`{error}` is not an error the request declares"));
        }

        if self.guarded.is_some() && !self.general {
            return fail("a sketch applies at most one transition");
        }

        let refusal = otherwise.clone().or_else(|| self.settled.refusal.clone());

        if self.context.request && refusal.is_none() {
            return fail(format!(
                "the transition `{transition}` names no `otherwise` error for a record in the \
                 wrong state"
            ));
        }

        // The transition's declared side effects are established with
        // it, and executed after the commit.
        let values = match effects_from {
            Some(from) => self.derivation(from)?,
            None => Derivation::Deterministic {
                from: self
                    .context
                    .carries
                    .iter()
                    .map(|field| input(&self.context, field))
                    .collect(),
            },
        };

        let mut effect_intents = BTreeMap::new();

        for side_effect in declared.side_effects.keys() {
            let short = side_effect
                .0
                .rsplit('.')
                .next()
                .unwrap_or(&side_effect.0)
                .to_string();

            let intent = Id(format!("intent.{}.{short}", self.context.name));

            effect_intents.insert(
                side_effect.clone(),
                crate::spec::TransitionEffectIntent {
                    bind: intent.clone(),
                    values: values.clone(),
                },
            );

            self.after_commit.push((intent, false));
        }

        // Every declared side effect is a publication or a request: the
        // vocabulary has nothing further to check about them here.
        let _: Vec<&TransitionSideEffect> = declared.side_effects.values().collect();

        // The transition's declared outbox messages are admitted with it,
        // from the same values.
        let effects = declared
            .effects
            .keys()
            .map(|effect| {
                (
                    effect.clone(),
                    crate::spec::TransitionEffectApplication {
                        values: values.clone(),
                    },
                )
            })
            .collect();

        if let Some(refusal) = &refusal {
            self.refusals.push(refusal.clone());
        }

        if self.guarded.is_none() {
            self.guarded = Some((
                record.clone(),
                machine.clone(),
                transition.clone(),
                refusal,
                already_ok,
            ));
        }

        self.steps
            .push(TransactionStep::Transition(StateTransition {
                machine,
                transition: transition.clone(),
                subject,
                compare: Vec::new(),
                effect_intents,
                effects,
            }));

        self.mutated.push(record.clone());

        Ok(())
    }

    /// An effect step outside any transaction.
    fn effect_step(&mut self, step: &SketchStep) -> Result<Vec<OperationStep>, CompileError> {
        self.key_override = match step {
            SketchStep::Enqueue { key, .. }
            | SketchStep::Publish { key, .. }
            | SketchStep::Request { key, .. } => self.key_of(key)?,
            _ => None,
        };

        match step {
            SketchStep::Publish {
                topic,
                schema,
                from,
                ..
            } => {
                let (effect, _, publication) = self.publication(topic, schema)?;

                Ok(vec![OperationStep::ExecuteEffect(ExecuteEffect {
                    effect_id: effect,
                    effect: Effect::Publication(publication),
                    values: self.checked(from, schema)?,
                    bind: None,
                })])
            }

            SketchStep::Call {
                name,
                alias,
                identity,
                duplicates,
                result_replay,
                result,
                from,
                on_ok,
                on_error,
                ..
            } => {
                let (effect, _) = self.effect_ids("call");

                let identity = match identity {
                    Some(values) => ExternalIdentity::Keyed {
                        key: ExternalIdentityKey {
                            components: values
                                .iter()
                                .map(|value| self.resolve(value))
                                .collect::<Result<_, _>>()?,
                        },
                    },
                    None => ExternalIdentity::Unspecified,
                };

                let result_type = result.as_ref().map(|result| ResultType {
                    ok: result.ok.clone(),
                    errors: result
                        .errors
                        .iter()
                        .map(|(class, error)| (class.clone(), error.declared()))
                        .collect(),
                });

                for class in on_error.keys() {
                    if !result
                        .as_ref()
                        .is_some_and(|result| result.errors.contains_key(class))
                    {
                        return fail(format!(
                            "the call to `{name}` acts on the error `{class}`, which its result \
                             does not declare"
                        ));
                    }
                }

                if result.is_none() && !on_ok.is_empty() {
                    return fail(format!(
                        "the call to `{name}` acts on its answer but declares no result"
                    ));
                }

                let bind = result.as_ref().map(|_| {
                    Id(format!(
                        "result.{}.{}",
                        self.context.name,
                        alias.clone().unwrap_or_else(|| "call".to_string())
                    ))
                });

                let values = self.derivation(&from.refs())?;

                let mut steps = vec![OperationStep::ExecuteEffect(ExecuteEffect {
                    effect_id: effect,
                    effect: Effect::External(ExternalEffect {
                        name: name.clone(),
                        identity,
                        idempotency: *duplicates,
                        result_replay: *result_replay,
                        result: result_type,
                    }),
                    values,
                    bind: bind.clone(),
                })];

                if let Some(bind) = bind {
                    let mut arm = |inner: &[SketchStep]| -> Result<OperationBlock, CompileError> {
                        let mut block = Vec::new();

                        for step in inner {
                            if transactional(step) {
                                return fail(format!(
                                    "an answer of `{name}` may only publish or call; record \
                                     changes belong to the operation the publication triggers"
                                ));
                            }

                            block.extend(self.effect_step(step)?);
                        }

                        block.push(finish_effects(&self.context));

                        Ok(OperationBlock { steps: block })
                    };

                    let ok = arm(on_ok)?;

                    let mut errors = BTreeMap::new();

                    for class in result.iter().flat_map(|result| result.errors.keys()) {
                        let inner = on_error.get(class).map(Vec::as_slice).unwrap_or(&[]);

                        errors.insert(class.clone(), arm(inner)?);
                    }

                    steps.push(OperationStep::MatchResult(MatchResult {
                        result: bind,
                        ok,
                        errors,
                    }));
                }

                Ok(steps)
            }

            other => fail(format!("{} is not an effect step", step_name(other))),
        }
    }
}

/// The end of an effect-only path: a request returns `ok` from what it
/// carries; a message is done.
fn finish_effects(context: &Context<'_>) -> OperationStep {
    if context.request {
        ok_from_input(context)
    } else {
        OperationStep::Complete
    }
}

/// Compiles an operation's sketch into its program.
pub fn compile(
    operation: &Id,
    draft: &super::DraftOperation,
    symbols: &Symbols,
    settled: &Settled,
) -> Result<OperationBlock, CompileError> {
    let mut program = compile_inner(operation, draft, symbols, settled)?;

    let sketch = sketch_of(operation, draft)?;
    let context = context(operation, draft, symbols)?;

    // Declarations the sketch makes for every transaction.
    let commit = match &sketch.commit_key {
        None => None,
        Some(values) if values.is_empty() => Some(IdempotencyGuarantee::NotDeduplicated),
        Some(values) => Some(IdempotencyGuarantee::DeduplicatedBy {
            key: IdempotencyKey {
                components: values
                    .iter()
                    .map(|value| {
                        let reference =
                            resolve(&context, &BTreeMap::new(), &BTreeMap::new(), value)?;

                        if !matches!(reference.source, ValueSource::Input(_)) {
                            return fail(format!(
                                "the commit key `{value}` is not a field of the input"
                            ));
                        }

                        Ok(reference)
                    })
                    .collect::<Result<_, CompileError>>()?,
            },
        }),
    };

    let ids: Vec<Id> = program
        .transactions()
        .into_iter()
        .map(|(_, transaction)| transaction.id.clone())
        .collect();

    for id in ids {
        let transaction = program.transaction_mut(&id).expect("listed");

        // The sketch's isolation is every transaction's default; a step
        // that declared its own transaction's keeps it.
        if let Some(isolation) = sketch.isolation
            && transaction.isolation == TransactionIsolation::ReadCommitted
        {
            transaction.isolation = isolation;
        }

        if let Some(commit) = &commit {
            transaction.idempotency = commit.clone();
        }
    }

    Ok(program)
}

fn compile_inner(
    operation: &Id,
    draft: &super::DraftOperation,
    symbols: &Symbols,
    settled: &Settled,
) -> Result<OperationBlock, CompileError> {
    let context = context(operation, draft, symbols)?;
    let sketch = sketch_of(operation, draft)?;

    let mut compiler = Compiler {
        context,
        symbols,
        settled,
        found: BTreeMap::new(),
        steps: Vec::new(),
        mutated: Vec::new(),
        created: BTreeSet::new(),
        created_from: Vec::new(),
        data_models: BTreeSet::new(),
        guarded: None,
        after_commit: Vec::new(),
        effects: 0,
        deleted: Vec::new(),
        general: false,
        segments: 0,
        refusals: Vec::new(),
        results: BTreeMap::new(),
        final_output: None,
        joined: BTreeMap::new(),
        on_rejected: Vec::new(),
        key_override: None,
        segment_isolation: None,
        on_rejected_used: false,
    };

    if !is_simple(sketch, symbols) {
        return compiler.general(sketch);
    }

    // A sketch that touches no record is a sequence of effects.
    if !sketch.steps.iter().any(transactional) {
        let mut steps = Vec::new();
        let mut ended = false;

        for step in &sketch.steps {
            if ended {
                return fail("nothing may follow a call that acts on its answer");
            }

            let compiled = compiler.effect_step(step)?;

            ended = matches!(compiled.last(), Some(OperationStep::MatchResult(_)));

            steps.extend(compiled);
        }

        if !ended {
            steps.push(finish_effects(&compiler.context));
        }

        return Ok(OperationBlock { steps });
    }

    for step in &sketch.steps {
        compiler.record_step(step)?;
    }

    compiler.assemble(sketch)
}

impl Compiler<'_> {
    fn assemble(mut self, sketch: &OperationSketch) -> Result<OperationBlock, CompileError> {
        let [data_model] = self.data_models.iter().collect::<Vec<_>>()[..] else {
            return fail(format!(
                "the sketch touches {} data models; one transaction spans one",
                self.data_models.len()
            ));
        };

        let data_model = data_model.clone();

        self.protect();

        let output = self.output(sketch)?;

        if let Some((bind, schema, from)) = &output {
            self.steps.push(TransactionStep::EstablishTransactionOutput(
                EstablishTransactionOutput {
                    bind: bind.clone(),
                    schema: schema.clone(),
                    values: Derivation::Deterministic { from: from.clone() },
                },
            ));
        }

        let context = &self.context;

        let mut main = Transaction {
            id: Id(format!("tx.{}", context.name)),
            data_model: Some(data_model),
            isolation: self
                .segment_isolation
                .unwrap_or(TransactionIsolation::ReadCommitted),
            idempotency: match &context.key {
                Some(key) => IdempotencyGuarantee::DeduplicatedBy { key: key.clone() },
                None => IdempotencyGuarantee::Unspecified,
            },
            requirements: Default::default(),
            steps: std::mem::take(&mut self.steps),
        };

        narrow_reads(&mut main, &self.found);

        let rejects = main.rejects();

        // What runs after the commit: the intents established in it.
        let mut success: Vec<OperationStep> = self
            .after_commit
            .clone()
            .into_iter()
            .map(|(intent, detached)| execute_intent(intent, detached))
            .collect();

        success.push(finish(context, output.as_ref().map(|(bind, _, _)| bind)));

        // A guarded transition behind a keyed request: inspect, then
        // decide, so every attempt of one request decides alike.
        if let Some((alias, machine, transition, Some(refusal), already_ok)) = &self.guarded
            && context.request
            && let Some(key) = &context.key
            && self.found[alias].input_only
            && self.found[alias].single
        {
            let target = &self.found[alias];
            let state = state_field(self.symbols, machine)?;
            let declared = &self.symbols.machines[machine].transitions[transition];

            let read = Id(format!("read.{}.inspect", context.name));
            let lookup = Id(format!("output.{}.lookup", context.name));

            let mut exported = key.components.clone();

            exported.push(ValueRef {
                source: ValueSource::TransactionRead(read.clone()),
                path: state.clone(),
            });

            let inspect = Transaction {
                id: Id(format!("tx.{}.inspect", context.name)),
                data_model: Some(target.data_model.clone()),
                isolation: TransactionIsolation::ReadCommitted,
                idempotency: IdempotencyGuarantee::DeduplicatedBy { key: key.clone() },
                requirements: Default::default(),
                steps: inspect_steps(target, &read, &state, &lookup, exported),
            };

            let in_state = |state_id: &Id| Condition::Eq {
                value: ValueRef {
                    source: ValueSource::TransactionOutput(lookup.clone()),
                    path: state.clone(),
                },
                equals: SelectorValue::Literal(Literal::String(state_id.0.clone())),
            };

            let applies = match declared.from.iter().collect::<Vec<_>>().as_slice() {
                [only] => in_state(only),
                many => Condition::Not {
                    condition: Box::new(Condition::And {
                        conditions: many
                            .iter()
                            .map(|state_id| Condition::Not {
                                condition: Box::new(in_state(state_id)),
                            })
                            .collect(),
                    }),
                },
            };

            let mut then = vec![OperationStep::Transaction(ExecuteTransaction {
                transaction: main,
                rejected: Some(OperationBlock {
                    steps: vec![OperationStep::Complete],
                }),
            })];

            then.extend(success);

            let decide = OperationStep::Branch(Branch {
                condition: applies,
                then: OperationBlock { steps: then },
                otherwise: Some(OperationBlock {
                    steps: vec![refused(context, refusal)],
                }),
            });

            // Already done: the request's `ok`, from what it carries.
            let decide = if *already_ok {
                OperationStep::Branch(Branch {
                    condition: in_state(&declared.to),
                    then: OperationBlock {
                        steps: vec![ok_from_input(context)],
                    },
                    otherwise: Some(OperationBlock {
                        steps: vec![decide],
                    }),
                })
            } else {
                decide
            };

            return Ok(OperationBlock {
                steps: vec![
                    OperationStep::Transaction(ExecuteTransaction {
                        rejected: inspect.rejects().then(|| OperationBlock {
                            steps: vec![OperationStep::Complete],
                        }),
                        transaction: inspect,
                    }),
                    decide,
                ],
            });
        }

        // Otherwise one transaction. A rejection — a version mismatch, a
        // stale position, a transition from the wrong state — returns the
        // refusal a request declared for it, or completes.
        let rejected = rejects.then(|| OperationBlock {
            steps: vec![match &self.guarded {
                Some((_, _, _, Some(refusal), _)) if context.request => refused(context, refusal),
                _ => OperationStep::Complete,
            }],
        });

        let mut steps = vec![OperationStep::Transaction(ExecuteTransaction {
            transaction: main,
            rejected,
        })];

        steps.extend(success);

        Ok(OperationBlock { steps })
    }

    /// What the result carries that only the transaction holds.
    fn output(
        &self,
        sketch: &OperationSketch,
    ) -> Result<Option<(Id, Id, Vec<ValueRef>)>, CompileError> {
        let context = &self.context;

        let Some((schema, fields)) = context.ok.as_ref().filter(|_| context.request) else {
            return Ok(None);
        };

        let from: Vec<ValueRef> = match &sketch.returns {
            Some(values) => values
                .iter()
                .map(|value| self.resolve(value))
                .collect::<Result<_, _>>()?,

            None => fields
                .iter()
                .filter_map(|field| {
                    if context.carries.contains(field) {
                        return Some(input(context, field));
                    }

                    if let Some(target) = self
                        .found
                        .values()
                        .find(|target| target.fields.contains(field))
                    {
                        return Some(ValueRef {
                            source: ValueSource::TransactionRead(target.read.clone()),
                            path: FieldPath(vec![field.clone()]),
                        });
                    }

                    self.created
                        .contains(field)
                        .then(|| self.created_from.first().cloned())
                        .flatten()
                })
                .collect(),
        };

        let from = if from.is_empty() {
            match &context.key {
                Some(key) => key.components.clone(),
                None => return Ok(None),
            }
        } else {
            from
        };

        Ok(Some((
            Id(format!("output.{}.result", context.name)),
            schema.clone(),
            from,
        )))
    }
}

/// One input's contract, as compilation sees it.
struct Shape {
    request: bool,
    carries: BTreeSet<String>,
    ok: Option<(Id, BTreeSet<String>)>,
    errors: BTreeMap<Id, BTreeSet<String>>,
    key: Option<IdempotencyKey>,
}

/// The fields every one of `schemas` carries, and the identity key they
/// share when each maps it to the same fields.
fn messages(
    schemas: &BTreeSet<Id>,
    identity: Option<&MessageIdentity>,
    input: &Id,
    symbols: &Symbols,
) -> Result<(BTreeSet<String>, Option<IdempotencyKey>), CompileError> {
    let mut carries: Option<BTreeSet<String>> = None;
    let mut keys: Option<Option<IdempotencyKey>> = None;

    for schema in schemas {
        let fields = symbols.fields(schema);

        carries = Some(match carries {
            None => fields,
            Some(seen) => seen.intersection(&fields).cloned().collect(),
        });

        let key = identity.and_then(|identity| message_key(identity, schema, input));

        keys = Some(match keys {
            None => key,
            Some(seen) if seen == key => seen,
            Some(_) => None,
        });
    }

    let Some(carries) = carries else {
        return fail(format!("`{input}` consumes no message schema"));
    };

    Ok((carries, keys.flatten()))
}

fn shape(input_id: &Id, input: &Input, symbols: &Symbols) -> Result<Shape, CompileError> {
    Ok(match input {
        Input::Request(request) => Shape {
            request: true,
            carries: symbols.fields(&request.schema),
            ok: Some((
                request.result.ok.clone(),
                symbols.fields(&request.result.ok),
            )),
            errors: request
                .result
                .errors
                .iter()
                .map(|(class, error)| (class.clone(), symbols.fields(&error.schema)))
                .collect(),
            key: match &request.identity {
                RequestIdentity::Keyed(key) => Some(IdempotencyKey {
                    components: key
                        .fields
                        .iter()
                        .map(|field| ValueRef {
                            source: ValueSource::Input(input_id.clone()),
                            path: field.clone(),
                        })
                        .collect(),
                }),
                RequestIdentity::Unspecified => None,
            },
        },

        Input::Subscription(subscription) => {
            let topic = symbols.topics.get(&subscription.topic);

            let schemas = match &subscription.messages {
                MessageSelector::Only(schemas) => schemas.clone(),
                MessageSelector::All => topic
                    .map(|topic| topic.messages.clone())
                    .unwrap_or_default(),
            };

            let (carries, key) = messages(
                &schemas,
                topic.map(|topic| &topic.message_identity),
                input_id,
                symbols,
            )?;

            Shape {
                request: false,
                carries,
                ok: None,
                errors: BTreeMap::new(),
                key,
            }
        }

        Input::Outbox(consumer) => {
            let Some((_, outbox)) = symbols.outboxes.get(&consumer.outbox) else {
                return fail(format!("`{}` is not a declared outbox", consumer.outbox));
            };

            let (carries, key) = messages(
                &outbox.messages,
                Some(&outbox.message_identity),
                input_id,
                symbols,
            )?;

            Shape {
                request: false,
                carries,
                ok: None,
                errors: BTreeMap::new(),
                key,
            }
        }
    })
}

fn context<'a>(
    operation: &'a Id,
    draft: &'a super::DraftOperation,
    symbols: &'a Symbols,
) -> Result<Context<'a>, CompileError> {
    let name = operation
        .0
        .strip_prefix("operation.")
        .unwrap_or(&operation.0)
        .to_string();

    let mut shapes: BTreeMap<Id, Shape> = BTreeMap::new();

    for (input_id, input) in &draft.inputs {
        shapes.insert(input_id.clone(), shape(input_id, input, symbols)?);
    }

    if shapes.is_empty() {
        return fail(format!("{operation} declares no input"));
    }

    // The input the program's result belongs to: with several request
    // inputs, the sketch names it (a path is the input's it returns for,
    // §16); otherwise the only request input, or the first input.
    let named = draft
        .sketch
        .as_ref()
        .and_then(|sketch| sketch.returns_for.clone());

    let requests: Vec<&Id> = shapes
        .iter()
        .filter(|(_, shape)| shape.request)
        .map(|(id, _)| id)
        .collect();

    let primary = match (&named, requests.as_slice()) {
        (Some(input), _) if draft.inputs.contains_key(input) => input.clone(),
        (Some(input), _) => {
            return fail(format!("`{input}` is not an input of {operation}"));
        }
        (None, [only]) => (*only).clone(),
        (None, []) => shapes.keys().next().expect("an input").clone(),
        (None, _) => {
            return fail(format!(
                "{operation} has several request inputs; name the one its result is for with \
                 `returns_for`"
            ));
        }
    };

    let single = shapes.len() == 1;

    let inputs: BTreeMap<Id, BTreeSet<String>> = shapes
        .iter()
        .map(|(id, shape)| (id.clone(), shape.carries.clone()))
        .collect();

    let shape = shapes.remove(&primary).expect("an input");

    let input = draft
        .inputs
        .keys()
        .find(|id| **id == primary)
        .expect("declared");

    Ok(Context {
        operation,
        name,
        input,
        request: shape.request,
        carries: shape.carries,
        ok: shape.ok,
        errors: shape.errors,

        // Several inputs have no one identity to key commits by; a
        // sketch states its `commit_key` instead.
        key: if single { shape.key } else { None },
        inputs,
        single,
    })
}

fn input(context: &Context<'_>, field: &str) -> ValueRef {
    ValueRef {
        source: ValueSource::Input(context.input.clone()),
        path: FieldPath(vec![field.to_string()]),
    }
}

/// `input.field` or `alias.field`, as a value reference.
fn resolve(
    context: &Context<'_>,
    found: &BTreeMap<String, Found>,
    results: &BTreeMap<String, ValueSource>,
    reference: &str,
) -> Result<ValueRef, CompileError> {
    let Some((source, field)) = reference.split_once('.') else {
        return fail(format!(
            "`{reference}` is not a value: write `input.<field>` or `<found record>.<field>`"
        ));
    };

    // A value of a named input: the input's id, then the field.
    if let Some((input_id, carries)) = context
        .inputs
        .iter()
        .filter(|(id, _)| reference.starts_with(&format!("{}.", id.0)))
        .max_by_key(|(id, _)| id.0.len())
    {
        let rest = &reference[input_id.0.len() + 1..];
        let path = FieldPath(rest.split('.').map(str::to_string).collect());
        let head = path.0.first().cloned().unwrap_or_default();

        if !carries.contains(&head) {
            return fail(format!("`{input_id}` carries no `{head}`"));
        }

        return Ok(ValueRef {
            source: ValueSource::Input(input_id.clone()),
            path,
        });
    }

    let path = FieldPath(field.split('.').map(str::to_string).collect());
    let head = path.0.first().cloned().unwrap_or_default();

    if source == "input" {
        if !context.single {
            return fail(format!(
                "{} has several inputs: write `<input id>.{field}` to say whose",
                context.operation
            ));
        }

        if !context.carries.contains(&head) {
            return fail(format!(
                "the input of {} carries no `{head}`",
                context.operation
            ));
        }

        return Ok(ValueRef {
            source: ValueSource::Input(context.input.clone()),
            path,
        });
    }

    if let Some(answer) = results.get(source) {
        return Ok(ValueRef {
            source: answer.clone(),
            path,
        });
    }

    let Some(target) = found.get(source) else {
        return fail(format!(
            "`{reference}` names `{source}`, which is neither `input`, a record found before it, \
             nor an answer in scope"
        ));
    };

    if !target.fields.contains(&head) {
        return fail(format!("`{}` has no field `{head}`", target.record));
    }

    if target.open {
        return Ok(ValueRef {
            source: ValueSource::TransactionRead(target.read.clone()),
            path,
        });
    }

    match &target.exported {
        Some(output) => Ok(ValueRef {
            source: ValueSource::TransactionOutput(output.clone()),
            path,
        }),

        None => fail(format!(
            "`{reference}` is used where the transaction that found `{source}` is out of scope"
        )),
    }
}

fn derivation(
    context: &Context<'_>,
    found: &BTreeMap<String, Found>,
    results: &BTreeMap<String, ValueSource>,
    from: &[String],
) -> Result<Derivation, CompileError> {
    if from.is_empty() {
        return Ok(Derivation::Unspecified);
    }

    Ok(Derivation::Deterministic {
        from: from
            .iter()
            .map(|reference| resolve(context, found, results, reference))
            .collect::<Result<_, _>>()?,
    })
}

/// Each read selects exactly the fields later steps use — the version
/// only when a comparison names it.
fn narrow_reads(transaction: &mut Transaction, found: &BTreeMap<String, Found>) {
    let mut used: BTreeMap<Id, BTreeSet<FieldPath>> = BTreeMap::new();

    for step in &transaction.steps {
        for value in step.roots() {
            if let ValueSource::TransactionRead(read) = &value.source {
                used.entry(read.clone())
                    .or_default()
                    .insert(value.path.clone());
            }
        }
    }

    for step in &mut transaction.steps {
        if let TransactionStep::Read(read) = step {
            let mut fields = used.remove(&read.bind).unwrap_or_default();

            // A read observes at least the identity it selected by.
            if fields.is_empty()
                && let Some(target) = found.values().find(|target| target.read == read.bind)
            {
                fields.extend(target.data.identity.iter().cloned());
            }

            read.fields = FieldSelection::Only(fields);
        }
    }
}

/// The inspection's body: one read of the state, and the output it is
/// decided by. Nothing guards it, and nothing needs to: a read-only
/// transaction observing one identified instance once serializes at
/// that read, so it is a protected participant of its record's conflict
/// closure without a lock or a comparison it has no mutation to carry.
fn inspect_steps(
    target: &Found,
    read: &Id,
    state: &FieldPath,
    lookup: &Id,
    exported: Vec<ValueRef>,
) -> Vec<TransactionStep> {
    vec![
        TransactionStep::Read(Read {
            bind: read.clone(),
            target: target.selector.clone(),
            fields: FieldSelection::Only([state.clone()].into()),
        }),
        TransactionStep::EstablishTransactionOutput(EstablishTransactionOutput {
            bind: lookup.clone(),
            schema: target.data.schema.clone(),
            values: Derivation::Deterministic { from: exported },
        }),
    ]
}

fn state_field(symbols: &Symbols, machine: &Id) -> Result<FieldPath, CompileError> {
    let StateMachineSubject::Object { state, .. } = &symbols
        .machines
        .get(machine)
        .ok_or_else(|| CompileError(format!("`{machine}` is not declared")))?
        .subject;

    Ok(state.clone())
}

/// A result or error payload's values: the input fields named in it,
/// else the trigger's identity, else everything the input carries.
fn payload(context: &Context<'_>, fields: &BTreeSet<String>) -> Derivation {
    let mut from: Vec<ValueRef> = fields
        .iter()
        .filter(|field| context.carries.contains(*field))
        .map(|field| input(context, field))
        .collect();

    if from.is_empty() {
        from = context
            .key
            .as_ref()
            .map(|key| key.components.clone())
            .unwrap_or_else(|| {
                context
                    .carries
                    .iter()
                    .map(|field| input(context, field))
                    .collect()
            });
    }

    Derivation::Deterministic { from }
}

fn refused(context: &Context<'_>, error: &Id) -> OperationStep {
    OperationStep::Return(Return {
        request: context.input.clone(),
        outcome: ResultOutcome::Err {
            error: error.clone(),
            values: payload(
                context,
                context.errors.get(error).unwrap_or(&BTreeSet::new()),
            ),
        },
    })
}

fn ok_from_input(context: &Context<'_>) -> OperationStep {
    OperationStep::Return(Return {
        request: context.input.clone(),
        outcome: ResultOutcome::Ok {
            values: payload(
                context,
                &context
                    .ok
                    .as_ref()
                    .map(|(_, fields)| fields.clone())
                    .unwrap_or_default(),
            ),
        },
    })
}

/// The end of a successful path: the `ok` result, from the output when
/// there is one, or `complete`.
fn finish(context: &Context<'_>, output: Option<&Id>) -> OperationStep {
    if !context.request {
        return OperationStep::Complete;
    }

    let Some(output) = output else {
        return ok_from_input(context);
    };

    let fields = context
        .ok
        .as_ref()
        .map(|(_, fields)| fields.clone())
        .unwrap_or_default();

    OperationStep::Return(Return {
        request: context.input.clone(),
        outcome: ResultOutcome::Ok {
            values: Derivation::Deterministic {
                from: fields
                    .iter()
                    .map(|field| ValueRef {
                        source: ValueSource::TransactionOutput(output.clone()),
                        path: FieldPath(vec![field.clone()]),
                    })
                    .collect(),
            },
        },
    })
}

/// A step's kind, for messages.
fn step_name(step: &SketchStep) -> &'static str {
    match step {
        SketchStep::Find { .. } => "find",
        SketchStep::Update { .. } => "update",
        SketchStep::Create { .. } => "create",
        SketchStep::Upsert { .. } => "upsert",
        SketchStep::Transition { .. } => "transition",
        SketchStep::Advance { .. } => "advance",
        SketchStep::Enqueue { .. } => "enqueue",
        SketchStep::Publish { .. } => "publish",
        SketchStep::Call { .. } => "call",
        SketchStep::Delete { .. } => "delete",
        SketchStep::Fence { .. } => "fence",
        SketchStep::Request { .. } => "request",
        SketchStep::When { .. } => "when",
        SketchStep::Reject { .. } => "reject",
        SketchStep::Abandon => "abandon",
        SketchStep::Parallel { .. } => "parallel",
        SketchStep::Race { .. } => "race",
        SketchStep::Start { .. } => "start",
        SketchStep::Answer { .. } => "answer",
    }
}

impl Compiler<'_> {
    /// Protects every record the open transaction changes or deletes.
    fn protect(&mut self) {
        // A changed single record that carries a version conditions its
        // first change on the version its read observed: an update
        // becomes a compare-and-set, and a transition or cursor advance
        // carries the comparison. Every later change then runs under the
        // write protection that guarded mutation holds. Everything else
        // that changes or is deleted — an unversioned record, a set of
        // instances, a record only deleted or fenced — is held under an
        // exclusive lock from before its read.
        let changed: BTreeSet<&String> = self.mutated.iter().collect();
        let deleted: BTreeSet<&String> = self.deleted.iter().collect();

        let mut held: Vec<String> = Vec::new();

        for alias in changed
            .iter()
            .chain(deleted.iter())
            .collect::<BTreeSet<_>>()
        {
            let target = &self.found[*alias];

            if changed.contains(alias)
                && target.single
                && let Some(version) = &target.data.version
                && self.steps.iter().any(
                    |step| matches!(step, TransactionStep::Read(read) if read.bind == target.read),
                )
            {
                // A fence is no such guard: on an equal token it writes
                // nothing and holds no protection.
                let Some(first) = self.steps.iter().position(|step| {
                    matches!(
                        step,
                        TransactionStep::Update(_)
                            | TransactionStep::Transition(_)
                            | TransactionStep::AdvanceCursor(_)
                    ) && step.selector() == Some(&target.selector)
                }) else {
                    held.push((*alias).clone());
                    continue;
                };

                let observed = CompareCondition {
                    field: version.field.clone(),
                    expected: SelectorValue::Value(ValueRef {
                        source: ValueSource::TransactionRead(target.read.clone()),
                        path: version.field.clone(),
                    }),
                };

                let guarded = match self.steps[first].clone() {
                    TransactionStep::Update(update) => {
                        TransactionStep::CompareAndSet(CompareAndSet {
                            target: update.target,
                            compare: vec![observed],
                            fields: update.fields,
                            values: update.values,
                        })
                    }

                    mut guard => {
                        match &mut guard {
                            TransactionStep::Transition(transition) => {
                                transition.compare.push(observed)
                            }
                            TransactionStep::AdvanceCursor(advance) => {
                                advance.compare.push(observed)
                            }
                            _ => unreachable!("matched above"),
                        }

                        guard
                    }
                };

                self.steps[first] = guarded;

                continue;
            }

            held.push((*alias).clone());
        }

        for alias in held {
            let target = &self.found[&alias];

            // An author's lock on the selection is kept, and made
            // exclusive: it now protects a change.
            if let Some(lock) = self.steps.iter_mut().find_map(|step| match step {
                TransactionStep::Lock(lock) if lock.target == target.selector => Some(lock),
                _ => None,
            }) {
                lock.mode = LockMode::Exclusive;
                continue;
            }

            let read = self
                .steps
                .iter()
                .position(
                    |step| matches!(step, TransactionStep::Read(read) if read.bind == target.read),
                )
                .expect("a found record has a read");

            self.steps.insert(
                read,
                TransactionStep::Lock(Lock {
                    target: target.selector.clone(),
                    mode: LockMode::Exclusive,
                    order: LockOrder::Unspecified,
                }),
            );
        }
    }
}

// ---------------------------------------------------------------------
// The general path
// ---------------------------------------------------------------------

/// Whether a sketch is one transaction with at most one transition, or
/// a sequence of effects: the shapes [`Compiler::assemble`] and the
/// effect-only path compile, with the inspect-then-decide shape where it
/// applies. Everything else takes the general path.
fn is_simple(sketch: &OperationSketch, symbols: &Symbols) -> bool {
    let plain = |step: &SketchStep| match step {
        SketchStep::Publish {
            detached, durable, ..
        } => !detached && !durable,
        SketchStep::Call {
            durable, detached, ..
        } => !durable && !detached,
        SketchStep::Advance { otherwise, .. } => otherwise.is_none(),
        _ => true,
    };

    if !sketch.on_rejected.is_empty() || !sketch.steps.iter().all(plain) {
        return false;
    }

    let effects_only = sketch
        .steps
        .iter()
        .all(|step| matches!(step, SketchStep::Publish { .. } | SketchStep::Call { .. }));

    if effects_only {
        return true;
    }

    let shapes = sketch.steps.iter().all(|step| {
        matches!(
            step,
            SketchStep::Find { .. }
                | SketchStep::Update { .. }
                | SketchStep::Create { .. }
                | SketchStep::Upsert { .. }
                | SketchStep::Transition { .. }
                | SketchStep::Advance { .. }
                | SketchStep::Enqueue { .. }
                | SketchStep::Publish { .. }
        )
    });

    let transitions = sketch
        .steps
        .iter()
        .filter(|step| matches!(step, SketchStep::Transition { .. }))
        .count();

    let models: BTreeSet<&Id> = sketch
        .steps
        .iter()
        .filter_map(|step| match step {
            SketchStep::Find { record, .. }
            | SketchStep::Create { record, .. }
            | SketchStep::Upsert { record, .. } => {
                symbols.objects.get(record).map(|(model, _)| model)
            }
            SketchStep::Enqueue { outbox, .. } => {
                symbols.outboxes.get(outbox).map(|(model, _)| model)
            }
            _ => None,
        })
        .collect();

    shapes && transitions <= 1 && models.len() <= 1
}

/// Everything after a point of a sketch, as text: what a closing
/// transaction scans for the records later steps still use.
fn later(steps: &[SketchStep], after: &str) -> String {
    serde_json::to_string(steps).expect("a sketch serializes") + after
}

/// The fields of `alias` that `text` references, as `"alias.field`.
fn referenced_fields(text: &str, alias: &str) -> BTreeSet<String> {
    let marker = format!("\"{alias}.");

    text.match_indices(&marker)
        .filter_map(|(at, _)| {
            let rest = &text[at + marker.len()..];
            let end = rest.find(['"', '.']).unwrap_or(rest.len());

            (end > 0).then(|| rest[..end].to_string())
        })
        .collect()
}

/// A call's or request's effect, its derivation, and the error classes
/// its answer may carry (none when it answers nothing).
struct Answered {
    alias: Option<String>,
    effect_id: Id,
    effect: Effect,
    values: Derivation,
    classes: Option<Vec<Id>>,
    on_ok: Vec<SketchStep>,
    on_error: BTreeMap<Id, Vec<SketchStep>>,
}

impl Compiler<'_> {
    fn general(mut self, sketch: &OperationSketch) -> Result<OperationBlock, CompileError> {
        self.general = true;
        self.on_rejected = sketch.on_rejected.clone();

        let returns = serde_json::to_string(&sketch.returns).expect("serializes");

        let (steps, _) = self.block(&sketch.steps, &returns, true, sketch)?;

        if !sketch.on_rejected.is_empty() && !self.on_rejected_used {
            return fail(
                "`on_rejected` never applies: every transaction that can be refused here already \
                 returns its transition's `otherwise` error, and a transaction no guard can refuse \
                 is never rejected (creating a record whose identity exists is not a rejection). \
                 Drop it, or drop the `otherwise` it would replace",
            );
        }

        Ok(OperationBlock { steps })
    }

    /// The data model a transactional step works in.
    fn data_model_of(&self, step: &SketchStep) -> Result<Id, CompileError> {
        let of_alias = |record: &String| -> Result<Id, CompileError> {
            let Some(target) = self.found.get(record) else {
                return fail(format!("`{record}` is used before a find names it"));
            };

            if !target.open {
                return fail(format!(
                    "`{record}` was found in an earlier transaction; find it again where it \
                     changes"
                ));
            }

            Ok(target.data_model.clone())
        };

        match step {
            SketchStep::Find { record, .. }
            | SketchStep::Create { record, .. }
            | SketchStep::Upsert { record, .. } => self
                .symbols
                .objects
                .get(record)
                .map(|(model, _)| model.clone())
                .ok_or_else(|| CompileError(format!("`{record}` is not a declared data object"))),

            SketchStep::Enqueue { outbox, .. } => self
                .symbols
                .outboxes
                .get(outbox)
                .map(|(model, _)| model.clone())
                .ok_or_else(|| CompileError(format!("`{outbox}` is not a declared outbox"))),

            SketchStep::Update { record, .. }
            | SketchStep::Transition { record, .. }
            | SketchStep::Advance { record, .. }
            | SketchStep::Delete { record }
            | SketchStep::Fence { record, .. } => of_alias(record),

            other => fail(format!(
                "{} is not a step of a transaction",
                step_name(other)
            )),
        }
    }

    /// Seals the open transaction, when there is one: the transaction,
    /// then the intents it established. `later` is everything after it,
    /// which decides what it exports; `last` is the sketch when this is
    /// the operation's last transaction, which then establishes the
    /// `ok` result's output.
    fn seal(
        &mut self,
        later: &str,
        last: Option<&OperationSketch>,
    ) -> Result<Vec<OperationStep>, CompileError> {
        if self.steps.is_empty() {
            return Ok(Vec::new());
        }

        let data_model = match self.data_models.iter().collect::<Vec<_>>()[..] {
            [] => None,
            [data_model] => Some(data_model.clone()),
            _ => {
                return fail(format!(
                    "one transaction touches {} data models",
                    self.data_models.len()
                ));
            }
        };

        self.protect();

        // Each record later steps still use is exported, by its own
        // schema, from what this transaction read.
        let mut exports = Vec::new();

        for (alias, target) in &self.found {
            if !target.open {
                continue;
            }

            let fields = referenced_fields(later, alias);

            if fields.is_empty() {
                continue;
            }

            let bind = Id(format!("output.{}.{alias}", self.context.name));

            self.steps.push(TransactionStep::EstablishTransactionOutput(
                EstablishTransactionOutput {
                    bind: bind.clone(),
                    schema: target.data.schema.clone(),
                    values: Derivation::Deterministic {
                        from: fields
                            .iter()
                            .filter(|field| target.fields.contains(*field))
                            .map(|field| ValueRef {
                                source: ValueSource::TransactionRead(target.read.clone()),
                                path: FieldPath(vec![field.clone()]),
                            })
                            .collect(),
                    },
                },
            ));

            exports.push((alias.clone(), bind));
        }

        if let Some(sketch) = last
            && let Some((bind, schema, from)) = self.output(sketch)?
        {
            self.steps.push(TransactionStep::EstablishTransactionOutput(
                EstablishTransactionOutput {
                    bind: bind.clone(),
                    schema,
                    values: Derivation::Deterministic { from },
                },
            ));

            self.final_output = Some(bind);
        }

        self.segments += 1;

        let id = if self.segments == 1 {
            format!("tx.{}", self.context.name)
        } else {
            format!("tx.{}.{}", self.context.name, self.segments)
        };

        let mut transaction = Transaction {
            id: Id(id),
            data_model,
            isolation: self
                .segment_isolation
                .take()
                .unwrap_or(TransactionIsolation::ReadCommitted),
            idempotency: match &self.context.key {
                Some(key) => IdempotencyGuarantee::DeduplicatedBy { key: key.clone() },
                None => IdempotencyGuarantee::Unspecified,
            },
            requirements: Default::default(),
            steps: std::mem::take(&mut self.steps),
        };

        narrow_reads(&mut transaction, &self.found);

        // A rejected transaction committed nothing: nothing it found is
        // in scope on that course.
        for target in self.found.values_mut() {
            target.open = false;
        }

        let rejected = if !transaction.rejects() {
            None
        } else if let Some(refusal) = self.refusals.first().filter(|_| self.context.request) {
            Some(OperationBlock {
                steps: vec![refused(&self.context, refusal)],
            })
        } else if !self.on_rejected.is_empty() {
            self.on_rejected_used = true;

            let steps = self.on_rejected.clone();

            let course = OperationSketch {
                steps: steps.clone(),
                returns: None,
                isolation: None,
                commit_key: None,
                returns_for: None,
                on_rejected: Vec::new(),
            };

            let outer = std::mem::take(&mut self.on_rejected);
            let (mut block, ended) = self.block(&steps, "", false, &course)?;
            self.on_rejected = outer;

            if !ended {
                if self.context.request {
                    return fail("`on_rejected` must end a request: add a reject");
                }

                block.push(OperationStep::Complete);
            }

            Some(OperationBlock { steps: block })
        } else {
            Some(OperationBlock {
                steps: vec![OperationStep::Complete],
            })
        };

        let mut out = vec![OperationStep::Transaction(ExecuteTransaction {
            transaction,
            rejected,
        })];

        let after_commit: Vec<(Id, bool)> = self.after_commit.drain(..).collect();

        out.extend(
            after_commit
                .into_iter()
                .map(|(intent, detached)| execute_intent(intent, detached)),
        );

        for target in self.found.values_mut() {
            target.open = false;
        }

        for (alias, bind) in exports {
            if let Some(target) = self.found.get_mut(&alias) {
                target.exported = Some(bind);
            }
        }

        self.mutated.clear();
        self.deleted.clear();
        self.data_models.clear();
        self.guarded = None;
        self.refusals.clear();

        Ok(out)
    }

    /// A call's or request's effect.
    fn answered(&mut self, step: &SketchStep) -> Result<Answered, CompileError> {
        self.key_override = match step {
            SketchStep::Enqueue { key, .. }
            | SketchStep::Publish { key, .. }
            | SketchStep::Request { key, .. } => self.key_of(key)?,
            _ => None,
        };

        match step {
            SketchStep::Call {
                name,
                alias,
                identity,
                duplicates,
                result_replay,
                result,
                from,
                on_ok,
                on_error,
                ..
            } => {
                let (effect_id, _) = self.effect_ids("call");

                let identity = match identity {
                    Some(values) => ExternalIdentity::Keyed {
                        key: ExternalIdentityKey {
                            components: values
                                .iter()
                                .map(|value| self.resolve(value))
                                .collect::<Result<_, _>>()?,
                        },
                    },
                    None => ExternalIdentity::Unspecified,
                };

                let result_type = result.as_ref().map(|result| ResultType {
                    ok: result.ok.clone(),
                    errors: result
                        .errors
                        .iter()
                        .map(|(class, error)| (class.clone(), error.declared()))
                        .collect(),
                });

                Ok(Answered {
                    alias: alias.clone(),
                    classes: result
                        .as_ref()
                        .map(|result| result.errors.keys().cloned().collect()),
                    effect: Effect::External(ExternalEffect {
                        name: name.clone(),
                        identity,
                        idempotency: *duplicates,
                        result_replay: *result_replay,
                        result: result_type,
                    }),
                    values: self.derivation(&from.refs())?,
                    effect_id,
                    on_ok: on_ok.clone(),
                    on_error: on_error.clone(),
                })
            }

            SketchStep::Request {
                operation,
                input,
                alias,
                retry,
                from,
                on_ok,
                on_error,
                ..
            } => {
                let Some(inputs) = self.symbols.requests.get(operation) else {
                    return fail(format!("`{operation}` is not a declared operation"));
                };

                let (input, request) = match input {
                    Some(input) => match inputs.get(input) {
                        Some(request) => (input.clone(), request.clone()),
                        None => {
                            return fail(format!(
                                "`{input}` is not a request input of `{operation}`"
                            ));
                        }
                    },

                    None => match inputs.iter().collect::<Vec<_>>()[..] {
                        [(input, request)] => (input.clone(), request.clone()),
                        _ => {
                            return fail(format!(
                                "`{operation}` has {} request inputs; name the one to invoke",
                                inputs.len()
                            ));
                        }
                    },
                };

                let (effect_id, _) = self.effect_ids("request");

                // The trigger's identity carried into the target's own,
                // so a duplicate of this invocation is one request.
                let propagation = match (
                    self.key_override.as_ref().or(self.context.key.as_ref()),
                    &request.identity,
                ) {
                    (Some(key), RequestIdentity::Keyed(target))
                        if target.fields.len() == key.components.len() =>
                    {
                        vec![IdempotencyKeyPropagation {
                            source: key.clone(),
                            target: IdempotencyKey {
                                components: target
                                    .fields
                                    .iter()
                                    .map(|field| ValueRef {
                                        source: ValueSource::Effect(effect_id.clone()),
                                        path: field.clone(),
                                    })
                                    .collect(),
                            },
                        }]
                    }
                    _ => Vec::new(),
                };

                Ok(Answered {
                    alias: alias.clone(),
                    classes: Some(request.result.errors.keys().cloned().collect()),
                    effect: Effect::Request(crate::spec::RequestEffect {
                        target: crate::spec::RequestTarget {
                            operation: operation.clone(),
                            input,
                        },
                        schema: request.schema.clone(),
                        retry: *retry,
                        idempotency_key_propagation: propagation,
                    }),
                    values: self.checked(from, &request.schema)?,
                    effect_id,
                    on_ok: on_ok.clone(),
                    on_error: on_error.clone(),
                })
            }

            SketchStep::Publish {
                topic,
                schema,
                from,
                ..
            } => {
                let (effect_id, _, publication) = self.publication(topic, schema)?;

                Ok(Answered {
                    alias: None,
                    classes: None,
                    effect: Effect::Publication(publication),
                    values: self.checked(from, schema)?,
                    effect_id,
                    on_ok: Vec::new(),
                    on_error: BTreeMap::new(),
                })
            }

            other => fail(format!("{} is not an effect", step_name(other))),
        }
    }

    fn is_reference(&self, text: &str) -> bool {
        text.split_once('.').is_some_and(|(source, _)| {
            source == "input"
                || self.found.contains_key(source)
                || self.results.contains_key(source)
        })
    }

    fn condition(&self, condition: &SketchCondition) -> Result<Condition, CompileError> {
        Ok(match condition {
            SketchCondition::Equals(operands) => {
                let [left, right] = operands.as_slice() else {
                    return fail("`equals` compares exactly two operands");
                };

                let Some(left) = left.as_str() else {
                    return fail("`equals` begins with a value reference");
                };

                let value = self.resolve(left)?;

                let equals = match right {
                    serde_json::Value::String(text) if self.is_reference(text) => {
                        SelectorValue::Value(self.resolve(text)?)
                    }
                    serde_json::Value::String(text) => {
                        SelectorValue::Literal(Literal::String(text.clone()))
                    }
                    serde_json::Value::Bool(flag) => SelectorValue::Literal(Literal::Bool(*flag)),
                    serde_json::Value::Number(number) => match number.as_i64() {
                        Some(number) => SelectorValue::Literal(Literal::Int(number)),
                        None => return fail("`equals` compares with integers only"),
                    },
                    _ => {
                        return fail(
                            "`equals` compares with a value, a string, an integer or a boolean",
                        );
                    }
                };

                Condition::Eq { value, equals }
            }

            SketchCondition::Unspecified(_) => Condition::Unspecified,

            SketchCondition::Present(value) => Condition::Present {
                value: self.resolve(value)?,
            },

            SketchCondition::Not(inner) => Condition::Not {
                condition: Box::new(self.condition(inner)?),
            },

            SketchCondition::All(all) => Condition::And {
                conditions: all
                    .iter()
                    .map(|condition| self.condition(condition))
                    .collect::<Result<_, _>>()?,
            },

            SketchCondition::Any(any) => Condition::Not {
                condition: Box::new(Condition::And {
                    conditions: any
                        .iter()
                        .map(|condition| {
                            Ok(Condition::Not {
                                condition: Box::new(self.condition(condition)?),
                            })
                        })
                        .collect::<Result<_, CompileError>>()?,
                }),
            },
        })
    }

    /// The end of the operation's successful course.
    fn finish_general(&self, sketch: &OperationSketch) -> Result<OperationStep, CompileError> {
        if !self.context.request {
            return Ok(OperationStep::Complete);
        }

        if let Some(output) = &self.final_output {
            return Ok(finish(&self.context, Some(output)));
        }

        Ok(match &sketch.returns {
            Some(values) => OperationStep::Return(Return {
                request: self.context.input.clone(),
                outcome: ResultOutcome::Ok {
                    values: self.derivation(values)?,
                },
            }),
            None => ok_from_input(&self.context),
        })
    }

    /// Compiles `steps`, which `after` follows. Returns the steps and
    /// whether every course through them ends the operation.
    fn block(
        &mut self,
        steps: &[SketchStep],
        after: &str,
        top: bool,
        sketch: &OperationSketch,
    ) -> Result<(Vec<OperationStep>, bool), CompileError> {
        let in_scope: BTreeSet<String> = self.found.keys().cloned().collect();

        let mut out = Vec::new();

        for (index, step) in steps.iter().enumerate() {
            let here = later(&steps[index..], after);
            let rest = later(&steps[index + 1..], after);
            let last_step = index + 1 == steps.len();

            match step {
                step if transactional(step) => {
                    let model = self.data_model_of(step)?;

                    if !self.steps.is_empty() && !self.data_models.contains(&model) {
                        out.extend(self.seal(&here, None)?);
                    }

                    self.record_step(step)?;
                }

                SketchStep::Publish { .. } if !self.steps.is_empty() => self.record_step(step)?,

                SketchStep::Publish { .. }
                | SketchStep::Call { .. }
                | SketchStep::Request { .. } => {
                    let (durable, detached) = match step {
                        SketchStep::Call {
                            durable, detached, ..
                        }
                        | SketchStep::Request {
                            durable, detached, ..
                        } => (*durable, *detached),
                        SketchStep::Publish {
                            detached, durable, ..
                        } => (*durable, *detached),
                        _ => (false, false),
                    };

                    // A durable effect is arranged in the open
                    // transaction, as an intent its commit establishes,
                    // and made once that commits.
                    // With no record step before it, the transaction
                    // that arranges it holds nothing else: an
                    // artifact-only transaction, of no data model.
                    let launch = if durable {
                        let answered = self.answered(step)?;
                        let intent = Id(answered.effect_id.0.replacen("effect.", "intent.", 1));

                        self.steps.push(TransactionStep::EstablishEffectIntent(
                            EstablishEffectIntent {
                                bind: intent.clone(),
                                effect_id: answered.effect_id.clone(),
                                effect: answered.effect.clone(),
                                values: answered.values.clone(),
                            },
                        ));

                        out.extend(self.seal(&rest, None)?);

                        (answered, Launch::Intent(intent))
                    } else {
                        out.extend(self.seal(&here, None)?);

                        let answered = self.answered(step)?;

                        let launch = Launch::Direct {
                            effect_id: answered.effect_id.clone(),
                            effect: answered.effect.clone(),
                            values: answered.values.clone(),
                        };

                        (answered, launch)
                    };

                    let (answered, launch) = launch;

                    if detached {
                        if !answered.on_ok.is_empty() || !answered.on_error.is_empty() {
                            return fail(format!(
                                "a detached {} is not waited for, so it cannot act on its answer",
                                step_name(step)
                            ));
                        }

                        self.effects += 1;

                        out.push(launch.detached(Id(format!(
                            "handle.{}.{}",
                            self.context.name, self.effects
                        ))));

                        continue;
                    }

                    let used_later = answered
                        .alias
                        .as_ref()
                        .is_some_and(|alias| !referenced_fields(&rest, alias).is_empty());

                    let acts =
                        !answered.on_ok.is_empty() || !answered.on_error.is_empty() || used_later;

                    let Some(classes) = answered.classes.clone().filter(|_| acts) else {
                        if acts {
                            return fail("a step acts on an answer its effect does not declare");
                        }

                        out.push(launch.waited(None));

                        continue;
                    };

                    let name = answered
                        .alias
                        .clone()
                        .unwrap_or_else(|| format!("answer_{}", self.effects));

                    let bind = Id(format!("result.{}.{name}", self.context.name));

                    out.push(launch.waited(Some(bind.clone())));

                    out.push(self.matched(
                        &bind,
                        &name,
                        &answered.alias,
                        &classes,
                        &answered.on_ok,
                        &answered.on_error,
                        used_later,
                        &steps[index + 1..],
                        &rest,
                        after,
                        top,
                        sketch,
                    )?);

                    if used_later {
                        self.found.retain(|alias, _| in_scope.contains(alias));

                        return Ok((out, true));
                    }
                }

                SketchStep::Answer {
                    of,
                    on_ok,
                    on_error,
                } => {
                    out.extend(self.seal(&here, None)?);

                    let Some((bind, classes)) = self.joined.get(of).cloned() else {
                        return fail(format!("`{of}` names no answer a parallel bound before it"));
                    };

                    let used_later = !referenced_fields(&rest, of).is_empty();

                    out.push(self.matched(
                        &bind,
                        of,
                        &Some(of.clone()),
                        &classes,
                        on_ok,
                        on_error,
                        used_later,
                        &steps[index + 1..],
                        &rest,
                        after,
                        top,
                        sketch,
                    )?);

                    if used_later {
                        self.found.retain(|alias, _| in_scope.contains(alias));

                        return Ok((out, true));
                    }
                }

                SketchStep::When {
                    condition,
                    then,
                    otherwise,
                } => {
                    out.extend(self.seal(&here, None)?);

                    let condition = self.condition(condition)?;

                    let (then_steps, then_ended) = self.block(then, &rest, false, sketch)?;

                    let otherwise_block = if otherwise.is_empty() {
                        None
                    } else {
                        Some(self.block(otherwise, &rest, false, sketch)?)
                    };

                    let both_end =
                        then_ended && otherwise_block.as_ref().is_some_and(|(_, ended)| *ended);

                    out.push(OperationStep::Branch(Branch {
                        condition,
                        then: OperationBlock { steps: then_steps },
                        otherwise: otherwise_block.map(|(steps, _)| OperationBlock { steps }),
                    }));

                    if both_end {
                        if !last_step {
                            return fail("nothing may follow a when whose every course ends");
                        }

                        self.found.retain(|alias, _| in_scope.contains(alias));

                        return Ok((out, true));
                    }
                }

                SketchStep::Reject { error, from } => {
                    out.extend(self.seal(&here, None)?);

                    if !self.context.request {
                        return fail("only a request rejects; a message's course just ends");
                    }

                    if !self.context.errors.contains_key(error) {
                        return fail(format!("`{error}` is not an error the request declares"));
                    }

                    if !last_step {
                        return fail("nothing may follow a reject");
                    }

                    from.check(&self.context.errors[error], &format!("the error `{error}`"))?;

                    let values = if from.is_empty() {
                        payload(&self.context, &self.context.errors[error])
                    } else {
                        self.derivation(&from.refs())?
                    };

                    out.push(OperationStep::Return(Return {
                        request: self.context.input.clone(),
                        outcome: ResultOutcome::Err {
                            error: error.clone(),
                            values,
                        },
                    }));

                    self.found.retain(|alias, _| in_scope.contains(alias));

                    return Ok((out, true));
                }

                SketchStep::Abandon => {
                    out.extend(self.seal(&here, None)?);

                    if self.context.request {
                        return fail(
                            "only a message's attempt is abandoned; a request passes a \
                             retryable error up with reject",
                        );
                    }

                    if !last_step {
                        return fail("nothing may follow an abandon");
                    }

                    out.push(OperationStep::Abandon);

                    self.found.retain(|alias, _| in_scope.contains(alias));

                    return Ok((out, true));
                }

                SketchStep::Parallel { steps: members } => {
                    out.extend(self.seal(&here, None)?);

                    if members.is_empty() {
                        return fail("parallel starts nothing");
                    }

                    let mut joins = Vec::new();

                    for member in members {
                        let Launched {
                            handle,
                            step: launch,
                            classes,
                            alias,
                        } = self.launch(member, "parallel")?;

                        // A member named with `as` has its answer bound by
                        // the join, for an `answer` step to act on.
                        let bind = match &alias {
                            Some(alias) => {
                                let Some(classes) = classes else {
                                    return fail(format!(
                                        "`{alias}` names an answer its effect does not declare"
                                    ));
                                };

                                let bind = Id(format!("result.{}.{alias}", self.context.name));

                                self.joined.insert(alias.clone(), (bind.clone(), classes));

                                Some(bind)
                            }
                            None => None,
                        };

                        out.push(launch);
                        joins.push(crate::spec::AsyncJoin { handle, bind });
                    }

                    out.push(OperationStep::JoinAll(crate::spec::JoinAll {
                        handles: joins,
                    }));
                }

                SketchStep::Start { step: member } => {
                    out.extend(self.seal(&here, None)?);

                    let launch = self.launch(member, "start")?.step;

                    out.push(launch);
                }

                SketchStep::Race {
                    steps: members,
                    alias,
                    on_ok,
                    on_error,
                } => {
                    out.extend(self.seal(&here, None)?);

                    if members.is_empty() {
                        return fail("race starts nothing");
                    }

                    let mut handles = Vec::new();
                    let mut classes: Option<Option<Vec<Id>>> = None;

                    for member in members {
                        let Launched {
                            handle,
                            step: launch,
                            classes: answers,
                            ..
                        } = self.launch(member, "race")?;

                        match &classes {
                            None => classes = Some(answers),
                            Some(seen) if *seen != answers => {
                                return fail("every racer must answer with the same result type");
                            }
                            Some(_) => {}
                        }

                        out.push(launch);
                        handles.push(handle);
                    }

                    let classes = classes.flatten();

                    let used_later = alias
                        .as_ref()
                        .is_some_and(|alias| !referenced_fields(&rest, alias).is_empty());

                    let acts = !on_ok.is_empty() || !on_error.is_empty() || used_later;

                    let bind = acts.then(|| {
                        Id(format!(
                            "result.{}.{}",
                            self.context.name,
                            alias.clone().unwrap_or_else(|| "race".to_string())
                        ))
                    });

                    // Fire-and-forget starts written right after the race
                    // belong before its match, as their authors placed them
                    // in time.
                    out.push(OperationStep::Race(crate::spec::Race {
                        handles,
                        bind: bind.clone(),
                    }));

                    if let Some(bind) = bind {
                        let Some(classes) = classes else {
                            return fail("a race acts on an answer its racers do not declare");
                        };

                        let name = alias.clone().unwrap_or_else(|| "race".to_string());

                        let mut remaining_index = index + 1;

                        while let Some(SketchStep::Start { step: member }) =
                            steps.get(remaining_index)
                        {
                            let launch = self.launch(member, "start")?.step;

                            out.push(launch);
                            remaining_index += 1;
                        }

                        let rest = later(&steps[remaining_index..], after);

                        out.push(self.matched(
                            &bind,
                            &name,
                            alias,
                            &classes,
                            on_ok,
                            on_error,
                            used_later,
                            &steps[remaining_index..],
                            &rest,
                            after,
                            top,
                            sketch,
                        )?);

                        if used_later {
                            self.found.retain(|alias, _| in_scope.contains(alias));

                            return Ok((out, true));
                        }

                        // The starts consumed above are not compiled again.
                        let (tail, ended) =
                            self.block(&steps[remaining_index..], after, top, sketch)?;

                        out.extend(tail);

                        self.found.retain(|alias, _| in_scope.contains(alias));

                        return Ok((out, ended));
                    }
                }

                other => {
                    return fail(format!("{} cannot appear here", step_name(other)));
                }
            }
        }

        let ended = if top {
            out.extend(self.seal(after, Some(sketch))?);
            out.push(self.finish_general(sketch)?);
            true
        } else {
            out.extend(self.seal(after, None)?);
            false
        };

        self.found.retain(|alias, _| in_scope.contains(alias));

        Ok((out, ended))
    }
}

impl Compiler<'_> {
    /// The match on an answer bound as `bind`. When later steps use the
    /// answer (`used_later`), they — `remaining` — continue in its `ok`
    /// arm, and every error arm ends the operation.
    #[allow(clippy::too_many_arguments)]
    fn matched(
        &mut self,
        bind: &Id,
        name: &str,
        alias: &Option<String>,
        classes: &[Id],
        on_ok: &[SketchStep],
        on_error: &BTreeMap<Id, Vec<SketchStep>>,
        used_later: bool,
        remaining: &[SketchStep],
        rest: &str,
        after: &str,
        top: bool,
        sketch: &OperationSketch,
    ) -> Result<OperationStep, CompileError> {
        for class in on_error.keys() {
            if !classes.contains(class) {
                return fail(format!(
                    "an arm acts on the error `{class}`, which the answer does not declare"
                ));
            }
        }

        let mut ok_steps = on_ok.to_vec();

        if used_later {
            ok_steps.extend(remaining.iter().cloned());
        }

        if let Some(alias) = alias {
            self.results
                .insert(alias.clone(), ValueSource::EffectResultOk(bind.clone()));
        }

        let (ok, _) = self.block(
            &ok_steps,
            if used_later { after } else { rest },
            top && used_later,
            sketch,
        )?;

        let mut errors = BTreeMap::new();

        for class in classes {
            if let Some(alias) = alias {
                self.results
                    .insert(alias.clone(), ValueSource::EffectResultErr(bind.clone()));
            }

            let arm = on_error.get(class).cloned().unwrap_or_default();

            let (mut block, ended) = self.block(&arm, rest, false, sketch)?;

            if used_later && !ended {
                if !self.context.request {
                    block.push(OperationStep::Complete);
                } else if self.context.errors.contains_key(class) {
                    block.push(refused(&self.context, class));
                } else {
                    return fail(format!(
                        "the steps after `{name}` use its answer, so its `{class}` arm must end \
                         the request: add a reject"
                    ));
                }
            }

            errors.insert(class.clone(), OperationBlock { steps: block });
        }

        if let Some(alias) = alias {
            self.results.remove(alias);
        }

        Ok(OperationStep::MatchResult(MatchResult {
            result: bind.clone(),
            ok: OperationBlock { steps: ok },
            errors,
        }))
    }

    /// Launches one effect without waiting for it.
    fn launch(&mut self, member: &SketchStep, of: &str) -> Result<Launched, CompileError> {
        let answered = self.answered(member)?;

        if !answered.on_ok.is_empty() || !answered.on_error.is_empty() {
            return fail(format!(
                "an effect started by {of} cannot act on its answer itself; give the {of} its arms"
            ));
        }

        let handle = Id(format!("handle.{}.{}", self.context.name, self.effects));

        Ok(Launched {
            handle: handle.clone(),
            step: OperationStep::ExecuteEffectAsync(crate::spec::ExecuteEffectAsync {
                handle,
                effect_id: answered.effect_id,
                effect: answered.effect,
                values: answered.values,
            }),
            classes: answered.classes,
            alias: answered.alias,
        })
    }
}

/// Executes an intent established in a transaction, once it has
/// committed: waiting for it, or — detached — not.
fn execute_intent(intent: Id, detached: bool) -> OperationStep {
    if detached {
        OperationStep::ExecuteEffectIntentAsync(crate::spec::ExecuteEffectIntentAsync {
            handle: Id(intent.0.replacen("intent.", "handle.", 1)),
            intent,
        })
    } else {
        OperationStep::ExecuteEffectIntent(ExecuteEffectIntent { intent, bind: None })
    }
}

/// An effect started without waiting: its handle, the launching step,
/// the error classes of its answer (none when it answers nothing), and
/// the name it is bound under.
struct Launched {
    handle: Id,
    step: OperationStep,
    classes: Option<Vec<Id>>,
    alias: Option<String>,
}

/// How an effect is made: directly, or by executing the intent a
/// transaction established.
enum Launch {
    Direct {
        effect_id: Id,
        effect: Effect,
        values: Derivation,
    },
    Intent(Id),
}

impl Launch {
    /// Made, and waited for.
    fn waited(self, bind: Option<Id>) -> OperationStep {
        match self {
            Self::Direct {
                effect_id,
                effect,
                values,
            } => OperationStep::ExecuteEffect(ExecuteEffect {
                effect_id,
                effect,
                values,
                bind,
            }),
            Self::Intent(intent) => {
                OperationStep::ExecuteEffectIntent(ExecuteEffectIntent { intent, bind })
            }
        }
    }

    /// Started, and not waited for.
    fn detached(self, handle: Id) -> OperationStep {
        match self {
            Self::Direct {
                effect_id,
                effect,
                values,
            } => OperationStep::ExecuteEffectAsync(crate::spec::ExecuteEffectAsync {
                handle,
                effect_id,
                effect,
                values,
            }),
            Self::Intent(intent) => {
                OperationStep::ExecuteEffectIntentAsync(crate::spec::ExecuteEffectIntentAsync {
                    intent,
                    handle,
                })
            }
        }
    }
}

impl Compiler<'_> {
    /// Records the declared error a guard's rejection returns.
    fn refusal(&mut self, otherwise: &Option<Id>) -> Result<(), CompileError> {
        if let Some(error) = otherwise {
            if !self.context.errors.contains_key(error) {
                return fail(format!("`{error}` is not an error the request declares"));
            }

            self.refusals.push(error.clone());
        }

        Ok(())
    }
}

impl Compiler<'_> {
    /// A step's `key`, resolved.
    fn key_of(&self, key: &Option<Vec<String>>) -> Result<Option<IdempotencyKey>, CompileError> {
        key.as_ref()
            .map(|values| {
                Ok(IdempotencyKey {
                    components: values
                        .iter()
                        .map(|value| self.resolve(value))
                        .collect::<Result<_, CompileError>>()?,
                })
            })
            .transpose()
    }

    /// Records the isolation a step declares for its transaction; two
    /// steps of one transaction may not disagree.
    fn declare_isolation(
        &mut self,
        isolation: Option<TransactionIsolation>,
    ) -> Result<(), CompileError> {
        let Some(isolation) = isolation else {
            return Ok(());
        };

        match self.segment_isolation {
            Some(declared) if declared != isolation => {
                fail("two steps of one transaction declare different isolation".to_string())
            }
            _ => {
                self.segment_isolation = Some(isolation);
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sketch_round_trips_through_json() {
        let sketch: OperationSketch = serde_json::from_value(serde_json::json!({
            "steps": [
                { "kind": "find", "as": "order", "record": "object.order",
                  "by": { "order_id": "input.order_id" } },
                { "kind": "transition", "record": "order", "transition": "transition.order.pay",
                  "otherwise": "not_payable" },
                { "kind": "create", "record": "object.payment",
                  "from": ["input.request_id", "input.order_id", "input.amount"] },
                { "kind": "advance", "record": "order", "field": "last_applied_sequence",
                  "to": "input.sequence", "rule": "successor" },
                { "kind": "publish", "topic": "topic.order_events", "schema": "schema.OrderPaid",
                  "from": ["input.order_id"] },
                { "kind": "call", "name": "payment-provider.charge", "as": "charge",
                  "duplicates": "distinguishable",
                  "result": { "ok": "schema.ChargeAccepted",
                              "errors": { "declined": "schema.ChargeDeclined" } },
                  "on_ok": [{ "kind": "publish", "topic": "topic.order_events",
                              "schema": "schema.PaymentCaptured", "from": [] }] }
            ],
            "returns": ["input.order_id"]
        }))
        .expect("parses");

        let back: OperationSketch =
            serde_json::from_value(serde_json::to_value(&sketch).expect("serializes"))
                .expect("parses back");

        assert_eq!(sketch, back);
    }
}
