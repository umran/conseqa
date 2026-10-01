//! Operation sketches, and their compilation into programs.
//!
//! A sketch is what an operation does, as a short list of typed business
//! actions over the skeleton's symbols — find a record, change it,
//! create one, apply a lifecycle transition, advance a position, admit
//! or publish a message, call an outside service. The coordinator writes
//! it with the operation's interface, where it already knows what the
//! operation is for. Code compiles it into a program: transaction
//! grouping, ids, the version protocol or strict locks, keyed commits
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
    AdvanceCursor, Branch, BumpVersion, Condition, CursorAdvanceRule, DataObject, Derivation,
    Effect, ErrorDisposition, ErrorResultType, EstablishEffectIntent, EstablishTransactionOutput,
    ExecuteEffect, ExecuteEffectIntent, ExecuteTransaction, ExternalEffect, ExternalIdempotency,
    ExternalIdentity, ExternalIdentityKey, ExternalResultReplay, FieldPath, FieldSelection, Id,
    IdempotencyGuarantee, IdempotencyKey, IdempotencyKeyPropagation, Input, Insert, Literal, Lock,
    LockMode, LockOrder, MatchResult, MessageIdentity, MessageSelector, ObjectSelector,
    OperationBlock, OperationStep, Outbox, OutboxWriteEffect, PublicationEffect, Read,
    RequestIdentity, ResultOutcome, ResultType, Return, SelectorPredicate, SelectorValue,
    StateMachine, StateMachineSubject, StateTransition, Topic, Transaction, TransactionIsolation,
    TransactionStep, TransitionSideEffect, ValidateVersion, ValueRef, ValueSource, Write,
    WriteOutboxEffect,
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
        /// Record field → value reference, e.g. `product_id:
        /// input.product_id`. Together they must pin one instance.
        by: BTreeMap<String, String>,
    },

    /// Give fields of a found record new values, derived from `from`.
    Update {
        record: String,
        set: Vec<String>,
        #[serde(default)]
        from: Vec<String>,
    },

    /// Create one instance of `record` from `from`.
    Create { record: Id, from: Vec<String> },

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
    },

    /// Admit a message to a transactional outbox, atomically with the
    /// records changed.
    Enqueue {
        outbox: Id,
        schema: Id,
        from: Vec<String>,
    },

    /// Publish a message to a topic: after the commit when the
    /// operation changes records, directly otherwise.
    Publish {
        topic: Id,
        schema: Id,
        from: Vec<String>,
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
        from: Vec<String>,

        /// Steps on an `ok` answer.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        on_ok: Vec<SketchStep>,

        /// Steps on each declared error.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        on_error: BTreeMap<Id, Vec<SketchStep>>,
    },
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
    pub errors: BTreeMap<Id, Id>,
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
                } => {
                    visit(on_ok, context, open);
                    for arm in on_error.values() {
                        visit(arm, context, open);
                    }
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
            | SketchStep::Transition { .. }
            | SketchStep::Advance { .. }
            | SketchStep::Enqueue { .. }
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
    after_commit: Vec<Id>,

    effects: usize,
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
        resolve(&self.context, &self.found, reference)
    }

    fn derivation(&self, from: &[String]) -> Result<Derivation, CompileError> {
        derivation(&self.context, &self.found, from)
    }

    /// The trigger's identity carried into a message's own identity, so
    /// a duplicate of one trigger is a duplicate of one message.
    fn propagation(
        &self,
        effect: &Id,
        identity: &MessageIdentity,
        schema: &Id,
    ) -> Vec<IdempotencyKeyPropagation> {
        let (Some(key), MessageIdentity::Keyed(target)) = (&self.context.key, identity) else {
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
        match step {
            SketchStep::Find { alias, record, by } => self.find(alias, record, by),

            SketchStep::Update { record, set, from } => {
                let Some(target) = self.found.get(record) else {
                    return fail(format!(
                        "update names `{record}`, which no find before it names"
                    ));
                };

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
                let values = self.derivation(from)?;

                self.steps.push(TransactionStep::Write(Write {
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

            SketchStep::Create { record, from } => {
                let Some((data_model, data)) = self.symbols.objects.get(record) else {
                    return fail(format!("`{record}` is not a declared data object"));
                };

                let (data_model, data) = (data_model.clone(), data.clone());

                let values = self.derivation(from)?;

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
            } => {
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
                    }));

                self.mutated.push(record.clone());

                Ok(())
            }

            SketchStep::Enqueue {
                outbox,
                schema,
                from,
            } => {
                let Some((data_model, declared)) = self.symbols.outboxes.get(outbox) else {
                    return fail(format!("`{outbox}` is not a declared outbox"));
                };

                if !declared.messages.contains(schema) {
                    return fail(format!("`{outbox}` does not admit `{schema}`"));
                }

                let (data_model, identity) =
                    (data_model.clone(), declared.message_identity.clone());

                let (effect, _) = self.effect_ids("enqueue");
                let values = self.derivation(from)?;

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
            } => {
                let (effect, intent, publication) = self.publication(topic, schema)?;
                let values = self.derivation(from)?;

                self.steps.push(TransactionStep::EstablishEffectIntent(
                    EstablishEffectIntent {
                        bind: intent.clone(),
                        effect_id: effect,
                        effect: Effect::Publication(publication),
                        values,
                    },
                ));

                self.after_commit.push(intent);

                Ok(())
            }

            SketchStep::Call { name, .. } => fail(format!(
                "the call to `{name}` is in a sketch that changes records; call outside services \
                 from an operation of their own, triggered by a message"
            )),
        }
    }

    fn find(
        &mut self,
        alias: &str,
        record: &Id,
        by: &BTreeMap<String, String>,
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

        if by.is_empty() {
            return fail(format!("find `{alias}` gives no field to find it by"));
        }

        let mut predicates = Vec::new();
        let mut input_only = true;

        for (field, value) in by {
            if !fields.contains(field) {
                return fail(format!("`{record}` has no field `{field}` to find it by"));
            }

            let value = self.resolve(value)?;

            input_only &= matches!(value.source, ValueSource::Input(_));

            predicates.push(SelectorPredicate::Eq {
                field: FieldPath(vec![field.clone()]),
                value: SelectorValue::Value(value),
            });
        }

        let identity: BTreeSet<&String> = data
            .identity
            .iter()
            .filter_map(|path| path.0.last())
            .collect();

        if !identity.iter().all(|field| by.contains_key(*field)) {
            return fail(format!(
                "find `{alias}` must give every identity field of `{record}`: {}",
                identity
                    .iter()
                    .map(|field| format!("`{field}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }

        let predicate = match <[_; 1]>::try_from(predicates) {
            Ok([only]) => only,
            Err(many) => SelectorPredicate::And { predicates: many },
        };

        let selector = ObjectSelector {
            object: record.clone(),
            predicate,
        };

        let read = Id(format!("read.{}.{alias}", self.context.name));

        self.data_models.insert(data_model.clone());

        self.steps.push(TransactionStep::Read(Read {
            bind: read.clone(),
            target: selector.clone(),
            fields: FieldSelection::All,
        }));

        if let Some(version) = &data.version {
            self.steps
                .push(TransactionStep::ValidateVersion(ValidateVersion {
                    target: selector.clone(),
                    expected: ValueRef {
                        source: ValueSource::TransactionRead(read.clone()),
                        path: version.field.clone(),
                    },
                }));
        }

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
            },
        );

        Ok(())
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

        if self.guarded.is_some() {
            return fail("a sketch applies at most one transition");
        }

        if !declared.effects.is_empty() {
            return fail(format!(
                "`{transition}` admits outbox messages, which sketches cannot yet derive"
            ));
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

            self.after_commit.push(intent);
        }

        // Every declared side effect is a publication or a request: the
        // vocabulary has nothing further to check about them here.
        let _: Vec<&TransitionSideEffect> = declared.side_effects.values().collect();

        self.guarded = Some((
            record.clone(),
            machine.clone(),
            transition.clone(),
            refusal,
            already_ok,
        ));

        self.steps
            .push(TransactionStep::Transition(StateTransition {
                machine,
                transition: transition.clone(),
                subject,
                effect_intents,
                effects: BTreeMap::new(),
            }));

        self.mutated.push(record.clone());

        Ok(())
    }

    /// An effect step outside any transaction.
    fn effect_step(&mut self, step: &SketchStep) -> Result<Vec<OperationStep>, CompileError> {
        match step {
            SketchStep::Publish {
                topic,
                schema,
                from,
            } => {
                let (effect, _, publication) = self.publication(topic, schema)?;

                Ok(vec![OperationStep::ExecuteEffect(ExecuteEffect {
                    effect_id: effect,
                    effect: Effect::Publication(publication),
                    values: self.derivation(from)?,
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
                        .map(|(class, schema)| {
                            (
                                class.clone(),
                                ErrorResultType {
                                    schema: schema.clone(),
                                    disposition: ErrorDisposition::Unspecified,
                                },
                            )
                        })
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

                let values = self.derivation(from)?;

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

            other => fail(format!("{other:?} is not an effect step")),
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
    };

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

        // A changed record is protected: a versioned one validates the
        // version it read and advances it after its last change; an
        // unversioned one is held under an exclusive lock from before its
        // read.
        for alias in self.mutated.iter().collect::<BTreeSet<_>>() {
            let target = &self.found[alias];

            if target.data.version.is_some() {
                let last = self
                    .steps
                    .iter()
                    .rposition(|step| match step {
                        TransactionStep::Write(write) => write.target == target.selector,
                        TransactionStep::Transition(transition) => {
                            transition.subject == target.selector
                        }
                        TransactionStep::AdvanceCursor(advance) => {
                            advance.target == target.selector
                        }
                        _ => false,
                    })
                    .expect("a mutated record has a mutating step");

                self.steps.insert(
                    last + 1,
                    TransactionStep::BumpVersion(BumpVersion {
                        target: target.selector.clone(),
                    }),
                );
            } else {
                let read = self
                    .steps
                    .iter()
                    .position(|step| {
                        matches!(step, TransactionStep::Read(read) if read.bind == target.read)
                    })
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
            isolation: TransactionIsolation::ReadCommitted,
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
            .iter()
            .map(|intent| {
                OperationStep::ExecuteEffectIntent(ExecuteEffectIntent {
                    intent: intent.clone(),
                    bind: None,
                })
            })
            .collect();

        success.push(finish(context, output.as_ref().map(|(bind, _, _)| bind)));

        // A guarded transition behind a keyed request: inspect, then
        // decide, so every attempt of one request decides alike.
        if let Some((alias, machine, transition, Some(refusal), already_ok)) = &self.guarded
            && context.request
            && let Some(key) = &context.key
            && self.found[alias].input_only
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

fn context<'a>(
    operation: &'a Id,
    draft: &'a super::DraftOperation,
    symbols: &'a Symbols,
) -> Result<Context<'a>, CompileError> {
    let [(input_id, input)] = draft.inputs.iter().collect::<Vec<_>>()[..] else {
        return fail("a sketched operation has exactly one input");
    };

    let name = operation
        .0
        .strip_prefix("operation.")
        .unwrap_or(&operation.0)
        .to_string();

    let (request, schema, ok, errors, key) = match input {
        Input::Request(request) => {
            let key = match &request.identity {
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
            };

            (
                true,
                request.schema.clone(),
                Some((
                    request.result.ok.clone(),
                    symbols.fields(&request.result.ok),
                )),
                request
                    .result
                    .errors
                    .iter()
                    .map(|(class, error)| (class.clone(), symbols.fields(&error.schema)))
                    .collect(),
                key,
            )
        }

        Input::Subscription(subscription) => {
            let one = |schemas: &BTreeSet<Id>| match schemas.iter().collect::<Vec<_>>()[..] {
                [schema] => Ok(schema.clone()),
                _ => fail("a sketched subscription consumes one message schema"),
            };

            let topic = symbols.topics.get(&subscription.topic);

            let schema = match &subscription.messages {
                MessageSelector::Only(schemas) => one(schemas)?,
                MessageSelector::All => one(&topic
                    .map(|topic| topic.messages.clone())
                    .unwrap_or_default())?,
            };

            let key =
                topic.and_then(|topic| message_key(&topic.message_identity, &schema, input_id));

            (false, schema, None, BTreeMap::new(), key)
        }

        Input::Outbox(consumer) => {
            let Some((_, outbox)) = symbols.outboxes.get(&consumer.outbox) else {
                return fail(format!("`{}` is not a declared outbox", consumer.outbox));
            };

            let [schema] = outbox.messages.iter().collect::<Vec<_>>()[..] else {
                return fail("a sketched outbox consumer consumes one message schema");
            };

            let key = message_key(&outbox.message_identity, schema, input_id);

            (false, schema.clone(), None, BTreeMap::new(), key)
        }
    };

    Ok(Context {
        operation,
        name,
        input: input_id,
        request,
        carries: symbols.fields(&schema),
        ok,
        errors,
        key,
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
    reference: &str,
) -> Result<ValueRef, CompileError> {
    let Some((source, field)) = reference.split_once('.') else {
        return fail(format!(
            "`{reference}` is not a value: write `input.<field>` or `<found record>.<field>`"
        ));
    };

    let path = FieldPath(field.split('.').map(str::to_string).collect());
    let head = path.0.first().cloned().unwrap_or_default();

    if source == "input" {
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

    let Some(target) = found.get(source) else {
        return fail(format!(
            "`{reference}` names `{source}`, which is neither `input` nor a record found before it"
        ));
    };

    if !target.fields.contains(&head) {
        return fail(format!("`{}` has no field `{head}`", target.record));
    }

    Ok(ValueRef {
        source: ValueSource::TransactionRead(target.read.clone()),
        path,
    })
}

fn derivation(
    context: &Context<'_>,
    found: &BTreeMap<String, Found>,
    from: &[String],
) -> Result<Derivation, CompileError> {
    if from.is_empty() {
        return Ok(Derivation::Unspecified);
    }

    Ok(Derivation::Deterministic {
        from: from
            .iter()
            .map(|reference| resolve(context, found, reference))
            .collect::<Result<_, _>>()?,
    })
}

/// Each read selects exactly the fields later steps use, and the
/// version it is validated against.
fn narrow_reads(transaction: &mut Transaction, found: &BTreeMap<String, Found>) {
    let mut used: BTreeMap<Id, BTreeSet<FieldPath>> = BTreeMap::new();

    let mut note = |value: &ValueRef| {
        if let ValueSource::TransactionRead(read) = &value.source {
            used.entry(read.clone())
                .or_default()
                .insert(value.path.clone());
        }
    };

    let note_derivation = |values: &Derivation, note: &mut dyn FnMut(&ValueRef)| {
        if let Derivation::Deterministic { from } = values {
            from.iter().for_each(note);
        }
    };

    for step in &transaction.steps {
        match step {
            TransactionStep::Read(read) => read
                .target
                .predicate
                .roots()
                .into_iter()
                .for_each(&mut note),
            TransactionStep::Lock(lock) => lock
                .target
                .predicate
                .roots()
                .into_iter()
                .for_each(&mut note),
            TransactionStep::Write(write) => {
                write
                    .target
                    .predicate
                    .roots()
                    .into_iter()
                    .for_each(&mut note);
                note_derivation(&write.values, &mut note);
            }
            TransactionStep::Insert(insert) => note_derivation(&insert.values, &mut note),
            TransactionStep::ValidateVersion(validate) => note(&validate.expected),
            TransactionStep::EstablishTransactionOutput(output) => {
                note_derivation(&output.values, &mut note)
            }
            TransactionStep::EstablishEffectIntent(intent) => {
                note_derivation(&intent.values, &mut note)
            }
            TransactionStep::WriteOutbox(outbox) => note_derivation(&outbox.values, &mut note),
            TransactionStep::Transition(transition) => {
                transition
                    .subject
                    .predicate
                    .roots()
                    .into_iter()
                    .for_each(&mut note);

                for intent in transition.effect_intents.values() {
                    note_derivation(&intent.values, &mut note);
                }
            }
            TransactionStep::AdvanceCursor(advance) => {
                advance
                    .target
                    .predicate
                    .roots()
                    .into_iter()
                    .for_each(&mut note);
                note(&advance.incoming);
            }
            TransactionStep::BumpVersion(bump) => bump
                .target
                .predicate
                .roots()
                .into_iter()
                .for_each(&mut note),
            _ => {}
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

/// The inspection's body: read the state — and validate the version
/// it was read at, so the inspection is a protected participant of its
/// record's conflict closure, as every reader of a versioned record
/// must be for the version route to prove the writers serializable.
fn inspect_steps(
    target: &Found,
    read: &Id,
    state: &FieldPath,
    lookup: &Id,
    exported: Vec<ValueRef>,
) -> Vec<TransactionStep> {
    let mut fields: BTreeSet<FieldPath> = [state.clone()].into();

    let mut steps = Vec::new();

    if let Some(version) = &target.data.version {
        fields.insert(version.field.clone());
    }

    steps.push(TransactionStep::Read(Read {
        bind: read.clone(),
        target: target.selector.clone(),
        fields: FieldSelection::Only(fields),
    }));

    if let Some(version) = &target.data.version {
        steps.push(TransactionStep::ValidateVersion(ValidateVersion {
            target: target.selector.clone(),
            expected: ValueRef {
                source: ValueSource::TransactionRead(read.clone()),
                path: version.field.clone(),
            },
        }));
    }

    steps.push(TransactionStep::EstablishTransactionOutput(
        EstablishTransactionOutput {
            bind: lookup.clone(),
            schema: target.data.schema.clone(),
            values: Derivation::Deterministic { from: exported },
        },
    ));

    steps
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
