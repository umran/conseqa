//! The declaration partition (§7 of the spec change analysis revision).
//!
//! Every semantic fact of a workspace belongs to exactly one
//! declaration, so a change is reported once, at that declaration, and
//! never again at each aggregate containing it. The design layer's
//! symbol fingerprints overlap on purpose: a program's fingerprint
//! covers its transactions, which is right for invalidation and wrong
//! for attribution (§7.1). The partition is therefore a projection of
//! its own, and the fingerprints are untouched.
//!
//! A member's residual is the content it owns: its canonical form minus
//! what the members it contains own. A program holds each inline
//! transaction as the transaction's id and each effect contract as the
//! effect's id, and a transaction holds the effect contracts of its
//! intent and outbox-write steps the same way, since the effect site
//! owns its contract. A requirement owns everything of it but the key
//! that identifies it. Authoring content (sketches, stages,
//! descriptions, requirement proposals, the project's settings) is
//! classified apart: reported, but never semantic (§7.3).
//!
//! The projections destructure every workspace struct with all of its
//! fields named and match every program and transaction step by name,
//! with no wildcard arm. A new field or step therefore fails to compile
//! until the partition says who owns it. The property test of §19.1
//! checks that the members do partition every leaf.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::analyzer::basis::{
    self, DeclarationKey, ObligationKey, ObligationProperty, ObligationSubject,
};
use crate::design::workspace::{DraftOperation, WorkspaceState};
use crate::spec::{
    DataModel, Id, OperationBlock, OperationStep, RuntimeModel, Schema, StateMachine, Transaction,
    TransactionStep,
};

/// The declarations of one workspace, each with its residual, and its
/// authoring content.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Partition {
    pub members: BTreeMap<DeclarationKey, Value>,
    pub authoring: BTreeMap<AuthoringItem, Value>,
}

/// A piece of authoring content (§7.3): prose, drafting state, and the
/// provenance of requirements. Each is present on a side only when the
/// side has it, so writing a first sketch adds one.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum AuthoringItem {
    OperationSketch(Id),
    OperationStage(Id),
    OperationDescription(Id),
    SchemaDescription(Id),

    /// `intent/proposals.yaml`, as a whole.
    RequirementProposals,

    /// `project.yaml` but its `format` and `dsl`: the name and the run
    /// policy.
    ProjectSettings,
}

impl fmt::Display for AuthoringItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OperationSketch(operation) => write!(f, "sketch of {operation}"),
            Self::OperationStage(operation) => write!(f, "stage of {operation}"),
            Self::OperationDescription(operation) => write!(f, "description of {operation}"),
            Self::SchemaDescription(schema) => write!(f, "description of {schema}"),
            Self::RequirementProposals => f.write_str("requirement proposals"),
            Self::ProjectSettings => f.write_str("project settings"),
        }
    }
}

/// The semantic layer a declaration belongs to, in the order a diff
/// lists changes (§8.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeclarationLayer {
    L0,
    L1,
    Intent,
}

impl fmt::Display for DeclarationLayer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::L0 => "l0",
            Self::L1 => "l1",
            Self::Intent => "intent",
        })
    }
}

/// The layer of a declaration.
pub fn layer(declaration: &DeclarationKey) -> DeclarationLayer {
    match declaration {
        DeclarationKey::Service(_)
        | DeclarationKey::Schema(_)
        | DeclarationKey::DataModel(_)
        | DeclarationKey::DataObject { .. }
        | DeclarationKey::Outbox { .. }
        | DeclarationKey::Topic(_)
        | DeclarationKey::StateMachine(_)
        | DeclarationKey::Transition { .. }
        | DeclarationKey::Operation(_)
        | DeclarationKey::Input { .. }
        | DeclarationKey::Program(_)
        | DeclarationKey::Transaction { .. }
        | DeclarationKey::EffectSite { .. }
        | DeclarationKey::Requirement(_) => DeclarationLayer::L0,

        DeclarationKey::TopicRuntime(_)
        | DeclarationKey::SubscriptionRuntime { .. }
        | DeclarationKey::OutboxRuntime { .. }
        | DeclarationKey::ExecutionPool(_)
        | DeclarationKey::Router(_)
        | DeclarationKey::StorageLayout(_) => DeclarationLayer::L1,

        DeclarationKey::PromptObligation(_) => DeclarationLayer::Intent,
    }
}

