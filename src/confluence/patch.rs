//! Typed semantic mutations: the only commit protocol.
//!
//! Never a text diff, JSON Patch over array indexes, or YAML string
//! replacement (§25 of the confluence spec). V1 granularity is the
//! operation slice — `ReplaceOperationProgram` is one semantic
//! transaction — because one primary writer per operation already
//! avoids most contention (§26).
//!
//! Beyond §25's enum, two mutations carry the authoring flows the
//! workspace holds outside the normative DSL: `PutPromptObligation`
//! (the decomposer records explicit prompt obligations) and
//! `ProposeRequirements` (requirement discovery submits proposals with
//! provenance; the commit gate records them and mechanically adopts
//! per run policy, §69–70).

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::spec::{
    DataModel, Effect, ExecutionPool, Id, Input, OperationBlock, OperationRequirements,
    OperationStep, OutboxPartitioning, OutboxRuntime, Router, Schema, Service, StateMachine,
    StorageLayout, SubscriptionRuntime, Topic, TopicRuntime, TransactionStep,
    TransitionSideEffect, ValueRef, ValueSource,
};

use super::symbol::SymbolKey;
use super::workspace::{
    OperationInterfaceDraft, PromptObligation, PromptObligationId, ProposedRequirement,
    RequirementOrigin,
};

/// Identity of one submitted patch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PatchId(pub Uuid);

impl PatchId {
    pub fn fresh() -> Self {
        Self(Uuid::new_v4())
    }
}

impl fmt::Display for PatchId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "patch-{}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpecPatch {
    pub mutations: Vec<Mutation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Mutation {
    PutService {
        id: Id,
        value: Service,
    },

    PutSchema {
        id: Id,
        value: Schema,
    },

    PutDataModel {
        id: Id,
        value: DataModel,
    },

    PutTopic {
        id: Id,
        value: Topic,
    },

    PutStateMachine {
        id: Id,
        value: StateMachine,
    },

    PutOperationInterface {
        operation: Id,
        value: OperationInterfaceDraft,
    },

    ReplaceOperationProgram {
        operation: Id,
        program: OperationBlock,
    },

    ReplaceOperationRequirements {
        operation: Id,
        requirements: OperationRequirements,
    },

    // ---- L1: runtime topology ----
    //
    // Each is a shared-skeleton write: runtime topology is an
    // architectural decision, not part of any one operation's
    // synthesis.
    PutTopicRuntime {
        topic: Id,
        value: TopicRuntime,
    },

    PutSubscriptionRuntime {
        operation: Id,
        input: Id,
        value: SubscriptionRuntime,
    },

    PutOutboxRuntime {
        operation: Id,
        input: Id,
        value: OutboxRuntime,
    },

    PutExecutionPool {
        id: Id,
        value: ExecutionPool,
    },

    PutRouter {
        id: Id,
        value: Router,
    },

    PutStorageLayout {
        id: Id,
        value: StorageLayout,
    },

    /// Requirement proposals with provenance. The gate records each on
    /// the workspace and adopts qualifying ones into the operation's
    /// declared requirements per run policy.
    ProposeRequirements {
        operation: Id,
        proposals: Vec<RequirementSubmission>,
    },

    PutPromptObligation {
        id: PromptObligationId,
        value: PromptObligation,
    },

    /// Normally coordinator-only.
    DeleteTopLevel {
        symbol: SymbolKey,
    },
}

/// One proposed requirement as submitted: content and origin. Status
/// is the gate's to assign, never the agent's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequirementSubmission {
    pub requirement: ProposedRequirement,
    pub origin: RequirementOrigin,
}

