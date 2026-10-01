//! Operation sketches, and their compilation into programs.
//!
//! A sketch is what an operation does, as a short list of typed business
//! actions over the skeleton's symbols — find a record, update its
//! fields, create one, apply a lifecycle transition. The coordinator
//! writes it with the operation's interface, where it already knows what
//! the operation is for. Code compiles it into a program: transaction
//! grouping, ids, the version protocol, keyed commits from the request's
//! identity, the inspect-then-decide shape a guarded transition needs to
//! replay, outputs, returns and rejection arms. No session writes the
//! program, and no model writes a step.
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
    Branch, BumpVersion, Condition, DataObject, Derivation, EstablishTransactionOutput,
    ExecuteTransaction, FieldPath, FieldSelection, Id, IdempotencyGuarantee, IdempotencyKey, Input,
    Insert, Literal, MessageSelector, ObjectSelector, OperationBlock, OperationStep, Read,
    RequestIdentity, ResultOutcome, Return, SelectorPredicate, SelectorValue, StateMachine,
    StateMachineSubject, StateTransition, Transaction, TransactionIsolation, TransactionStep,
    ValidateVersion, ValueRef, ValueSource, Write,
};

/// What an operation does, in order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationSketch {
    pub steps: Vec<SketchStep>,
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
    },
}

/// What compilation reads from the skeleton.
#[derive(Debug, Clone, Default)]
pub struct Symbols {
    /// Data object → its data model and declaration.
    pub objects: BTreeMap<Id, (Id, DataObject)>,

    /// Schema → its top-level field names.
    pub schemas: BTreeMap<Id, BTreeSet<String>>,

    pub machines: BTreeMap<Id, StateMachine>,
}