impl Partition {
    /// The partition of a workspace.
    pub fn of(workspace: &WorkspaceState) -> Self {
        let WorkspaceState {
            // The engine's commit counter, never committed (§2 of the
            // semantics contract).
            revision: _,
            services,
            schemas,
            data_models,
            topics,
            state_machines,
            operations,
            runtime,
            prompt_obligations,
            requirement_proposals,
            run_meta,
        } = workspace;

        let mut partition = Self::default();

        for (id, service) in services {
            partition.member(DeclarationKey::Service(id.clone()), canonical(service));
        }

        for (id, schema) in schemas {
            let description = match schema {
                Schema::Canonical(canonical) => canonical.description.as_ref(),
                Schema::Fragment(_) => None,
            };

            if let Some(description) = description {
                partition.authoring(
                    AuthoringItem::SchemaDescription(id.clone()),
                    canonical(description),
                );
            }

            partition.member(
                DeclarationKey::Schema(id.clone()),
                without(canonical(schema), &["description"]),
            );
        }

        for (id, data_model) in data_models {
            partition.data_model(id, data_model);
        }

        for (id, topic) in topics {
            partition.member(DeclarationKey::Topic(id.clone()), canonical(topic));
        }

        for (id, machine) in state_machines {
            partition.state_machine(id, machine);
        }

        for (id, operation) in operations {
            partition.operation(id, operation);
        }

        partition.runtime(runtime);

        for (id, obligation) in prompt_obligations {
            partition.member(
                DeclarationKey::PromptObligation(Id(id.0.clone())),
                canonical(obligation),
            );
        }

        if !requirement_proposals.is_empty() {
            partition.authoring(
                AuthoringItem::RequirementProposals,
                canonical(requirement_proposals),
            );
        }

        partition.authoring(AuthoringItem::ProjectSettings, canonical(run_meta));

        partition
    }

    fn member(&mut self, declaration: DeclarationKey, residual: Value) {
        self.members.insert(declaration, residual);
    }

    fn authoring(&mut self, item: AuthoringItem, content: Value) {
        self.authoring.insert(item, content);
    }

    fn data_model(&mut self, id: &Id, data_model: &DataModel) {
        for (object, declared) in &data_model.objects {
            self.member(
                DeclarationKey::DataObject {
                    data_model: id.clone(),
                    object: object.clone(),
                },
                canonical(declared),
            );
        }

        for (outbox, declared) in &data_model.outboxes {
            self.member(
                DeclarationKey::Outbox {
                    data_model: id.clone(),
                    outbox: outbox.clone(),
                },
                canonical(declared),
            );
        }

        self.member(
            DeclarationKey::DataModel(id.clone()),
            without(canonical(data_model), &["objects", "outboxes"]),
        );
    }

    fn state_machine(&mut self, id: &Id, machine: &StateMachine) {
        for (transition, declared) in &machine.transitions {
            self.member(
                DeclarationKey::Transition {
                    machine: id.clone(),
                    transition: transition.clone(),
                },
                canonical(declared),
            );
        }

        self.member(
            DeclarationKey::StateMachine(id.clone()),
            without(canonical(machine), &["transitions"]),
        );
    }