impl Mutation {
    /// The symbol this mutation overwrites — the unit of write-write
    /// conflict detection and scope checking.
    pub fn write_target(&self) -> SymbolKey {
        match self {
            Self::PutService { id, .. } => SymbolKey::Service(id.clone()),
            Self::PutSchema { id, .. } => SymbolKey::Schema(id.clone()),
            Self::PutDataModel { id, .. } => SymbolKey::DataModel(id.clone()),
            Self::PutTopic { id, .. } => SymbolKey::Topic(id.clone()),
            Self::PutStateMachine { id, .. } => SymbolKey::StateMachine(id.clone()),

            Self::PutOperationInterface { operation, .. } => {
                SymbolKey::OperationInterface(operation.clone())
            }

            Self::ReplaceOperationProgram { operation, .. } => {
                SymbolKey::OperationProgram(operation.clone())
            }

            Self::PutTopicRuntime { topic, .. } => SymbolKey::TopicRuntime(topic.clone()),

            Self::PutSubscriptionRuntime {
                operation, input, ..
            } => SymbolKey::SubscriptionRuntime {
                operation: operation.clone(),
                input: input.clone(),
            },

            Self::PutOutboxRuntime {
                operation, input, ..
            } => SymbolKey::OutboxRuntime {
                operation: operation.clone(),
                input: input.clone(),
            },

            Self::PutExecutionPool { id, .. } => SymbolKey::ExecutionPool(id.clone()),
            Self::PutRouter { id, .. } => SymbolKey::Router(id.clone()),
            Self::PutStorageLayout { id, .. } => SymbolKey::StorageLayout(id.clone()),

            Self::ReplaceOperationRequirements { operation, .. }
            | Self::ProposeRequirements { operation, .. } => {
                SymbolKey::OperationRequirements(operation.clone())
            }

            Self::PutPromptObligation { id, .. } => SymbolKey::PromptObligation(id.clone()),

            Self::DeleteTopLevel { symbol } => symbol.clone(),
        }
    }

    /// The operation whose draft this mutation belongs to, if any.
    pub fn operation(&self) -> Option<&Id> {
        match self {
            Self::PutOperationInterface { operation, .. }
            | Self::ReplaceOperationProgram { operation, .. }
            | Self::ReplaceOperationRequirements { operation, .. }
            | Self::ProposeRequirements { operation, .. } => Some(operation),

            // A subscription runtime names an operation but is not part
            // of its draft: the boundary it targets is an external
            // reference the writer must have read.
            _ => None,
        }
    }
}

impl SpecPatch {
    /// The symbols this patch overwrites, in canonical order.
    pub fn write_targets(&self) -> Vec<SymbolKey> {
        let mut targets: Vec<SymbolKey> = self
            .mutations
            .iter()
            .map(Mutation::write_target)
            .collect();

        targets.sort();
        targets.dedup();

        targets
    }

    /// Every external symbol the patch's content references — the
    /// read-before-reference obligation (§24). References to symbols
    /// the patch itself writes, and to symbols owned by the operation
    /// a mutation belongs to, are internal and excluded: an agent
    /// needs no prior read of what it is creating or of its own
    /// operation's parts.
    pub fn external_references(&self) -> Vec<SymbolKey> {
        let mut references = Vec::new();

        for mutation in &self.mutations {
            collect_references(mutation, &mut references);
        }

        let written = self.write_targets();

        references.retain(|reference| {
            // Written by this very patch, in any granularity: a patch
            // creating an interface satisfies references to the
            // operation it plans.
            let written_here = written.iter().any(|target| {
                target == reference
                    || matches!(
                        (target, reference),
                        (
                            SymbolKey::OperationInterface(created),
                            SymbolKey::Operation(referenced)
                        ) if created == referenced
                    )
            });

            !written_here
        });

        references.sort();
        references.dedup();

        references
    }
}