impl Symbols {
    /// Everything a workspace declares.
    pub fn of(workspace: &super::WorkspaceState) -> Self {
        let objects = workspace
            .data_models
            .iter()
            .flat_map(|(model, data)| {
                data.objects.iter().map(move |(object, declared)| {
                    (object.clone(), (model.clone(), declared.clone()))
                })
            })
            .collect();

        let schemas = workspace
            .schemas
            .iter()
            .map(|(id, schema)| (id.clone(), schema_fields(schema)))
            .collect();

        Self {
            objects,
            schemas,
            machines: workspace.state_machines.clone(),
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
}

/// Settled answers to [`Open`] choices.
#[derive(Debug, Clone, Default)]
pub struct Settled {
    pub refusal: Option<Id>,
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
    let sketch = draft
        .sketch
        .as_ref()
        .ok_or_else(|| CompileError(format!("{operation} has no sketch")))?;

    let mut open = Vec::new();

    for step in &sketch.steps {
        if let SketchStep::Transition {
            transition,
            otherwise: None,
            ..
        } = step
            && context.request
        {
            open.push(Open::Refusal {
                transition: transition.clone(),
                errors: context.errors.keys().cloned().collect(),
            });
        }
    }

    Ok(open)
}

/// Compiles an operation's sketch into its program.
pub fn compile(
    operation: &Id,
    draft: &super::DraftOperation,
    symbols: &Symbols,
    settled: &Settled,
) -> Result<OperationBlock, CompileError> {
    let context = context(operation, draft, symbols)?;

    let sketch = draft
        .sketch
        .as_ref()
        .ok_or_else(|| CompileError(format!("{operation} has no sketch")))?;

    if sketch.steps.is_empty() {
        return fail("the sketch has no steps");
    }

    let mut found: BTreeMap<String, Found> = BTreeMap::new();
    let mut steps: Vec<TransactionStep> = Vec::new();
    let mut mutated: Vec<String> = Vec::new();
    let mut created: BTreeSet<String> = BTreeSet::new();
    let mut created_from: Vec<ValueRef> = Vec::new();
    let mut data_models: BTreeSet<Id> = BTreeSet::new();
    let mut guarded: Option<(String, Id, Id, Option<Id>, bool)> = None;

    for step in &sketch.steps {
        match step {
            SketchStep::Find { alias, record, by } => {
                if found.contains_key(alias) || alias == "input" {
                    return fail(format!(
                        "`{alias}` names two things; give each find its own name"
                    ));
                }

                let Some((data_model, data)) = symbols.objects.get(record) else {
                    return fail(format!("`{record}` is not a declared data object"));
                };

                let fields = symbols.fields(&data.schema);

                if by.is_empty() {
                    return fail(format!("find `{alias}` gives no field to find it by"));
                }

                let mut predicates = Vec::new();
                let mut input_only = true;

                for (field, value) in by {
                    if !fields.contains(field) {
                        return fail(format!("`{record}` has no field `{field}` to find it by"));
                    }

                    let value = resolve(&context, &found, value)?;

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

                let read = Id(format!("read.{}.{alias}", context.name));

                data_models.insert(data_model.clone());

                found.insert(
                    alias.clone(),
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

                // The read itself is placed once every step is known:
                // its field selection is what later steps use.
                steps.push(TransactionStep::Read(Read {
                    bind: Id(format!("read.{}.{alias}", context.name)),
                    target: found[alias].selector.clone(),
                    fields: FieldSelection::All,
                }));

                if let Some(version) = &data.version {
                    steps.push(TransactionStep::ValidateVersion(ValidateVersion {
                        target: found[alias].selector.clone(),
                        expected: ValueRef {
                            source: ValueSource::TransactionRead(found[alias].read.clone()),
                            path: version.field.clone(),
                        },
                    }));
                }
            }

            SketchStep::Update { record, set, from } => {
                let Some(target) = found.get(record) else {
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

                let values = derivation(&context, &found, from)?;

                steps.push(TransactionStep::Write(Write {
                    target: target.selector.clone(),
                    fields: set
                        .iter()
                        .map(|field| FieldPath(vec![field.clone()]))
                        .collect(),
                    values,
                }));

                mutated.push(record.clone());
            }

            SketchStep::Create { record, from } => {
                let Some((data_model, data)) = symbols.objects.get(record) else {
                    return fail(format!("`{record}` is not a declared data object"));
                };

                let values = derivation(&context, &found, from)?;

                let Derivation::Deterministic { from: roots } = &values else {
                    return fail(format!("create `{record}` derives its values from nothing"));
                };

                created_from.extend(roots.iter().cloned());
                created.extend(symbols.fields(&data.schema));
                data_models.insert(data_model.clone());

                steps.push(TransactionStep::Insert(Insert {
                    object: record.clone(),
                    values,
                }));
            }

            SketchStep::Transition {
                record,
                transition,
                otherwise,
                already_ok,
            } => {
                let Some(target) = found.get(record) else {
                    return fail(format!(
                        "transition names `{record}`, which no find before it names"
                    ));
                };

                let Some((machine, _)) = symbols.machines.iter().find(|(_, machine)| {
                    let StateMachineSubject::Object { object, .. } = &machine.subject;

                    object == &target.record && machine.transitions.contains_key(transition)
                }) else {
                    return fail(format!(
                        "no state machine of `{}` declares `{transition}`",
                        target.record
                    ));
                };

                if let Some(error) = otherwise
                    && !context.errors.contains_key(error)
                {
                    return fail(format!("`{error}` is not an error the request declares"));
                }

                if guarded.is_some() {
                    return fail("a sketch applies at most one transition");
                }

                let refusal = otherwise.clone().or_else(|| settled.refusal.clone());

                if context.request && refusal.is_none() {
                    return fail(format!(
                        "the transition `{transition}` names no `otherwise` error for a record in \
                         the wrong state"
                    ));
                }

                guarded = Some((
                    record.clone(),
                    machine.clone(),
                    transition.clone(),
                    refusal,
                    *already_ok,
                ));

                steps.push(TransactionStep::Transition(StateTransition {
                    machine: machine.clone(),
                    transition: transition.clone(),
                    subject: target.selector.clone(),
                    effect_intents: BTreeMap::new(),
                    effects: BTreeMap::new(),
                }));

                mutated.push(record.clone());
            }
        }
    }

    let [data_model] = data_models.iter().collect::<Vec<_>>()[..] else {
        return fail(if data_models.is_empty() {
            "the sketch touches no record".to_string()
        } else {
            format!(
                "the sketch touches records of {} data models; one transaction spans one",
                data_models.len()
            )
        });
    };

    // Each changed versioned record advances its version, after its
    // last change.
    for alias in mutated.iter().collect::<BTreeSet<_>>() {
        let target = &found[alias];

        if target.data.version.is_none() {
            continue;
        }

        let last = steps
            .iter()
            .rposition(|step| match step {
                TransactionStep::Write(write) => write.target == target.selector,
                TransactionStep::Transition(transition) => transition.subject == target.selector,
                _ => false,
            })
            .expect("a mutated record has a mutating step");

        steps.insert(
            last + 1,
            TransactionStep::BumpVersion(BumpVersion {
                target: target.selector.clone(),
            }),
        );
    }

    // What the result carries that only the transaction holds.
    let output = context
        .ok
        .as_ref()
        .filter(|_| context.request)
        .and_then(|(schema, fields)| {
            let from: Vec<ValueRef> = fields
                .iter()
                .filter_map(|field| {
                    if context.carries.contains(field) {
                        return Some(input(&context, field));
                    }

                    if let Some(target) =
                        found.values().find(|target| target.fields.contains(field))
                    {
                        return Some(ValueRef {
                            source: ValueSource::TransactionRead(target.read.clone()),
                            path: FieldPath(vec![field.clone()]),
                        });
                    }

                    created
                        .contains(field)
                        .then(|| created_from.first().cloned())
                        .flatten()
                })
                .collect();

            let from = if from.is_empty() {
                context.key.clone()?.components
            } else {
                from
            };

            Some((
                Id(format!("output.{}.result", context.name)),
                schema.clone(),
                from,
            ))
        });

    if let Some((bind, schema, from)) = &output {
        steps.push(TransactionStep::EstablishTransactionOutput(
            EstablishTransactionOutput {
                bind: bind.clone(),
                schema: schema.clone(),
                values: Derivation::Deterministic { from: from.clone() },
            },
        ));
    }

    let mut main = Transaction {
        id: Id(format!("tx.{}", context.name)),
        data_model: Some(data_model.clone()),
        isolation: TransactionIsolation::ReadCommitted,
        idempotency: match &context.key {
            Some(key) => IdempotencyGuarantee::DeduplicatedBy { key: key.clone() },
            None => IdempotencyGuarantee::Unspecified,
        },
        requirements: Default::default(),
        steps,
    };

    narrow_reads(&mut main, &found);

    let rejects = main.rejects();

    let finish = finish(&context, output.as_ref().map(|(bind, _, _)| bind));

    // A guarded transition behind a keyed request: inspect, then decide,
    // so every attempt of one request decides alike.
    if let Some((alias, machine, transition, Some(refusal), already_ok)) = &guarded
        && let Some(key) = &context.key
        && found[alias].input_only
    {
        let target = &found[alias];
        let state = state_field(symbols, machine)?;
        let declared = &symbols.machines[machine].transitions[transition];

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

        let decide = OperationStep::Branch(Branch {
            condition: applies,
            then: OperationBlock {
                steps: vec![
                    OperationStep::Transaction(ExecuteTransaction {
                        transaction: main,
                        rejected: Some(OperationBlock {
                            steps: vec![OperationStep::Complete],
                        }),
                    }),
                    finish,
                ],
            },
            otherwise: Some(OperationBlock {
                steps: vec![refused(&context, refusal)],
            }),
        });

        // Already done: the request's `ok`, from what it carries.
        let decide = if *already_ok {
            OperationStep::Branch(Branch {
                condition: in_state(&declared.to),
                then: OperationBlock {
                    steps: vec![ok_from_input(&context)],
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
    // transition from the wrong state — returns the refusal a request
    // declared for it, or completes.
    let rejected = rejects.then(|| OperationBlock {
        steps: vec![match &guarded {
            Some((_, _, _, Some(refusal), _)) => refused(&context, refusal),
            _ => OperationStep::Complete,
        }],
    });

    Ok(OperationBlock {
        steps: vec![
            OperationStep::Transaction(ExecuteTransaction {
                transaction: main,
                rejected,
            }),
            finish,
        ],
    })
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
            let MessageSelector::Only(schemas) = &subscription.messages else {
                return fail("a sketched subscription consumes one message schema");
            };

            let [schema] = schemas.iter().collect::<Vec<_>>()[..] else {
                return fail("a sketched subscription consumes one message schema");
            };

            (false, schema.clone(), None, BTreeMap::new(), None)
        }

        Input::Outbox(_) => return fail("outbox consumers cannot be sketched yet"),
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

    for step in &transaction.steps {
        match step {
            TransactionStep::Read(read) => {
                for root in read.target.predicate.roots() {
                    note(root);
                }
            }
            TransactionStep::Write(write) => {
                for root in write.target.predicate.roots() {
                    note(root);
                }

                if let Derivation::Deterministic { from } = &write.values {
                    from.iter().for_each(&mut note);
                }
            }
            TransactionStep::Insert(insert) => {
                if let Derivation::Deterministic { from } = &insert.values {
                    from.iter().for_each(&mut note);
                }
            }
            TransactionStep::ValidateVersion(validate) => note(&validate.expected),
            TransactionStep::EstablishTransactionOutput(output) => {
                if let Derivation::Deterministic { from } = &output.values {
                    from.iter().for_each(&mut note);
                }
            }
            TransactionStep::Transition(transition) => {
                for root in transition.subject.predicate.roots() {
                    note(root);
                }
            }
            TransactionStep::BumpVersion(bump) => {
                for root in bump.target.predicate.roots() {
                    note(root);
                }
            }
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
/// else the request's identity, else everything the input carries.
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
                  "from": ["input.request_id", "input.order_id", "input.amount"] }
            ]
        }))
        .expect("parses");

        let back: OperationSketch =
            serde_json::from_value(serde_json::to_value(&sketch).expect("serializes"))
                .expect("parses back");

        assert_eq!(sketch, back);
    }
}