    fn operation(&mut self, id: &Id, operation: &DraftOperation) {
        let DraftOperation {
            service,
            description,
            inputs,
            sketch,
            program,
            requirements,
            stage,
        } = operation;

        self.member(
            DeclarationKey::Operation(id.clone()),
            serde_json::json!({ "service": service }),
        );

        if let Some(description) = description {
            self.authoring(
                AuthoringItem::OperationDescription(id.clone()),
                canonical(description),
            );
        }

        if let Some(sketch) = sketch {
            self.authoring(
                AuthoringItem::OperationSketch(id.clone()),
                canonical(sketch),
            );
        }

        self.authoring(AuthoringItem::OperationStage(id.clone()), canonical(stage));

        for (input, declared) in inputs {
            self.member(
                DeclarationKey::Input {
                    operation: id.clone(),
                    input: input.clone(),
                },
                canonical(declared),
            );
        }

        if let Some(program) = program {
            let mut residual = canonical(program);

            self.block(id, program, &mut residual);
            self.member(DeclarationKey::Program(id.clone()), residual);
        }

        let subject = ObligationSubject::Operation {
            operation: id.clone(),
        };

        let idempotency = basis::requirement_keys(
            &subject,
            ObligationProperty::Idempotency,
            requirements
                .idempotency
                .iter()
                .map(|requirement| requirement.key.components.clone()),
        );

        for (key, requirement) in idempotency.iter().zip(&requirements.idempotency) {
            self.requirement(key, canonical(requirement));
        }

        let recoverability = basis::requirement_keys(
            &subject,
            ObligationProperty::Recoverability,
            requirements
                .recoverability
                .iter()
                .map(|requirement| requirement.key.components.clone()),
        );

        for (key, requirement) in recoverability.iter().zip(&requirements.recoverability) {
            self.requirement(key, canonical(requirement));
        }
    }

    /// A requirement member: everything of the requirement but the key
    /// that identifies it.
    fn requirement(&mut self, key: &ObligationKey, requirement: Value) {
        self.member(
            DeclarationKey::Requirement(key.requirement()),
            without(requirement, &["key"]),
        );
    }

    /// Projects one block of `operation`'s program in place, and adds the
    /// members its steps declare.
    fn block(&mut self, operation: &Id, block: &OperationBlock, residual: &mut Value) {
        let OperationBlock { steps } = block;

        for (index, step) in steps.iter().enumerate() {
            let residual = &mut residual["steps"][index];

            match step {
                OperationStep::Transaction(execute) => {
                    self.transaction(operation, &execute.transaction);

                    residual["transaction"] = Value::String(execute.transaction.id.0.clone());

                    if let Some(rejected) = &execute.rejected {
                        self.block(operation, rejected, &mut residual["rejected"]);
                    }
                }

                OperationStep::ExecuteEffect(execute) => {
                    self.effect_site(operation, &execute.effect_id, canonical(&execute.effect));

                    residual["effect"] = Value::String(execute.effect_id.0.clone());
                }

                OperationStep::ExecuteEffectAsync(execute) => {
                    self.effect_site(operation, &execute.effect_id, canonical(&execute.effect));

                    residual["effect"] = Value::String(execute.effect_id.0.clone());
                }

                OperationStep::MatchResult(matched) => {
                    self.block(operation, &matched.ok, &mut residual["ok"]);

                    for (error, arm) in &matched.errors {
                        self.block(operation, arm, &mut residual["errors"][&error.0]);
                    }
                }

                OperationStep::Branch(branch) => {
                    self.block(operation, &branch.then, &mut residual["then"]);

                    if let Some(otherwise) = &branch.otherwise {
                        self.block(operation, otherwise, &mut residual["otherwise"]);
                    }
                }

                // Control structure alone, owned by the program.
                OperationStep::ExecuteEffectIntent(_)
                | OperationStep::ExecuteEffectIntentAsync(_)
                | OperationStep::JoinAll(_)
                | OperationStep::Race(_)
                | OperationStep::Return(_)
                | OperationStep::Complete
                | OperationStep::Abandon => {}
            }
        }
    }