fn collect_references(mutation: &Mutation, out: &mut Vec<SymbolKey>) {
    match mutation {
        Mutation::PutService { .. } => {}

        Mutation::PutSchema { value, .. } => match value {
            Schema::Canonical(canonical) => {
                for field in canonical.fields.values() {
                    collect_type_refs(&field.ty, out);
                }
            }

            Schema::Fragment(fragment) => {
                out.push(SymbolKey::Schema(fragment.source.clone()));
            }
        },

        Mutation::PutDataModel { value, .. } => {
            for object in value.objects.values() {
                out.push(SymbolKey::Schema(object.schema.clone()));
            }

            for outbox in value.outboxes.values() {
                for schema in &outbox.messages {
                    out.push(SymbolKey::Schema(schema.clone()));
                }
            }
        }

        Mutation::PutTopic { value, .. } => {
            for message in &value.messages {
                out.push(SymbolKey::Schema(message.clone()));
            }
        }

        Mutation::PutStateMachine { value, .. } => {
            // The machine's subject names an object id whose data
            // model the DSL leaves implicit; resolution is the
            // validator's concern, so the subject is not an external
            // reference here. Side-effect contracts are.
            for transition in value.transitions.values() {
                for side_effect in transition.side_effects.values() {
                    match side_effect {
                        TransitionSideEffect::Publication(publication) => {
                            out.push(SymbolKey::Topic(publication.topic.clone()));
                            out.push(SymbolKey::Schema(publication.schema.clone()));
                        }

                        TransitionSideEffect::Request(request) => {
                            out.push(SymbolKey::OperationInterface(
                                request.target.operation.clone(),
                            ));
                            out.push(SymbolKey::Schema(request.schema.clone()));
                        }
                    }
                }
            }
        }

        Mutation::PutOperationInterface { value, .. } => {
            for input in value.inputs.values() {
                collect_input_refs(input, out);
            }

            out.push(SymbolKey::Service(value.service.clone()));
        }

        Mutation::ReplaceOperationProgram { operation, program } => {
            collect_program_refs(operation, program, out);
        }

        Mutation::PutTopicRuntime { topic, value } => {
            out.push(SymbolKey::Topic(topic.clone()));

            if let Some(key) = &value.grouping {
                for schema in key.mapping.keys() {
                    out.push(SymbolKey::Schema(schema.clone()));
                }
            }
        }

        Mutation::PutSubscriptionRuntime {
            operation, value, ..
        } => {
            if let Some(key) = &value.grouping {
                for schema in key.mapping.keys() {
                    out.push(SymbolKey::Schema(schema.clone()));
                }
            }

            // The boundary belongs to another authority, so its
            // interface is a genuine external reference — and reading
            // the interface is what shows the input. So is the pool the
            // dispatch terminates at.
            out.push(SymbolKey::OperationInterface(operation.clone()));
            out.push(SymbolKey::ExecutionPool(value.dispatch.pool.clone()));
        }

        Mutation::PutOutboxRuntime {
            operation, value, ..
        } => {
            if let OutboxPartitioning::Keyed(key) = &value.partitioning {
                for schema in key.mapping.keys() {
                    out.push(SymbolKey::Schema(schema.clone()));
                }
            }

            // Same rule as a subscription runtime: the targeted
            // boundary and the dispatch pool are genuine external
            // references. The consumed outbox is named through the
            // input, whose owning data model the DSL leaves implicit —
            // resolution is the validator's concern, as with a state
            // machine's subject.
            out.push(SymbolKey::OperationInterface(operation.clone()));
            out.push(SymbolKey::ExecutionPool(value.dispatch.pool.clone()));
        }

        Mutation::PutExecutionPool { .. } => {}

        Mutation::PutRouter { value, .. } => {
            out.push(SymbolKey::OperationInterface(
                value.boundary.operation.clone(),
            ));

            out.push(SymbolKey::ExecutionPool(value.pool.clone()));
        }

        Mutation::PutStorageLayout { value, .. } => {
            out.push(SymbolKey::DataModel(value.object.data_model.clone()));

            out.push(SymbolKey::DataObject {
                data_model: value.object.data_model.clone(),
                object: value.object.object.clone(),
            });
        }

        Mutation::ReplaceOperationRequirements { .. } | Mutation::ProposeRequirements { .. } => {
            // Requirement keys reference the operation's own inputs
            // and bindings — internal to the operation being written.
        }

        Mutation::PutPromptObligation { .. } => {
            // Obligation targets name operations by id without
            // depending on their content; obligations may legitimately
            // target operations planned in the same patch.
        }

        Mutation::DeleteTopLevel { .. } => {}
    }
}

fn collect_input_refs(input: &Input, out: &mut Vec<SymbolKey>) {
    match input {
        Input::Request(request) => {
            out.push(SymbolKey::Schema(request.schema.clone()));
            out.push(SymbolKey::Schema(request.result.ok.clone()));
            out.push(SymbolKey::Schema(request.result.err.schema.clone()));
        }

        Input::Subscription(subscription) => {
            out.push(SymbolKey::Topic(subscription.topic.clone()));

            if let crate::spec::MessageSelector::Only(schemas) = &subscription.messages {
                for schema in schemas {
                    out.push(SymbolKey::Schema(schema.clone()));
                }
            }
        }

        Input::Outbox(_) => {
            // The outbox id alone names the boundary; its owning data
            // model is implicit and resolved by the validator, so no
            // data-model key can be produced here — the same treatment
            // a state machine's subject gets. There is no message
            // selection: the exclusive consumer admits every schema
            // the outbox declares, and those schemas are references
            // of the outbox declaration, not of this input.
        }
    }
}