    /// A transaction member, minus its id and requirements, with the
    /// effect contracts of its steps held by effect id; and the members
    /// it declares.
    fn transaction(&mut self, operation: &Id, transaction: &Transaction) {
        let mut residual = without(canonical(transaction), &["id", "requirements"]);

        for (index, step) in transaction.steps.iter().enumerate() {
            let residual = &mut residual["steps"][index];

            match step {
                TransactionStep::EstablishEffectIntent(establish) => {
                    self.effect_site(
                        operation,
                        &establish.effect_id,
                        canonical(&establish.effect),
                    );

                    residual["effect"] = Value::String(establish.effect_id.0.clone());
                }

                TransactionStep::WriteOutbox(write) => {
                    self.effect_site(operation, &write.effect_id, canonical(&write.effect));

                    residual["effect"] = Value::String(write.effect_id.0.clone());
                }

                TransactionStep::Read(_)
                | TransactionStep::Update(_)
                | TransactionStep::CompareAndSet(_)
                | TransactionStep::Insert(_)
                | TransactionStep::Upsert(_)
                | TransactionStep::Delete(_)
                | TransactionStep::Lock(_)
                | TransactionStep::Transition(_)
                | TransactionStep::AdvanceCursor(_)
                | TransactionStep::Fence(_)
                | TransactionStep::EstablishTransactionOutput(_) => {}
            }
        }

        self.member(
            DeclarationKey::Transaction {
                operation: operation.clone(),
                transaction: transaction.id.clone(),
            },
            residual,
        );

        let serializability = basis::transaction_keys(
            operation,
            transaction,
            ObligationProperty::TransactionSerializability,
        );

        for (key, requirement) in serializability
            .iter()
            .zip(&transaction.requirements.serializability)
        {
            self.requirement(key, canonical(requirement));
        }

        let ordering = basis::transaction_keys(
            operation,
            transaction,
            ObligationProperty::TransactionOrdering,
        );

        for (key, requirement) in ordering.iter().zip(&transaction.requirements.ordering) {
            self.requirement(key, canonical(requirement));
        }
    }

    fn effect_site(&mut self, operation: &Id, effect: &Id, contract: Value) {
        self.member(
            DeclarationKey::EffectSite {
                operation: operation.clone(),
                effect: effect.clone(),
            },
            contract,
        );
    }

    fn runtime(&mut self, runtime: &RuntimeModel) {
        let RuntimeModel {
            topics,
            subscriptions,
            outboxes,
            execution_pools,
            routers,
            storage_layouts,
        } = runtime;

        for (topic, declared) in topics {
            self.member(
                DeclarationKey::TopicRuntime(topic.clone()),
                canonical(declared),
            );
        }

        for (operation, inputs) in subscriptions {
            for (input, declared) in inputs {
                self.member(
                    DeclarationKey::SubscriptionRuntime {
                        operation: operation.clone(),
                        input: input.clone(),
                    },
                    canonical(declared),
                );
            }
        }

        for (operation, inputs) in outboxes {
            for (input, declared) in inputs {
                self.member(
                    DeclarationKey::OutboxRuntime {
                        operation: operation.clone(),
                        input: input.clone(),
                    },
                    canonical(declared),
                );
            }
        }

        for (pool, declared) in execution_pools {
            self.member(
                DeclarationKey::ExecutionPool(pool.clone()),
                canonical(declared),
            );
        }

        for (router, declared) in routers {
            self.member(DeclarationKey::Router(router.clone()), canonical(declared));
        }

        for (layout, declared) in storage_layouts {
            self.member(
                DeclarationKey::StorageLayout(layout.clone()),
                canonical(declared),
            );
        }
    }
}

/// A declaration's canonical form: the one the store writes, as a value.
fn canonical<T: Serialize + ?Sized>(value: &T) -> Value {
    // Spec types serialize to maps with string keys, sequences, and
    // scalars only.
    serde_json::to_value(value).expect("a spec type serializes")
}

/// A canonical form without the fields other members own.
fn without(mut value: Value, fields: &[&str]) -> Value {
    if let Value::Object(map) = &mut value {
        for field in fields {
            map.remove(*field);
        }
    }

    value
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::Path;

    use serde_yaml::Value as Yaml;

    use super::*;
    use crate::design::change::diff::{Change, declaration_changes};
    use crate::design::store::{self, FileSet, FileTree};
    use crate::design::workspace::{
        OperationDraftStage, PromptObligation, PromptObligationId, PromptObligationStatus,
        ProposalStatus, ProposedRequirement, RequirementOrigin, RequirementProposal, RunId,
        RunMetadata,
    };
    use crate::spec::{IdempotencyKey, IdempotencyRequirement, ResultReplayRequirement, ValueRef};

    fn fixture(name: &str) -> WorkspaceState {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name);

        let model = crate::parser::yaml::parse(&std::fs::read_to_string(&path).unwrap())
            .unwrap_or_else(|error| panic!("{name}: {error}"));

        WorkspaceState::from_model(&model, RunMetadata::new(RunId(name.to_string())))
    }

    /// Every fixture's workspace, and one carrying every kind of
    /// authoring content and intent besides.
    fn workspaces() -> Vec<(String, WorkspaceState)> {
        let mut workspaces: Vec<(String, WorkspaceState)> = [
            "minimal.yaml",
            "flash_checkout.yaml",
            "payment_capture.yaml",
            "keyed_topic.yaml",
            "video_streaming.yaml",
            "shop.yaml",
            "transactional_outbox.yaml",
            "tenant_ledger.yaml",
            "hedged_read.yaml",
        ]
        .into_iter()
        .map(|name| (name.to_string(), fixture(name)))
        .collect();

        workspaces.push(("authored".to_string(), authored()));

        workspaces
    }

    /// Flash checkout with a sketch, descriptions, a planned operation,
    /// prompt obligations, and a requirement proposal.
    fn authored() -> WorkspaceState {
        let mut workspace = fixture("flash_checkout.yaml");

        let payment = workspace
            .operations
            .get_mut(&Id("operation.apply_payment".to_string()))
            .unwrap();

        payment.description = Some("Applies a captured payment to its order.".to_string());
        payment.sketch = Some(
            serde_json::from_value(serde_json::json!({ "steps": [], "returns": ["order_id"] }))
                .unwrap(),
        );

        let mut planned = workspace.operations.values().next().unwrap().clone();

        planned.program = None;
        planned.stage = OperationDraftStage::Planned;

        workspace
            .operations
            .insert(Id("operation.refund".to_string()), planned);

        if let Some(Schema::Canonical(schema)) = workspace.schemas.values_mut().next() {
            schema.description = Some("A record.".to_string());
        }

        workspace.prompt_obligations.insert(
            PromptObligationId("obligation.no_double_charge".to_string()),
            PromptObligation {
                source_span: Some("never charge twice".to_string()),
                normalized_intent: "a payment applies once".to_string(),
                targets: vec![Id("operation.apply_payment".to_string())],
                status: PromptObligationStatus::Unmapped,
            },
        );

        workspace.requirement_proposals.push(RequirementProposal {
            operation: Id("operation.apply_payment".to_string()),
            requirement: ProposedRequirement::Idempotency(IdempotencyRequirement {
                key: IdempotencyKey {
                    components: vec![ValueRef {
                        source: crate::spec::ValueSource::Input(Id(
                            "input.apply_payment.captured".to_string()
                        )),
                        path: crate::spec::FieldPath(vec!["order_id".to_string()]),
                    }],
                },
                result: ResultReplayRequirement::Unspecified,
            }),
            origin: RequirementOrigin::Recommended {
                rationale: "a payment is applied once per order".to_string(),
                evidence: Vec::new(),
            },
            status: ProposalStatus::Proposed,
        });

        workspace
    }

    fn files(workspace: &WorkspaceState) -> FileSet {
        store::render(workspace).expect("the workspace renders")
    }

    /// The workspace the files hold, through the store's own loader.
    fn load(files: &FileSet) -> Option<WorkspaceState> {
        store::load_source(&FileTree::of_files("test:", files.clone()))
            .ok()
            .flatten()
    }

    /// Where one leaf sits in a YAML document.
    #[derive(Debug, Clone)]
    enum Step {
        Key(Yaml),
        Index(usize),
    }

    fn leaves(value: &Yaml, at: &mut Vec<Step>, found: &mut Vec<Vec<Step>>) {
        match value {
            Yaml::Mapping(map) => {
                for (key, value) in map {
                    at.push(Step::Key(key.clone()));
                    leaves(value, at, found);
                    at.pop();
                }
            }

            Yaml::Sequence(items) => {
                for (index, value) in items.iter().enumerate() {
                    at.push(Step::Index(index));
                    leaves(value, at, found);
                    at.pop();
                }
            }

            Yaml::Tagged(tagged) => leaves(&tagged.value, at, found),

            Yaml::Null | Yaml::Bool(_) | Yaml::Number(_) | Yaml::String(_) => {
                found.push(at.clone())
            }
        }
    }

    fn leaf_mut<'a>(value: &'a mut Yaml, path: &[Step]) -> &'a mut Yaml {
        let Some((first, rest)) = path.split_first() else {
            return value;
        };

        let value = match value {
            Yaml::Tagged(tagged) => &mut tagged.value,
            value => value,
        };

        match (first, value) {
            (Step::Key(key), Yaml::Mapping(map)) => leaf_mut(map.get_mut(key).unwrap(), rest),
            (Step::Index(index), Yaml::Sequence(items)) => leaf_mut(&mut items[*index], rest),
            _ => unreachable!("a path found in this document"),
        }
    }

    /// The leaf changed, and the string it was and is when it is one.
    fn perturb(leaf: &mut Yaml) -> Option<(String, String)> {
        match leaf {
            Yaml::String(text) => {
                let old = text.clone();

                text.push('~');

                Some((old, text.clone()))
            }

            Yaml::Bool(flag) => {
                *flag = !*flag;

                None
            }

            Yaml::Number(number) => {
                *leaf = match number.as_i64() {
                    Some(value) => Yaml::from(value + 1),
                    None => Yaml::from(number.as_f64().unwrap_or_default() + 1.0),
                };

                None
            }

            Yaml::Null => {
                *leaf = Yaml::String("perturbed".to_string());

                None
            }

            Yaml::Mapping(_) | Yaml::Sequence(_) | Yaml::Tagged(_) => {
                unreachable!("only scalars are leaves")
            }
        }
    }

    /// A declaration key with every string spelled `old` respelled
    /// `new`: what a rename makes of it.
    fn respelled(key: &DeclarationKey, old: &str, new: &str) -> DeclarationKey {
        fn respell(value: &mut Value, old: &str, new: &str) {
            match value {
                Value::String(text) if text == old => *text = new.to_string(),
                Value::Array(items) => items.iter_mut().for_each(|item| respell(item, old, new)),
                Value::Object(map) => map.values_mut().for_each(|item| respell(item, old, new)),
                _ => {}
            }
        }

        let mut value = serde_json::to_value(key).unwrap();

        respell(&mut value, old, new);

        serde_json::from_value(value).unwrap()
    }

    /// §19.1: every leaf of every committed file is owned by exactly one
    /// member or authoring item. Perturbing one, wherever the files still
    /// load, changes exactly one residual or one authoring item; or, for
    /// a leaf that spells an identity, renames the members that identity
    /// names and changes at most the one member naming them (§7.4).
    #[test]
    fn every_leaf_is_owned_by_exactly_one_member_or_authoring_item() {
        let mut renames = 0;

        for (name, workspace) in workspaces() {
            let files = files(&workspace);
            let original = Partition::of(&load(&files).expect("the files load"));

            let mut checked = 0;

            for (file, text) in &files {
                let document: Yaml = serde_yaml::from_str(text).unwrap();

                let mut found = Vec::new();
                leaves(&document, &mut Vec::new(), &mut found);

                for path in found {
                    let mut perturbed = document.clone();
                    let respelling = perturb(leaf_mut(&mut perturbed, &path));

                    let mut changed = files.clone();
                    changed.insert(file.clone(), serde_yaml::to_string(&perturbed).unwrap());

                    let Some(loaded) = load(&changed) else {
                        continue;
                    };

                    let changes = declaration_changes(&original, &Partition::of(&loaded));
                    let at = || format!("{name}: {file} at {path:?}");

                    let of = |kind: Change| -> BTreeSet<DeclarationKey> {
                        changes
                            .changes
                            .iter()
                            .filter(|change| change.change == kind)
                            .map(|change| change.declaration.clone())
                            .collect()
                    };

                    let (added, removed) = (of(Change::Added), of(Change::Removed));
                    let modified = of(Change::Modified).len();
                    let authoring = changes.authoring.len();

                    if added.is_empty() && removed.is_empty() {
                        assert_eq!(modified + authoring, 1, "{}: {changes:#?}", at());
                    } else {
                        let (old, new) = respelling.unwrap_or_else(|| {
                            panic!("{}: only a respelled string renames: {changes:#?}", at())
                        });

                        let renamed: BTreeSet<DeclarationKey> = removed
                            .iter()
                            .map(|key| respelled(key, &old, &new))
                            .collect();

                        assert_eq!(renamed, added, "{}: {changes:#?}", at());
                        assert!(modified <= 1, "{}: {changes:#?}", at());
                        assert_eq!(authoring, 0, "{}: {changes:#?}", at());

                        renames += 1;
                    }

                    checked += 1;
                }
            }

            assert!(checked > 0, "{name}: no leaf loaded when perturbed");
        }

        assert!(renames > 0, "no identity leaf was perturbed");
    }

    /// §19.6: a sketch, a stage, and a description are authoring alone.
    #[test]
    fn authoring_changes_are_never_semantic() {
        let base = fixture("flash_checkout.yaml");
        let mut head = base.clone();

        let operation = head
            .operations
            .get_mut(&Id("operation.apply_payment".to_string()))
            .unwrap();

        operation.sketch =
            Some(serde_json::from_value(serde_json::json!({ "steps": [] })).unwrap());
        operation.stage = OperationDraftStage::RequirementsProposed;
        operation.description = Some("Applies a payment.".to_string());

        let changes = declaration_changes(&Partition::of(&base), &Partition::of(&head));

        assert!(changes.is_empty(), "{changes:#?}");

        let id = Id("operation.apply_payment".to_string());

        assert_eq!(
            changes
                .authoring
                .iter()
                .map(|change| (change.item.clone(), change.change))
                .collect::<Vec<_>>(),
            vec![
                (AuthoringItem::OperationSketch(id.clone()), Change::Added),
                (AuthoringItem::OperationStage(id.clone()), Change::Modified),
                (AuthoringItem::OperationDescription(id), Change::Modified),
            ]
        );
    }

    /// §19.8: a renamed transaction is removed and added, with its
    /// requirements, and nothing pairs the two.
    #[test]
    fn a_renamed_transaction_is_removed_and_added() {
        let base = fixture("flash_checkout.yaml");
        let mut head = base.clone();

        let operation = Id("operation.apply_payment".to_string());
        let (old, new) = (
            Id("tx.apply_payment".to_string()),
            Id("tx.apply_payment.v2".to_string()),
        );

        head.operations
            .get_mut(&operation)
            .and_then(|draft| draft.program.as_mut())
            .and_then(|program| program.transaction_mut(&old))
            .unwrap()
            .id = new.clone();

        let changes = declaration_changes(&Partition::of(&base), &Partition::of(&head));

        let summary: Vec<(&str, Change)> = changes
            .changes
            .iter()
            .map(|change| (change.declaration.kind(), change.change))
            .collect();

        assert_eq!(
            summary,
            vec![
                ("program", Change::Modified),
                ("transaction", Change::Removed),
                ("transaction", Change::Added),
                ("requirement", Change::Removed),
                ("requirement", Change::Removed),
                ("requirement", Change::Added),
                ("requirement", Change::Added),
            ]
        );

        assert!(changes.changes.iter().any(|change| change.declaration
            == DeclarationKey::Transaction {
                operation: operation.clone(),
                transaction: old.clone(),
            }
            && change.change == Change::Removed));

        assert!(changes.changes.iter().any(|change| change.declaration
            == DeclarationKey::Transaction {
                operation: operation.clone(),
                transaction: new.clone(),
            }
            && change.change == Change::Added));

        // The program names the transaction by id; nothing else moved.
        let program = &changes.changes[0];

        assert_eq!(
            program
                .delta
                .iter()
                .map(|entry| entry.path.as_str())
                .collect::<Vec<_>>(),
            vec!["steps[0].transaction"]
        );
    }
}