fn collect_program_refs(operation: &Id, program: &OperationBlock, into: &mut Vec<SymbolKey>) {
    let out = &mut Vec::new();

    let effect_refs = |effect: &Effect, out: &mut Vec<SymbolKey>| match effect {
        Effect::Publication(publication) => {
            out.push(SymbolKey::Topic(publication.topic.clone()));
            out.push(SymbolKey::Schema(publication.schema.clone()));
        }

        Effect::Request(request) => {
            out.push(SymbolKey::OperationInterface(
                request.target.operation.clone(),
            ));
            out.push(SymbolKey::Schema(request.schema.clone()));
        }

        Effect::External(external) => {
            if let Some(result) = &external.result {
                out.push(SymbolKey::Schema(result.ok.clone()));
                out.push(SymbolKey::Schema(result.err.schema.clone()));
            }
        }

        // Only legal inside a transaction's `write_outbox` step; at
        // this illegal site the schema is still a reference, and the
        // outbox resolves through the transaction's data model, which
        // this site does not have.
        Effect::OutboxWrite(write) => {
            out.push(SymbolKey::Schema(write.schema.clone()));
        }
    };

    for (_, step) in program.steps_with_locations() {
        match step {
            OperationStep::Transaction(transaction) => {
                for inner in &transaction.steps {
                    match inner {
                        TransactionStep::Read(read) => {
                            push_object_ref(transaction.data_model.as_ref(), &read.target.object, out);
                        }

                        TransactionStep::Write(write) => {
                            push_object_ref(transaction.data_model.as_ref(), &write.target.object, out);
                        }

                        TransactionStep::Insert(insert) => {
                            push_object_ref(transaction.data_model.as_ref(), &insert.object, out);
                        }

                        TransactionStep::Delete(delete) => {
                            push_object_ref(transaction.data_model.as_ref(), &delete.target.object, out);
                        }

                        TransactionStep::Lock(lock) => {
                            push_object_ref(transaction.data_model.as_ref(), &lock.target.object, out);
                        }

                        TransactionStep::Transition(transition) => {
                            out.push(SymbolKey::StateMachine(transition.machine.clone()));
                            out.push(SymbolKey::Transition {
                                machine: transition.machine.clone(),
                                transition: transition.transition.clone(),
                            });

                            push_object_ref(
                                transaction.data_model.as_ref(),
                                &transition.subject.object,
                                out,
                            );
                        }

                        TransactionStep::EstablishEffectIntent(establish) => {
                            effect_refs(&establish.effect, out);
                        }

                        TransactionStep::EstablishTransactionOutput(establish) => {
                            out.push(SymbolKey::Schema(establish.schema.clone()));
                        }

                        TransactionStep::WriteOutbox(write) => {
                            // The destination outbox lives in the
                            // transaction's data model, whose key the
                            // transaction itself contributes below;
                            // the written schema is a reference of its
                            // own.
                            out.push(SymbolKey::Schema(write.effect.schema.clone()));
                        }
                    }
                }

                if let Some(data_model) = &transaction.data_model {
                    out.push(SymbolKey::DataModel(data_model.clone()));
                }
            }

            OperationStep::ExecuteEffect(execute) => {
                effect_refs(&execute.effect, out);
            }

            OperationStep::ExecuteEffectAsync(execute) => {
                effect_refs(&execute.effect, out);
            }

            _ => {}
        }
    }

    // A program's value references to its own inputs, bindings, and
    // reads are operation-internal; drop anything scoped to the
    // operation itself.
    out.retain(|reference| reference.operation() != Some(operation));

    into.append(out);
}

fn push_object_ref(data_model: Option<&Id>, object: &Id, out: &mut Vec<SymbolKey>) {
    if let Some(data_model) = data_model {
        out.push(SymbolKey::DataObject {
            data_model: data_model.clone(),
            object: object.clone(),
        });
    }
}

/// Value roots naming an operation's inputs, for draft checks.
pub(crate) fn input_roots(roots: &[&ValueRef]) -> Vec<Id> {
    roots
        .iter()
        .filter_map(|root| match &root.source {
            ValueSource::Input(input) => Some(input.clone()),
            _ => None,
        })
        .collect()
}

fn collect_type_refs(ty: &crate::spec::TypeRef, out: &mut Vec<SymbolKey>) {
    match ty {
        crate::spec::TypeRef::Scalar(_) => {}
        crate::spec::TypeRef::Schema(id) => out.push(SymbolKey::Schema(id.clone())),
        crate::spec::TypeRef::List(inner) => collect_type_refs(inner, out),
    }
}
