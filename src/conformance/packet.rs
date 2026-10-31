//! Work items (§12 of the conformance revision): the packet for one
//! item, holding everything an agent needs to do it and nothing it can
//! fetch for itself.
//!
//! A packet states the declaration and what changed in it since its
//! record was attested, each condition in a sentence, the facts its
//! code must provide, the claims it must carry, and where its code and
//! its neighbours' code are. It references code by span and never
//! embeds a file, so its size is bounded by one declaration (§12.4).
//!
//! The load-bearing check is the sentence that stops an agent from
//! deleting a detail a proof rests on (§12.2): for every required fact
//! whose kind has a withdrawal in Part I's mutation catalogue, the
//! packet applies the withdrawal to the current model, verifies the
//! result, and names the obligations it would cost. Precedents show how
//! this project already provides each kind of fact, citing only
//! evidence another party has confirmed, since an unchecked idiom
//! spreading through the code is worse than none (§12.3).
//!
//! A packet carries its read set (§13.2): the declarations and design
//! graph queries it was built from, with their digests, and the record
//! it read. Attest re-derives them, and refuses work done against a
//! spec that has since moved.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::analyzer::basis::{
    ConformanceClass, DeclarationKey, Fact, ObligationKey, TextualKey, TransactionId,
};
use crate::analyzer::report::Status;
use crate::design::change::delta::{DeltaEntry, delta};
use crate::design::change::diff::verified;
use crate::design::change::mutation::withdrawal;
use crate::design::change::render::delta_entry;
use crate::design::fingerprint::SemanticHash;
use crate::design::graph_query::{self, GraphQuery, QueryResult, QueryRow};
use crate::design::workspace::WorkspaceState;
use crate::spec::{
    Effect, Id, IdempotencyGuarantee, Input, MessageIdentity, TransactionStep, TransitionEffect,
    TransitionSideEffect,
};

use super::Digest;
use super::attest::{self, LastRefusal};
use super::completeness;
use super::plan::{self, Priority};
use super::record::{ClaimScope, Party, Record, RecordedRegion, ReviewTarget, Span, Verdict};
use super::state::{
    self, Condition, ConditionKind, DeclarationState, Derivation, Orphan, ScopeFailure,
};

/// The packet for one work item (§12.1).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Packet {
    /// A bindable declaration's textual key, or an orphaned key.
    pub key: TextualKey,

    /// Its priority on the work list. Absent for a declaration with no
    /// conditions, which has no item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<Priority>,

    pub declaration: DeclarationSection,

    /// Each condition, in §10.1's order, stated in a sentence.
    pub conditions: Vec<Statement>,

    /// The reviews at the current digests disputing what its record
    /// carries, with their notes.
    pub disputes: Vec<OpenDispute>,

    /// Why its last attest was refused, while its record is still the
    /// one that attest saw.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_refusal: Option<LastRefusal>,

    /// The facts its code must provide (§7.3).
    pub required: Vec<RequiredSection>,

    /// The completeness claims its record must carry (§9.4).
    pub completeness: Vec<ClaimSection>,

    pub code: CodeSection,

    /// For each kind of attestable fact required of it, how this
    /// project already provides one.
    pub precedents: Vec<Precedents>,

    /// The `dsl_guide` topics defining the primitives it uses.
    pub semantics: Vec<&'static str>,

    pub done_when: Vec<Step>,

    pub read_set: ReadSet,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DeclarationSection {
    /// Its current residual, canonical. Absent for an orphaned key,
    /// which the spec no longer declares.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub residual: Option<Value>,

    /// Its record's path, relative to the project directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record: Option<String>,

    /// What changed in its residual since its readable record was
    /// attested: Part I's structural delta, from the recorded residual
    /// to the current one. Absent without a readable record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta: Option<Vec<DeltaEntry>>,
}

/// One condition, in a sentence.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Statement {
    pub condition: ConditionKind,
    pub statement: String,

    /// What the condition found. Absent for an orphaned key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<Condition>,
}

/// A review at the current digests disputing a record, or items of it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OpenDispute {
    pub by: Party,

    /// Its verdict on the record as a whole is `disputed`.
    pub record: bool,

    pub items: Vec<DisputedItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DisputedItem {
    pub target: ReviewTarget,

    /// The thing disputed, in words.
    pub about: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// A fact required of the declaration's code.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RequiredSection {
    /// In its canonical durable form.
    pub fact: Fact,

    pub rendering: String,
    pub class: ConformanceClass,

    /// Whether its record provides it: lists it, for an attestable
    /// fact, or claims structural conformance, for a structural one.
    pub provided: bool,

    /// The proven obligations whose bases hold it.
    pub obligations: Vec<Relying>,

    pub load_bearing: LoadBearing,
}

/// An obligation, by key and display name, with its summary line.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Relying {
    pub obligation: ObligationKey,
    pub display: String,
    pub summary: String,
}

/// What withdrawing a required fact costs (§12.2).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LoadBearing {
    /// The catalogue's withdrawal, applied to the current model and
    /// verified: the obligations proven now and not without the fact.
    Withdrawal {
        withdrawal: String,
        regressions: Vec<Regression>,
    },

    /// The withdrawal leaves a model that does not validate.
    Invalid { withdrawal: String, reason: String },

    /// The fact's kind has no withdrawal, or the model declares none
    /// for it: the obligations relying on it are what it costs.
    Relied,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Regression {
    pub obligation: ObligationKey,
    pub display: String,
    pub before: Status,

    /// Absent when the withdrawn model no longer has the obligation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<Status>,
}

/// A completeness claim required of the declaration's record.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ClaimSection {
    pub scope: ClaimScope,

    /// The facts of the reliance requiring it.
    pub because: Vec<Fact>,

    /// Why it does not hold now. Absent when it holds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<ScopeFailure>,

    /// The declared writers or publishers, in whose regions a match is
    /// accepted.
    pub declared: Listing,
}

/// Declarations with their regions, at most twenty, with the count of
/// the rest (§12.4).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Listing {
    pub declarations: Vec<Located>,

    #[serde(default, skip_serializing_if = "is_zero")]
    pub more: usize,
}

/// A declaration and its regions: paths and line ranges only, since the
/// agent reads the files itself.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Located {
    pub declaration: TextualKey,
    pub regions: Vec<Span>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CodeSection {
    /// The item's regions.
    pub regions: Vec<Span>,

    /// Its neighbours one hop away in the design graph, by relation.
    pub neighbours: Vec<Relation>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Relation {
    pub relation: RelationKind,

    #[serde(flatten)]
    pub listing: Listing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationKind {
    /// The effect sites calling its operation.
    Callers,

    /// The inputs its operation calls.
    Callees,

    /// The transactions reading an object it touches.
    Readers,

    /// The transactions writing an object it touches.
    Writers,

    /// The effect sites and transitions publishing to, or writing into,
    /// a channel it uses.
    Publishers,

    /// The inputs consuming a channel it uses.
    Consumers,
}

/// How this project already provides one kind of fact (§12.3).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Precedents {
    pub kind: &'static str,

    /// At most three, from distinct records.
    pub precedents: Vec<Precedent>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Precedent {
    pub declaration: TextualKey,
    pub fact: Fact,
    pub rendering: String,
    pub evidence: Vec<Span>,
}

/// One step of the checklist an item is done by.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Step {
    pub step: String,
    pub done: bool,
}

/// What a packet was built from (§13.2): the residual digests of the
/// declarations, the result digests of the design graph queries, and
/// the record it read. It mirrors the design engine's task read sets,
/// and like theirs its digests are runtime metadata, never committed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadSet {
    /// The item's declaration, and the anchors of the facts required of
    /// it.
    pub declarations: Vec<DeclarationRead>,

    pub queries: Vec<QueryRead>,

    /// The digest of the item's record file, `None` when it had none:
    /// the record an attest sending this read set replaces (§8.4).
    #[serde(default)]
    pub record: Option<Digest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeclarationRead {
    pub declaration: DeclarationKey,
    pub residual: SemanticHash,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryRead {
    pub query: GraphQuery,
    pub result: SemanticHash,
}

/// A member of a read set that no longer holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReadSetChange {
    /// The declaration's residual changed.
    Changed { declaration: DeclarationKey },

    /// The spec no longer declares it.
    Removed { declaration: DeclarationKey },

    /// The query's result changed.
    Query { query: GraphQuery },
}

impl ReadSet {
    /// The members that no longer hold in `derivation`'s spec, by the
    /// rule `stale_causes` applies to design tasks: residuals compared
    /// by digest, and every query rerun in full.
    pub fn changes(&self, derivation: &Derivation) -> Vec<ReadSetChange> {
        let mut changes = Vec::new();

        for read in &self.declarations {
            match derivation.partition.members.get(&read.declaration) {
                None => changes.push(ReadSetChange::Removed {
                    declaration: read.declaration.clone(),
                }),

                Some(residual) if SemanticHash::of(residual) != read.residual => {
                    changes.push(ReadSetChange::Changed {
                        declaration: read.declaration.clone(),
                    });
                }

                Some(_) => {}
            }
        }

        for read in &self.queries {
            let result = graph_query::run(derivation.workspace, &derivation.graph, &read.query);

            if result.fingerprint != read.result {
                changes.push(ReadSetChange::Query {
                    query: read.query.clone(),
                });
            }
        }

        changes
    }
}

/// The packet for `key` (§12): a bindable declaration of the spec, with
/// or without conditions, or an orphaned key. `None` for any other key.
pub fn packet(derivation: &Derivation, key: &TextualKey) -> Option<Packet> {
    if let Some(state) = derivation.state.declaration(key) {
        return Some(Builder::new(derivation).declaration(state));
    }

    derivation
        .state
        .orphaned
        .iter()
        .find(|orphan| &orphan.key == key)
        .map(|orphan| Builder::new(derivation).orphan(orphan))
}

/// Builds one packet, recording the queries it runs.
struct Builder<'d, 'a> {
    derivation: &'d Derivation<'a>,
    queries: Vec<QueryRead>,
}

impl<'d, 'a> Builder<'d, 'a> {
    fn new(derivation: &'d Derivation<'a>) -> Self {
        Self {
            derivation,
            queries: Vec::new(),
        }
    }

    /// Runs a design graph query, recording it in the read set.
    fn run(&mut self, query: GraphQuery) -> QueryResult {
        let result = graph_query::run(self.derivation.workspace, &self.derivation.graph, &query);

        if !self.queries.iter().any(|read| read.query == query) {
            self.queries.push(QueryRead {
                query,
                result: result.fingerprint,
            });
        }

        result
    }

    fn declaration(mut self, state: &DeclarationState) -> Packet {
        let derivation = self.derivation;
        let key = &state.declaration;
        let declaration = key.key();
        let file = derivation.records.get(key);
        let record = derivation.record(key);
        let residual = derivation.partition.members.get(declaration);

        let blocks = plan::blocked(&derivation.state)
            .remove(key)
            .unwrap_or_default();

        let current = state::digests(&state.regions);

        let required: Vec<RequiredSection> = state
            .required
            .iter()
            .map(|required| RequiredSection {
                fact: required.fact.clone(),
                rendering: required.fact.to_string(),
                class: required.class,
                provided: match required.class {
                    ConformanceClass::Structural => record.is_some_and(|record| record.structural),

                    _ => record.is_some_and(|record| {
                        record
                            .facts
                            .iter()
                            .any(|attested| attested.fact.canonical() == required.fact.canonical())
                    }),
                },
                obligations: required
                    .relied_on_by
                    .iter()
                    .map(|obligation| self.relying(obligation))
                    .collect(),
                load_bearing: load_bearing(derivation, &required.fact),
            })
            .collect();

        let completeness: Vec<ClaimSection> = state
            .claims
            .iter()
            .map(|scope| {
                let failure = state
                    .conditions
                    .iter()
                    .find_map(|condition| match condition {
                        Condition::Incomplete { scopes } => scopes
                            .iter()
                            .find(|incomplete| incomplete.scope == scope.scope)
                            .map(|incomplete| incomplete.failure.clone()),

                        _ => None,
                    });

                let declared = match completeness::declared_query(scope.scope, declaration) {
                    Some(query) => {
                        let rows = self.run(query).rows;

                        rows.into_iter()
                            .filter_map(|row| match row {
                                QueryRow::Access(access) => Some(DeclarationKey::Transaction {
                                    operation: access.transaction.operation,
                                    transaction: access.transaction.transaction,
                                }),

                                QueryRow::Publisher(publisher) => {
                                    Some(completeness::site_declaration(publisher.site))
                                }

                                _ => None,
                            })
                            .collect()
                    }

                    None => BTreeSet::new(),
                };

                ClaimSection {
                    scope: scope.scope,
                    because: scope.because.clone(),
                    failure,
                    declared: self.listing(declared),
                }
            })
            .collect();

        let neighbours = self.neighbours(declaration);

        let mut reads: BTreeMap<&DeclarationKey, SemanticHash> = BTreeMap::new();

        if let Some(residual) = residual {
            reads.insert(declaration, SemanticHash::of(residual));
        }

        for required in &state.required {
            for anchor in derivation
                .anchors
                .get(&required.fact.canonical())
                .into_iter()
                .flatten()
            {
                if let Some(residual) = derivation.partition.members.get(anchor) {
                    reads.insert(anchor, SemanticHash::of(residual));
                }
            }
        }

        let disputes = record
            .map(|record| disputes(record, &current))
            .unwrap_or_default();

        let last_refusal = attest::last_refusal(&derivation.project_dir, key)
            .filter(|refusal| refusal.record == file.map(|file| file.digest));

        Packet {
            key: key.clone(),
            priority: (!state.is_current()).then(|| plan::priority(state, &blocks)),
            declaration: DeclarationSection {
                residual: residual.cloned(),
                record: file.map(|file| state::relative(&derivation.project_dir, &file.path)),
                delta: record
                    .zip(residual)
                    .map(|(record, residual)| delta(&record.residual, residual)),
            },
            conditions: state
                .conditions
                .iter()
                .map(|condition| Statement {
                    condition: condition.kind(),
                    statement: sentence(condition, key, record),
                    detail: Some(condition.clone()),
                })
                .collect(),
            disputes,
            last_refusal,
            precedents: precedents(derivation, state),
            semantics: topics(derivation.workspace, declaration, state),
            done_when: done_when(state, record),
            required,
            completeness,
            code: CodeSection {
                regions: spans(&state.regions),
                neighbours,
            },
            read_set: ReadSet {
                declarations: reads
                    .into_iter()
                    .map(|(declaration, residual)| DeclarationRead {
                        declaration: declaration.clone(),
                        residual,
                    })
                    .collect(),
                queries: self.read_queries(),
                record: file.map(|file| file.digest),
            },
        }
    }

    fn orphan(self, orphan: &Orphan) -> Packet {
        let derivation = self.derivation;
        let file = derivation.records.get(&orphan.key);

        let mut done_when = vec![Step {
            step: format!(
                "Remove the `{}` markers, or bind their code to the declaration that replaced \
                 it.",
                orphan.key
            ),
            done: orphan.regions.is_empty(),
        }];

        if let Some(record) = &orphan.record {
            done_when.push(Step {
                step: format!("Delete its record `{record}`."),
                done: false,
            });
        }

        Packet {
            key: orphan.key.clone(),
            priority: Some(Priority::Cleanup),
            declaration: DeclarationSection {
                residual: None,
                record: orphan.record.clone(),
                delta: None,
            },
            conditions: vec![Statement {
                condition: ConditionKind::Orphaned,
                statement: orphaned(orphan),
                detail: None,
            }],
            disputes: Vec::new(),
            last_refusal: None,
            required: Vec::new(),
            completeness: Vec::new(),
            code: CodeSection {
                regions: spans(&orphan.regions),
                neighbours: Vec::new(),
            },
            precedents: Vec::new(),
            semantics: Vec::new(),
            done_when,
            read_set: ReadSet {
                declarations: Vec::new(),
                queries: Vec::new(),
                record: file.map(|file| file.digest),
            },
        }
    }

    fn relying(&self, obligation: &ObligationKey) -> Relying {
        Relying {
            obligation: obligation.clone(),
            display: obligation.to_string(),
            summary: self
                .derivation
                .obligation(obligation)
                .map(|obligation| obligation.summary.clone())
                .unwrap_or_default(),
        }
    }

    /// Declarations with their current regions, at most twenty.
    fn listing(&self, declarations: BTreeSet<DeclarationKey>) -> Listing {
        let total = declarations.len();

        let declarations: Vec<Located> = declarations
            .into_iter()
            .filter_map(TextualKey::new)
            .take(LISTED)
            .map(|declaration| Located {
                regions: self
                    .derivation
                    .regions_of(declaration.key())
                    .map(|region| Span {
                        path: region.path.clone(),
                        lines: region.lines,
                    })
                    .collect(),
                declaration,
            })
            .collect();

        Listing {
            more: total.saturating_sub(declarations.len()),
            declarations,
        }
    }

    /// The declarations one hop away in the design graph (§12.1):
    /// callers and callees of its operation, readers and writers of the
    /// objects it touches, and publishers and consumers of its channels.
    fn neighbours(&mut self, declaration: &DeclarationKey) -> Vec<Relation> {
        let reach = reach(self.derivation.workspace, declaration);
        let mut related: BTreeMap<RelationKind, BTreeSet<DeclarationKey>> = BTreeMap::new();

        if let Some(operation) = reach.operation {
            for row in self
                .run(GraphQuery::Callers {
                    operation: operation.clone(),
                })
                .rows
            {
                if let QueryRow::Call(call) = row {
                    related
                        .entry(RelationKind::Callers)
                        .or_default()
                        .insert(completeness::site_declaration(call.site));
                }
            }

            for row in self.run(GraphQuery::Callees { operation }).rows {
                if let QueryRow::Call(call) = row {
                    related.entry(RelationKind::Callees).or_default().insert(
                        DeclarationKey::Input {
                            operation: call.target,
                            input: call.target_input,
                        },
                    );
                }
            }
        }

        for object in reach.objects {
            let DeclarationKey::DataObject { data_model, object } = object else {
                continue;
            };

            for (relation, query) in [
                (
                    RelationKind::Readers,
                    GraphQuery::Readers {
                        data_model: data_model.clone(),
                        object: object.clone(),
                        field: None,
                    },
                ),
                (
                    RelationKind::Writers,
                    GraphQuery::Writers {
                        data_model,
                        object,
                        field: None,
                    },
                ),
            ] {
                for row in self.run(query).rows {
                    if let QueryRow::Access(access) = row {
                        related
                            .entry(relation)
                            .or_default()
                            .insert(DeclarationKey::Transaction {
                                operation: access.transaction.operation,
                                transaction: access.transaction.transaction,
                            });
                    }
                }
            }
        }

        for channel in reach.channels {
            for row in self
                .run(GraphQuery::Publishers {
                    topic: channel.clone(),
                })
                .rows
            {
                if let QueryRow::Publisher(publisher) = row {
                    related
                        .entry(RelationKind::Publishers)
                        .or_default()
                        .insert(completeness::site_declaration(publisher.site));
                }
            }

            for row in self.run(GraphQuery::Consumers { topic: channel }).rows {
                if let QueryRow::Consumer(consumer) = row {
                    related.entry(RelationKind::Consumers).or_default().insert(
                        DeclarationKey::Input {
                            operation: consumer.operation,
                            input: consumer.input,
                        },
                    );
                }
            }
        }

        related
            .into_iter()
            .filter_map(|(relation, mut declarations)| {
                declarations.remove(declaration);

                (!declarations.is_empty()).then(|| Relation {
                    relation,
                    listing: self.listing(declarations),
                })
            })
            .collect()
    }

    /// The recorded queries, in canonical order.
    fn read_queries(&mut self) -> Vec<QueryRead> {
        let mut queries = std::mem::take(&mut self.queries);

        queries.sort_by_key(|read| SemanticHash::of(&read.query));

        queries
    }
}

/// How many declarations a listing names before counting the rest
/// (§12.4).
const LISTED: usize = 20;

fn is_zero(count: &usize) -> bool {
    *count == 0
}

fn spans(regions: &[RecordedRegion]) -> Vec<Span> {
    regions
        .iter()
        .map(|region| Span {
            path: region.path.clone(),
            lines: region.lines,
        })
        .collect()
}

/// Where a declaration reaches in the design graph: its operation, the
/// objects it touches, and the channels it uses.
#[derive(Default)]
struct Reach {
    operation: Option<Id>,
    objects: BTreeSet<DeclarationKey>,
    channels: BTreeSet<Id>,
}

fn reach(workspace: &WorkspaceState, declaration: &DeclarationKey) -> Reach {
    let program = |operation: &Id| {
        workspace
            .operations
            .get(operation)
            .and_then(|draft| draft.program.as_ref())
    };

    let all_objects = |operation: &Id| -> BTreeSet<DeclarationKey> {
        let transactions: Vec<TransactionId> = program(operation)
            .map(|program| {
                program
                    .transactions()
                    .into_iter()
                    .map(|(_, transaction)| TransactionId {
                        operation: operation.clone(),
                        transaction: transaction.id.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default();

        completeness::accessed(workspace, transactions.iter())
            .into_iter()
            .collect()
    };

    let effect_channels = |operation: &Id| -> BTreeSet<Id> {
        let Some(program) = program(operation) else {
            return BTreeSet::new();
        };

        program
            .effect_declarations()
            .into_iter()
            .filter_map(|(_, effect)| effect_channel(effect))
            .chain(
                program
                    .outbox_write_declarations()
                    .into_iter()
                    .map(|(_, write)| write.effect.outbox.clone()),
            )
            .collect()
    };

    let input_channel = |operation: &Id, input: &Id| -> Option<Id> {
        match workspace.operations.get(operation)?.inputs.get(input)? {
            Input::Request(_) => None,
            Input::Subscription(subscription) => Some(subscription.topic.clone()),
            Input::Outbox(outbox) => Some(outbox.outbox.clone()),
        }
    };

    match declaration {
        DeclarationKey::Transaction {
            operation,
            transaction,
        } => Reach {
            operation: Some(operation.clone()),
            objects: completeness::accessed(
                workspace,
                [TransactionId {
                    operation: operation.clone(),
                    transaction: transaction.clone(),
                }]
                .iter(),
            )
            .into_iter()
            .collect(),
            channels: BTreeSet::new(),
        },

        DeclarationKey::EffectSite { operation, effect } => Reach {
            operation: Some(operation.clone()),
            objects: BTreeSet::new(),
            channels: site_effect(workspace, operation, effect)
                .into_iter()
                .collect(),
        },

        DeclarationKey::Program(operation) => Reach {
            operation: Some(operation.clone()),
            objects: all_objects(operation),
            channels: effect_channels(operation),
        },

        DeclarationKey::Operation(operation) => {
            let mut channels = effect_channels(operation);

            if let Some(draft) = workspace.operations.get(operation) {
                channels.extend(
                    draft
                        .inputs
                        .keys()
                        .filter_map(|input| input_channel(operation, input)),
                );
            }

            Reach {
                operation: Some(operation.clone()),
                objects: all_objects(operation),
                channels,
            }
        }

        DeclarationKey::Input { operation, input } => Reach {
            operation: Some(operation.clone()),
            objects: BTreeSet::new(),
            channels: input_channel(operation, input).into_iter().collect(),
        },

        DeclarationKey::DataObject { .. } => Reach {
            objects: BTreeSet::from([declaration.clone()]),
            ..Reach::default()
        },

        DeclarationKey::Outbox {
            outbox: channel, ..
        }
        | DeclarationKey::Topic(channel)
        | DeclarationKey::TopicRuntime(channel) => Reach {
            channels: BTreeSet::from([channel.clone()]),
            ..Reach::default()
        },

        DeclarationKey::Transition {
            machine,
            transition,
        } => Reach {
            channels: workspace
                .state_machines
                .get(machine)
                .and_then(|state_machine| state_machine.transitions.get(transition))
                .map(|transition| {
                    transition
                        .side_effects
                        .values()
                        .filter_map(|side_effect| match side_effect {
                            TransitionSideEffect::Publication(publication) => {
                                Some(publication.topic.clone())
                            }

                            TransitionSideEffect::Request(_) => None,
                        })
                        .chain(transition.effects.values().map(|effect| match effect {
                            TransitionEffect::OutboxWrite(write) => write.outbox.clone(),
                        }))
                        .collect()
                })
                .unwrap_or_default(),
            ..Reach::default()
        },

        DeclarationKey::SubscriptionRuntime { operation, input }
        | DeclarationKey::OutboxRuntime { operation, input } => Reach {
            channels: input_channel(operation, input).into_iter().collect(),
            ..Reach::default()
        },

        DeclarationKey::Service(_)
        | DeclarationKey::Schema(_)
        | DeclarationKey::DataModel(_)
        | DeclarationKey::StateMachine(_)
        | DeclarationKey::Requirement(_)
        | DeclarationKey::ExecutionPool(_)
        | DeclarationKey::Router(_)
        | DeclarationKey::StorageLayout(_)
        | DeclarationKey::PromptObligation(_) => Reach::default(),
    }
}

/// The channel an effect publishes to or writes into.
fn effect_channel(effect: &Effect) -> Option<Id> {
    match effect {
        Effect::Publication(publication) => Some(publication.topic.clone()),
        Effect::OutboxWrite(write) => Some(write.outbox.clone()),
        Effect::Request(_) | Effect::External(_) => None,
    }
}

/// The channel an operation's effect site publishes to or writes into.
fn site_effect(workspace: &WorkspaceState, operation: &Id, effect: &Id) -> Option<Id> {
    let program = workspace.operations.get(operation)?.program.as_ref()?;

    if let Some((_, declared)) = program
        .effect_declarations()
        .into_iter()
        .find(|(id, _)| *id == effect)
    {
        return effect_channel(declared);
    }

    program
        .outbox_write_declarations()
        .into_iter()
        .find(|(_, write)| &write.effect_id == effect)
        .map(|(_, write)| write.effect.outbox.clone())
}

/// What withdrawing a fact costs (§12.2): the catalogue's withdrawal,
/// applied to the current model and verified, against the obligations
/// proven now.
fn load_bearing(derivation: &Derivation, fact: &Fact) -> LoadBearing {
    let Some(mutation) = withdrawal(fact) else {
        return LoadBearing::Relied;
    };

    let Some(withdrawn) = mutation.apply(derivation.workspace) else {
        return LoadBearing::Relied;
    };

    let report = match verified(&withdrawn) {
        Ok(report) => report,

        Err(reason) => {
            return LoadBearing::Invalid {
                withdrawal: mutation.to_string(),
                reason,
            };
        }
    };

    let after: BTreeMap<&ObligationKey, Status> = report
        .obligations
        .iter()
        .map(|obligation| (&obligation.key, obligation.status))
        .collect();

    let regressions = derivation
        .report
        .obligations
        .iter()
        .filter(|obligation| obligation.status == Status::Proven)
        .filter_map(|obligation| {
            let status = after.get(&obligation.key).copied();

            (status != Some(Status::Proven)).then(|| Regression {
                obligation: obligation.key.clone(),
                display: obligation.key.to_string(),
                before: Status::Proven,
                after: status,
            })
        })
        .collect();

    LoadBearing::Withdrawal {
        withdrawal: mutation.to_string(),
        regressions,
    }
}

/// The reviews at the current digests disputing a record, or items of
/// it, with their notes.
fn disputes(record: &Record, current: &[Digest]) -> Vec<OpenDispute> {
    record
        .reviews
        .iter()
        .filter(|review| state::sorted(&review.digests) == current)
        .filter_map(|review| {
            let items: Vec<DisputedItem> = review
                .items
                .iter()
                .filter(|item| item.verdict == Verdict::Disputed)
                .map(|item| DisputedItem {
                    target: item.target,
                    about: target(record, item.target),
                    note: item.note.clone(),
                })
                .collect();

            let whole = review.verdict == Verdict::Disputed;

            (whole || !items.is_empty()).then(|| OpenDispute {
                by: review.by.clone(),
                record: whole,
                items,
            })
        })
        .collect()
}

/// For each kind of attestable fact required of a declaration, up to
/// three precedents (§12.3): facts of the kind, from the records of
/// other current declarations, that a review by another party confirms
/// at the current digests. Records whose declarations touch an object
/// this one touches come first, then those of its data model, then the
/// rest, by key; one precedent per record.
fn precedents(derivation: &Derivation, state: &DeclarationState) -> Vec<Precedents> {
    let workspace = derivation.workspace;

    let kinds: BTreeSet<&'static str> = state
        .required
        .iter()
        .filter(|required| required.class == ConformanceClass::Attestable)
        .map(|required| required.fact.kind())
        .collect();

    let objects = touched(workspace, state.declaration.key());
    let models = models(workspace, state.declaration.key());

    kinds
        .into_iter()
        .map(|kind| {
            let mut found: Vec<(u8, Precedent)> = Vec::new();

            for other in &derivation.state.declarations {
                if other.declaration == state.declaration || !other.is_current() {
                    continue;
                }

                let Some(record) = derivation.record(&other.declaration) else {
                    continue;
                };

                let current = state::digests(&other.regions);

                let confirmed = record.facts.iter().enumerate().find(|(index, attested)| {
                    attested.fact.kind() == kind
                        && record.reviews.iter().any(|review| {
                            state::confirms(review, record, ReviewTarget::Fact(*index), &current)
                        })
                });

                let Some((_, attested)) = confirmed else {
                    continue;
                };

                let other_key = other.declaration.key();

                let rank = if !objects.is_disjoint(&touched(workspace, other_key)) {
                    0
                } else if !models.is_disjoint(&self::models(workspace, other_key)) {
                    1
                } else {
                    2
                };

                found.push((
                    rank,
                    Precedent {
                        declaration: other.declaration.clone(),
                        fact: attested.fact.clone(),
                        rendering: attested.fact.to_string(),
                        evidence: attested.evidence.clone(),
                    },
                ));
            }

            found.sort_by(|(a, first), (b, second)| {
                (a, &first.declaration).cmp(&(b, &second.declaration))
            });

            Precedents {
                kind,
                precedents: found
                    .into_iter()
                    .take(3)
                    .map(|(_, precedent)| precedent)
                    .collect(),
            }
        })
        .collect()
}

/// The data objects a declaration touches: those a transaction, or the
/// transactions of a program or operation, read, write, or lock, and a
/// data object itself.
fn touched(workspace: &WorkspaceState, declaration: &DeclarationKey) -> BTreeSet<DeclarationKey> {
    match declaration {
        DeclarationKey::DataObject { .. } => BTreeSet::from([declaration.clone()]),

        DeclarationKey::Transaction { .. }
        | DeclarationKey::Program(_)
        | DeclarationKey::Operation(_) => reach(workspace, declaration).objects,

        _ => BTreeSet::new(),
    }
}

/// The data models a declaration's code belongs to.
fn models(workspace: &WorkspaceState, declaration: &DeclarationKey) -> BTreeSet<Id> {
    let transactions = |operation: &Id| -> BTreeSet<Id> {
        workspace
            .operations
            .get(operation)
            .and_then(|draft| draft.program.as_ref())
            .map(|program| {
                program
                    .transactions()
                    .into_iter()
                    .filter_map(|(_, transaction)| transaction.data_model.clone())
                    .collect()
            })
            .unwrap_or_default()
    };

    match declaration {
        DeclarationKey::Transaction {
            operation,
            transaction,
        } => workspace
            .operations
            .get(operation)
            .and_then(|draft| draft.program.as_ref())
            .and_then(|program| program.transaction(transaction))
            .and_then(|transaction| transaction.data_model.clone())
            .into_iter()
            .collect(),

        DeclarationKey::Program(operation) | DeclarationKey::Operation(operation) => {
            transactions(operation)
        }

        DeclarationKey::DataModel(data_model)
        | DeclarationKey::DataObject { data_model, .. }
        | DeclarationKey::Outbox { data_model, .. } => BTreeSet::from([data_model.clone()]),

        _ => BTreeSet::new(),
    }
}

/// The checklist an item is done by (§12.1).
fn done_when(state: &DeclarationState, record: Option<&Record>) -> Vec<Step> {
    let key = &state.declaration;
    let drifted = state.has(ConditionKind::Drifted);
    let stale = state.has(ConditionKind::Stale);

    // The structural claim is owed only where a relied-on structural
    // fact rests on the declaration; conforming structurally is always
    // the work.
    let structural = state
        .required
        .iter()
        .any(|required| required.class == ConformanceClass::Structural);

    let mut steps = vec![
        Step {
            step: format!(
                "Bind its code with `conseqa:begin {key}` and `conseqa:end {key}` comment \
                 lines."
            ),
            done: !state.regions.is_empty(),
        },
        Step {
            step: if structural {
                "Make its regions implement its residual and do no modeled work beyond it, \
                 and claim structural conformance: a relied-on structural fact rests on it."
                    .to_string()
            } else {
                "Make its regions implement its residual and do no modeled work beyond it."
                    .to_string()
            },
            done: record.is_some_and(|record| record.structural || !structural)
                && !stale
                && !drifted,
        },
    ];

    for required in &state.required {
        if required.class != ConformanceClass::Attestable {
            continue;
        }

        let attested = record.is_some_and(|record| {
            record
                .facts
                .iter()
                .any(|attested| attested.fact.canonical() == required.fact.canonical())
        });

        steps.push(Step {
            step: format!(
                "Evidence `{}` in its regions, or in those of the fact's anchors.",
                required.fact
            ),
            done: attested && !drifted,
        });
    }

    let incomplete: BTreeSet<ClaimScope> = state
        .conditions
        .iter()
        .flat_map(|condition| match condition {
            Condition::Incomplete { scopes } => scopes.iter().map(|scope| scope.scope).collect(),
            _ => Vec::new(),
        })
        .collect();

    for scope in &state.claims {
        steps.push(Step {
            step: format!(
                "Make its `{}` claim hold: every match lies in a declared region or an \
                 excepted file.",
                scope.scope
            ),
            done: !incomplete.contains(&scope.scope),
        });
    }

    steps.push(Step {
        step: "Attest it.".to_string(),
        done: record.is_some()
            && !stale
            && !drifted
            && !state.has(ConditionKind::MissingFacts)
            && !state.has(ConditionKind::Incomplete),
    });

    steps.push(Step {
        step: "Have another party review what the policy requires reviewed.".to_string(),
        done: record.is_some() && !state.has(ConditionKind::Unreviewed),
    });

    steps
}

/// A condition in a sentence.
fn sentence(condition: &Condition, key: &TextualKey, record: Option<&Record>) -> String {
    match condition {
        Condition::Unbound => format!(
            "It has no region, and the policy requires every `{}` bound. Mark its code with \
             `conseqa:begin {key}` and `conseqa:end {key}` comment lines.",
            key.key().kind()
        ),

        Condition::Unattested { set_aside: None } => {
            "It is bound, and has no record. Attest what its regions provide.".to_string()
        }

        Condition::Unattested {
            set_aside: Some(set_aside),
        } => format!(
            "Its record `{}` was set aside: {}. Attest it again.",
            set_aside.record, set_aside.reason
        ),

        Condition::Stale => "The spec changed since its record was attested, as the delta says. \
                             Check its code against the current residual, then attest again."
            .to_string(),

        Condition::Drifted { added, removed } => {
            let paths = |regions: &[RecordedRegion]| {
                regions
                    .iter()
                    .map(|region| {
                        format!("{}:{}-{}", region.path, region.lines[0], region.lines[1])
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            };

            let mut parts = Vec::new();

            if !added.is_empty() {
                parts.push(format!(
                    "{} region{} new or edited ({})",
                    added.len(),
                    plural(added.len()),
                    paths(added)
                ));
            }

            if !removed.is_empty() {
                parts.push(format!(
                    "{} recorded region{} gone or edited ({})",
                    removed.len(),
                    plural(removed.len()),
                    paths(removed)
                ));
            }

            format!(
                "Its code changed since its record was attested: {}. Check it, then attest \
                 again.",
                parts.join("; ")
            )
        }

        Condition::MissingFacts { facts, structural } => {
            let mut parts = Vec::new();

            if !facts.is_empty() {
                parts.push(format!(
                    "Its record does not attest {} required fact{}: {}.",
                    facts.len(),
                    plural(facts.len()),
                    listed(facts)
                ));
            }

            if !structural.is_empty() {
                parts.push(format!(
                    "Its record does not claim structural conformance, which {} relied-on \
                     structural fact{} rest{} on: {}.",
                    structural.len(),
                    plural(structural.len()),
                    if structural.len() == 1 { "s" } else { "" },
                    listed(structural)
                ));
            }

            parts.join(" ")
        }

        Condition::Incomplete { scopes } => scopes
            .iter()
            .map(|incomplete| match &incomplete.failure {
                ScopeFailure::Absent => format!(
                    "Its record carries no `{}` claim, which {} require{}.",
                    incomplete.scope,
                    listed(&incomplete.because),
                    if incomplete.because.len() == 1 {
                        "s"
                    } else {
                        ""
                    }
                ),

                ScopeFailure::Unheld { claims } => claims
                    .iter()
                    .map(|claim| {
                        let total = claim.matches.len() + claim.more;

                        format!(
                            "Its `{}` claim {} has {total} match{} outside the declared \
                             regions and its exceptions, the first at {}:{}-{}.",
                            incomplete.scope,
                            claim.index,
                            if total == 1 { "" } else { "es" },
                            claim.matches[0].path,
                            claim.matches[0].lines[0],
                            claim.matches[0].lines[1]
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(" "),
            })
            .collect::<Vec<_>>()
            .join(" "),

        Condition::Surplus { facts } => format!(
            "Its record attests {} fact{} no proof relies on any longer: {}. That does not mean \
             its code may stop providing {}.",
            facts.len(),
            plural(facts.len()),
            listed(facts),
            if facts.len() == 1 { "it" } else { "them" }
        ),

        Condition::Unreviewed { items } => format!(
            "The policy requires a review of {} thing{} its record carries that no review by \
             another party confirms at the current digests: {}.",
            items.len(),
            plural(items.len()),
            items
                .iter()
                .map(|item| record.map_or_else(|| format!("{item:?}"), |r| target(r, *item)))
                .collect::<Vec<_>>()
                .join("; ")
        ),

        Condition::Disputed { reviews } => format!(
            "{} review{} at the current digests dispute{} it: {}.",
            reviews.len(),
            plural(reviews.len()),
            if reviews.len() == 1 { "s" } else { "" },
            reviews
                .iter()
                .map(|dispute| {
                    let mut disputed = Vec::new();

                    if dispute.record {
                        disputed.push("the record".to_string());
                    }

                    disputed.extend(dispute.items.iter().map(|item| {
                        record.map_or_else(|| format!("{item:?}"), |r| target(r, *item))
                    }));

                    format!("{} on {}", party(&dispute.by), disputed.join(", "))
                })
                .collect::<Vec<_>>()
                .join("; ")
        ),
    }
}

/// An orphaned key in a sentence.
fn orphaned(orphan: &Orphan) -> String {
    let mut named = Vec::new();

    if !orphan.regions.is_empty() {
        named.push(format!(
            "{} region{}",
            orphan.regions.len(),
            plural(orphan.regions.len())
        ));
    }

    if let Some(record) = &orphan.record {
        named.push(format!("its record `{record}`"));
    }

    let singular = matches!(
        (orphan.regions.len(), &orphan.record),
        (1, None) | (0, Some(_))
    );

    format!(
        "The spec declares no `{}`, yet {} name{} it.",
        orphan.key,
        named.join(" and "),
        if singular { "s" } else { "" }
    )
}

/// One thing a record carries, in words.
fn target(record: &Record, target: ReviewTarget) -> String {
    match target {
        ReviewTarget::Fact(index) => match record.facts.get(index) {
            Some(attested) => format!("fact {index}, `{}`", attested.fact),
            None => format!("fact {index}"),
        },

        ReviewTarget::Completeness(index) => match record.completeness.get(index) {
            Some(claim) => format!("claim {index}, `{}`", claim.scope),
            None => format!("claim {index}"),
        },

        ReviewTarget::Structural => "the structural claim".to_string(),
    }
}

fn party(party: &Party) -> String {
    let kind = serde_json::to_value(party.kind)
        .ok()
        .and_then(|kind| kind.as_str().map(str::to_string))
        .unwrap_or_default();

    format!("{kind} {}", party.name)
}

fn listed(facts: &[Fact]) -> String {
    facts
        .iter()
        .map(|fact| format!("`{fact}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

// ---------------------------------------------------------------------
// Semantics
// ---------------------------------------------------------------------

/// The `dsl_guide` topics a packet names, each matching exactly one
/// section of the guide.
mod topic {
    pub const SERVICES: &str = "Services";
    pub const SCHEMAS: &str = "Schemas and field paths";
    pub const DATA_MODELS: &str = "Data models and persistent objects";
    pub const DATA_OBJECT: &str = "`DataObject`";
    pub const OUTBOXES: &str = "Outboxes";
    pub const TOPICS: &str = "Topics as logical channels";
    pub const MESSAGE_IDENTITY: &str = "`Topic.message_identity`";
    pub const STATE_MACHINES: &str = "State machines";
    pub const TRANSITION_SIDE_EFFECTS: &str = "Transition side effects";
    pub const TRANSITION_OUTBOX_EFFECTS: &str = "Transition-scoped outbox effects";
    pub const OPERATIONS: &str = "Operations";
    pub const REQUEST_INPUT: &str = "Request input";
    pub const SUBSCRIPTION_INPUT: &str = "Subscription input";
    pub const OUTBOX_INPUT: &str = "Outbox input";
    pub const INTRINSIC_CONSUMPTION: &str = "Intrinsic consumption semantics";
    pub const PROGRAM: &str = "The operation program and transaction artifacts";
    pub const TRANSACTIONS: &str = "Transactions, replayability, and explicit idempotency";
    pub const ISOLATION: &str = "Isolation";
    pub const TRANSACTION_IDEMPOTENCY: &str = "Transaction idempotency guarantee";
    pub const NATURAL_REPLAY: &str = "Natural replayability";
    pub const ARTIFACT_REPLAY: &str = "Artifact replay after a transaction";
    pub const PROVING_SERIALIZABILITY: &str = "Proving serializability";
    pub const PROVENANCE_ROOTS: &str = "Replay-stable provenance roots";
    pub const MUTATIONS: &str = "Read, update, insert, and delete steps";
    pub const DELETE: &str = "`Delete`";
    pub const ATOMIC_MUTATIONS: &str = "Atomic conditional mutations";
    pub const CURSOR_AND_FENCE: &str = "Cursor and fence steps";
    pub const LOCKS: &str = "Locks";
    pub const TRANSITION_STEP: &str = "`TransactionStep::transition`";
    pub const EFFECT_INTENTS: &str = "14. Effect intents";
    pub const TRANSACTION_OUTPUTS: &str = "Transaction outputs and request results";
    pub const PUBLICATION: &str = "Publication effect";
    pub const REQUEST: &str = "Request effect";
    pub const RETRY: &str = "Retry semantics";
    pub const EXTERNAL: &str = "External effect";
    pub const EXTERNAL_IDEMPOTENCY: &str = "`ExternalIdempotency`";
    pub const OUTBOX_WRITE: &str = "Outbox write effect";
    pub const PROPAGATION: &str = "`IdempotencyKeyPropagation`";
    pub const PATH_ADMISSION: &str = "Paths and path admission";
    pub const TERMINALS: &str = "Asynchronous effects and terminals";
    pub const GROUPING: &str = "Grouping and ordering";
    pub const SUBSCRIPTION_RUNTIME: &str = "`SubscriptionRuntime`";
    pub const OUTBOX_RUNTIME: &str = "`OutboxRuntime`";
    pub const ROUTER: &str = "`Router`";
    pub const EXECUTION_POOL: &str = "`ExecutionPool`";
    pub const STORAGE_LAYOUT: &str = "`StorageLayout`";

    /// Every topic, for the test that each resolves.
    #[cfg(test)]
    pub const ALL: [&str; 46] = [
        SERVICES,
        SCHEMAS,
        DATA_MODELS,
        DATA_OBJECT,
        OUTBOXES,
        TOPICS,
        MESSAGE_IDENTITY,
        STATE_MACHINES,
        TRANSITION_SIDE_EFFECTS,
        TRANSITION_OUTBOX_EFFECTS,
        OPERATIONS,
        REQUEST_INPUT,
        SUBSCRIPTION_INPUT,
        OUTBOX_INPUT,
        INTRINSIC_CONSUMPTION,
        PROGRAM,
        TRANSACTIONS,
        ISOLATION,
        TRANSACTION_IDEMPOTENCY,
        NATURAL_REPLAY,
        ARTIFACT_REPLAY,
        PROVING_SERIALIZABILITY,
        PROVENANCE_ROOTS,
        MUTATIONS,
        DELETE,
        ATOMIC_MUTATIONS,
        CURSOR_AND_FENCE,
        LOCKS,
        TRANSITION_STEP,
        EFFECT_INTENTS,
        TRANSACTION_OUTPUTS,
        PUBLICATION,
        REQUEST,
        RETRY,
        EXTERNAL,
        EXTERNAL_IDEMPOTENCY,
        OUTBOX_WRITE,
        PROPAGATION,
        PATH_ADMISSION,
        TERMINALS,
        GROUPING,
        SUBSCRIPTION_RUNTIME,
        OUTBOX_RUNTIME,
        ROUTER,
        EXECUTION_POOL,
        STORAGE_LAYOUT,
    ];
}

/// The guide topics for the primitives a declaration uses (§12.1): its
/// kind's, those of the constructs its content holds, and those of the
/// facts and claims required of it. Each topic appears once, in that
/// order.
fn topics(
    workspace: &WorkspaceState,
    declaration: &DeclarationKey,
    state: &DeclarationState,
) -> Vec<&'static str> {
    let mut topics: Vec<&'static str> = Vec::new();

    let mut add = |topic: &'static str| {
        if !topics.contains(&topic) {
            topics.push(topic);
        }
    };

    match declaration {
        DeclarationKey::Service(_) => add(topic::SERVICES),
        DeclarationKey::Schema(_) => add(topic::SCHEMAS),
        DeclarationKey::DataModel(_) => add(topic::DATA_MODELS),
        DeclarationKey::DataObject { .. } => add(topic::DATA_OBJECT),
        DeclarationKey::Outbox { .. } => add(topic::OUTBOXES),

        DeclarationKey::Topic(topic) => {
            add(topic::TOPICS);

            if workspace
                .topics
                .get(topic)
                .is_some_and(|topic| matches!(topic.message_identity, MessageIdentity::Keyed(_)))
            {
                add(topic::MESSAGE_IDENTITY);
            }
        }

        DeclarationKey::StateMachine(_) => add(topic::STATE_MACHINES),

        DeclarationKey::Transition {
            machine,
            transition,
        } => {
            add(topic::STATE_MACHINES);

            if let Some(transition) = workspace
                .state_machines
                .get(machine)
                .and_then(|state_machine| state_machine.transitions.get(transition))
            {
                if !transition.side_effects.is_empty() {
                    add(topic::TRANSITION_SIDE_EFFECTS);
                }

                if !transition.effects.is_empty() {
                    add(topic::TRANSITION_OUTBOX_EFFECTS);
                }
            }
        }

        DeclarationKey::Operation(_) => add(topic::OPERATIONS),

        DeclarationKey::Input { operation, input } => {
            match workspace
                .operations
                .get(operation)
                .and_then(|draft| draft.inputs.get(input))
            {
                Some(Input::Request(_)) => add(topic::REQUEST_INPUT),
                Some(Input::Subscription(_)) => add(topic::SUBSCRIPTION_INPUT),
                Some(Input::Outbox(_)) => add(topic::OUTBOX_INPUT),
                None => {}
            }
        }

        DeclarationKey::Program(_) => add(topic::PROGRAM),

        DeclarationKey::Transaction {
            operation,
            transaction,
        } => {
            add(topic::TRANSACTIONS);
            add(topic::ISOLATION);

            let transaction = workspace
                .operations
                .get(operation)
                .and_then(|draft| draft.program.as_ref())
                .and_then(|program| program.transaction(transaction));

            if let Some(transaction) = transaction {
                match &transaction.idempotency {
                    IdempotencyGuarantee::DeduplicatedBy { .. } => {
                        add(topic::TRANSACTION_IDEMPOTENCY)
                    }

                    IdempotencyGuarantee::Unspecified | IdempotencyGuarantee::NotDeduplicated => {
                        add(topic::NATURAL_REPLAY)
                    }
                }

                for step in &transaction.steps {
                    add(match step {
                        TransactionStep::Read(_)
                        | TransactionStep::Update(_)
                        | TransactionStep::Insert(_) => topic::MUTATIONS,
                        TransactionStep::Delete(_) => topic::DELETE,
                        TransactionStep::CompareAndSet(_) | TransactionStep::Upsert(_) => {
                            topic::ATOMIC_MUTATIONS
                        }
                        TransactionStep::Lock(_) => topic::LOCKS,
                        TransactionStep::Transition(_) => topic::TRANSITION_STEP,
                        TransactionStep::AdvanceCursor(_) | TransactionStep::Fence(_) => {
                            topic::CURSOR_AND_FENCE
                        }
                        TransactionStep::EstablishEffectIntent(_) => topic::EFFECT_INTENTS,
                        TransactionStep::EstablishTransactionOutput(_) => {
                            topic::TRANSACTION_OUTPUTS
                        }
                        TransactionStep::WriteOutbox(_) => topic::OUTBOX_WRITE,
                    });
                }
            }
        }

        DeclarationKey::EffectSite { operation, effect } => {
            let program = workspace
                .operations
                .get(operation)
                .and_then(|draft| draft.program.as_ref());

            let declared = program.and_then(|program| {
                program
                    .effect_declarations()
                    .into_iter()
                    .find(|(id, _)| *id == effect)
                    .map(|(_, declared)| declared)
            });

            match declared {
                Some(Effect::Publication(_)) => add(topic::PUBLICATION),
                Some(Effect::Request(_)) => add(topic::REQUEST),
                Some(Effect::External(_)) => add(topic::EXTERNAL),
                Some(Effect::OutboxWrite(_)) => add(topic::OUTBOX_WRITE),

                None => {
                    if program.is_some_and(|program| {
                        program
                            .outbox_write_declarations()
                            .iter()
                            .any(|(_, write)| &write.effect_id == effect)
                    }) {
                        add(topic::OUTBOX_WRITE);
                    }
                }
            }
        }

        DeclarationKey::TopicRuntime(_) => add(topic::GROUPING),
        DeclarationKey::SubscriptionRuntime { .. } => add(topic::SUBSCRIPTION_RUNTIME),
        DeclarationKey::OutboxRuntime { .. } => add(topic::OUTBOX_RUNTIME),
        DeclarationKey::ExecutionPool(_) => add(topic::EXECUTION_POOL),
        DeclarationKey::Router(_) => add(topic::ROUTER),
        DeclarationKey::StorageLayout(_) => add(topic::STORAGE_LAYOUT),

        DeclarationKey::Requirement(_) | DeclarationKey::PromptObligation(_) => {}
    }

    let facts = state
        .required
        .iter()
        .map(|required| &required.fact)
        .chain(state.claims.iter().flat_map(|scope| &scope.because));

    for fact in facts {
        if let Some(topic) = fact_topic(fact) {
            add(topic);
        }
    }

    topics
}

/// The guide topic defining what a fact states.
fn fact_topic(fact: &Fact) -> Option<&'static str> {
    Some(match fact {
        Fact::Isolation { .. } => topic::ISOLATION,
        Fact::Lock { .. } => topic::LOCKS,
        Fact::GuardedMutation { .. } => topic::ATOMIC_MUTATIONS,
        Fact::Cursor { .. } | Fact::Fence { .. } => topic::CURSOR_AND_FENCE,
        Fact::DeduplicatedBy { .. } => topic::TRANSACTION_IDEMPOTENCY,
        Fact::Delivery { .. } => topic::SUBSCRIPTION_RUNTIME,
        Fact::MessageIdentity { .. } => topic::MESSAGE_IDENTITY,
        Fact::OutboxRedrive { .. } => topic::INTRINSIC_CONSUMPTION,
        Fact::RetryMayRepeat { .. } => topic::RETRY,
        Fact::ExternalGuarantee { .. } => topic::EXTERNAL_IDEMPOTENCY,
        Fact::Propagation { .. } => topic::PROPAGATION,
        Fact::NaturallyReplayable { .. } => topic::NATURAL_REPLAY,
        Fact::ArtifactRoute { .. } => topic::ARTIFACT_REPLAY,
        Fact::AdmitsNoMessages { .. } | Fact::NoAdmittedPaths { .. } => topic::PATH_ADMISSION,
        Fact::ReturnsNoResult { .. } => topic::TRANSACTION_OUTPUTS,
        Fact::Terminal { .. } => topic::TERMINALS,
        Fact::StableRoot { .. } => topic::PROVENANCE_ROOTS,
        Fact::ConflictClosure { .. } | Fact::CyclicComponents { .. } => {
            topic::PROVING_SERIALIZABILITY
        }
        Fact::NoDeleters { .. } => topic::DELETE,

        // A realization's guidance is its catalogue entry's, and the
        // internal facts are statements about the spec, not the code.
        Fact::Realization { .. } | Fact::ReliesOn { .. } | Fact::KeyedRequirement { .. } => {
            return None;
        }
    })
}

// ---------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------

impl Packet {
    /// The packet in markdown, for an agent or a person to read: what
    /// the CLI prints, and what the MCP tool puts in front of the JSON
    /// (§12.4).
    pub fn render(&self) -> String {
        let mut out = String::new();

        let _ = writeln!(out, "# {}\n", self.key);

        match self.priority {
            Some(priority) => {
                let name = serde_json::to_value(priority)
                    .ok()
                    .and_then(|name| name.as_str().map(str::to_string))
                    .unwrap_or_default();

                let _ = writeln!(out, "Priority: {name}.\n");
            }

            None => {
                let _ = writeln!(out, "It has no conditions: its code conforms.\n");
            }
        }

        self.render_declaration(&mut out);

        let _ = writeln!(out, "## Conditions\n");

        if self.conditions.is_empty() {
            let _ = writeln!(out, "None.\n");
        } else {
            for statement in &self.conditions {
                let _ = writeln!(
                    out,
                    "- **{}**: {}",
                    statement.condition, statement.statement
                );
            }

            out.push('\n');
        }

        if !self.disputes.is_empty() {
            let _ = writeln!(out, "## Open disputes\n");

            for dispute in &self.disputes {
                if dispute.record {
                    let _ = writeln!(out, "- {} disputes the record.", party(&dispute.by));
                }

                for item in &dispute.items {
                    let note = item
                        .note
                        .as_ref()
                        .map(|note| format!(": {note}"))
                        .unwrap_or_default();

                    let _ = writeln!(
                        out,
                        "- {} disputes {}{note}",
                        party(&dispute.by),
                        item.about
                    );
                }
            }

            out.push('\n');
        }

        if let Some(refusal) = &self.last_refusal {
            let _ = writeln!(out, "## Last refusal\n");
            let _ = writeln!(
                out,
                "An attest by {} was refused: {}\n",
                party(&refusal.by),
                refusal.message
            );
        }

        self.render_required(&mut out);
        self.render_completeness(&mut out);
        self.render_code(&mut out);

        if !self.precedents.is_empty() {
            let _ = writeln!(out, "## Precedents\n");

            for precedents in &self.precedents {
                if precedents.precedents.is_empty() {
                    let _ = writeln!(out, "- {}: none confirmed yet", precedents.kind);
                    continue;
                }

                let _ = writeln!(out, "- {}:", precedents.kind);

                for precedent in &precedents.precedents {
                    let _ = writeln!(
                        out,
                        "  - `{}` in {}, at {}",
                        precedent.rendering,
                        precedent.declaration,
                        located(&precedent.evidence)
                    );
                }
            }

            out.push('\n');
        }

        if !self.semantics.is_empty() {
            let _ = writeln!(out, "## Semantics\n");
            let _ = writeln!(
                out,
                "Read with `dsl_guide`: {}\n",
                self.semantics
                    .iter()
                    .map(|topic| format!("`{topic}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }

        let _ = writeln!(out, "## Done when\n");

        for step in &self.done_when {
            let _ = writeln!(
                out,
                "- [{}] {}",
                if step.done { "x" } else { " " },
                step.step
            );
        }

        out.push('\n');

        let _ = writeln!(out, "## Read set\n");
        let _ = writeln!(
            out,
            "{} declaration{} and {} quer{}, and {}. Send the read set with the attest.",
            self.read_set.declarations.len(),
            plural(self.read_set.declarations.len()),
            self.read_set.queries.len(),
            if self.read_set.queries.len() == 1 {
                "y"
            } else {
                "ies"
            },
            match &self.read_set.record {
                Some(digest) => format!("its record at {digest}"),
                None => "no record".to_string(),
            }
        );

        out
    }

    fn render_declaration(&self, out: &mut String) {
        let section = &self.declaration;

        let _ = writeln!(out, "## Declaration\n");

        match &section.record {
            Some(record) => {
                let _ = writeln!(out, "Record: `{record}`\n");
            }

            None => {
                let _ = writeln!(out, "No record.\n");
            }
        }

        if let Some(residual) = &section.residual {
            let yaml = serde_yaml::to_string(residual).unwrap_or_default();

            let _ = writeln!(out, "```yaml\n{}```\n", yaml);
        }

        if let Some(delta) = &section.delta {
            if delta.is_empty() {
                let _ = writeln!(out, "Unchanged since its record was attested.\n");
            } else {
                let _ = writeln!(out, "Changed since its record was attested:\n");

                let width = delta
                    .iter()
                    .map(|entry| entry.path.chars().count())
                    .max()
                    .unwrap_or(0);

                for entry in delta {
                    let _ = writeln!(out, "    {:width$}  {}", entry.path, delta_entry(entry));
                }

                out.push('\n');
            }
        }
    }

    fn render_required(&self, out: &mut String) {
        let _ = writeln!(out, "## Required facts\n");

        if self.required.is_empty() {
            let _ = writeln!(out, "None.\n");
            return;
        }

        for required in &self.required {
            let class = serde_json::to_value(required.class)
                .ok()
                .and_then(|class| class.as_str().map(str::to_string))
                .unwrap_or_default();

            let _ = writeln!(
                out,
                "- `{}` ({class}, {})",
                required.rendering,
                match (required.class, required.provided) {
                    (ConformanceClass::Structural, true) => "claimed",
                    (ConformanceClass::Structural, false) => "not claimed",
                    (_, true) => "attested",
                    (_, false) => "not attested",
                }
            );

            let _ = writeln!(out, "  relied on by:");

            for relying in &required.obligations {
                let _ = writeln!(out, "  - {}: {}", relying.display, relying.summary);
            }

            match &required.load_bearing {
                LoadBearing::Withdrawal {
                    withdrawal,
                    regressions,
                } => {
                    if regressions.is_empty() {
                        let _ = writeln!(
                            out,
                            "  without it ({withdrawal}): no proven obligation regresses"
                        );
                    } else {
                        let _ = writeln!(out, "  without it ({withdrawal}):");

                        for regression in regressions {
                            let after = regression
                                .after
                                .map(status)
                                .unwrap_or("no longer an obligation");

                            let _ = writeln!(
                                out,
                                "  - {}   {} -> {after}",
                                regression.display,
                                status(regression.before)
                            );
                        }
                    }
                }

                LoadBearing::Invalid { withdrawal, reason } => {
                    let _ = writeln!(
                        out,
                        "  without it ({withdrawal}): the model does not validate: {reason}"
                    );
                }

                LoadBearing::Relied => {
                    let _ = writeln!(
                        out,
                        "  without it: every obligation above loses a fact its proof rests on"
                    );
                }
            }
        }

        out.push('\n');
    }

    fn render_completeness(&self, out: &mut String) {
        if self.completeness.is_empty() {
            return;
        }

        let _ = writeln!(out, "## Completeness\n");

        for claim in &self.completeness {
            let state = match &claim.failure {
                None => "holds".to_string(),
                Some(ScopeFailure::Absent) => "absent".to_string(),

                Some(ScopeFailure::Unheld { claims }) => {
                    let total: usize = claims
                        .iter()
                        .map(|claim| claim.matches.len() + claim.more)
                        .sum();

                    format!(
                        "{total} match{} outside the declared regions",
                        if total == 1 { "" } else { "es" }
                    )
                }
            };

            let _ = writeln!(
                out,
                "- `{}` claim, required by {}: {state}",
                claim.scope,
                listed(&claim.because)
            );

            if let Some(ScopeFailure::Unheld { claims }) = &claim.failure {
                for unheld in claims {
                    for found in &unheld.matches {
                        let _ = writeln!(
                            out,
                            "  - claim {}: {}:{}-{}",
                            unheld.index, found.path, found.lines[0], found.lines[1]
                        );
                    }

                    if unheld.more > 0 {
                        let _ = writeln!(out, "  - claim {}: {} more", unheld.index, unheld.more);
                    }
                }
            }

            let _ = writeln!(
                out,
                "  declared {}:",
                match claim.scope {
                    ClaimScope::Writers => "writers",
                    ClaimScope::Publishers => "publishers",
                }
            );

            listing(out, &claim.declared, "  ");
        }

        out.push('\n');
    }

    fn render_code(&self, out: &mut String) {
        let _ = writeln!(out, "## Code\n");

        if self.code.regions.is_empty() {
            let _ = writeln!(out, "No regions.");
        } else {
            let _ = writeln!(out, "Regions: {}", located(&self.code.regions));
        }

        for relation in &self.code.neighbours {
            let name = serde_json::to_value(relation.relation)
                .ok()
                .and_then(|name| name.as_str().map(str::to_string))
                .unwrap_or_default();

            let _ = writeln!(out, "\n{}{}:", name[..1].to_uppercase(), &name[1..]);

            listing(out, &relation.listing, "");
        }

        out.push('\n');
    }
}

fn listing(out: &mut String, listing: &Listing, indent: &str) {
    for located_declaration in &listing.declarations {
        let regions = if located_declaration.regions.is_empty() {
            "unbound".to_string()
        } else {
            located(&located_declaration.regions)
        };

        let _ = writeln!(
            out,
            "{indent}- {} ({regions})",
            located_declaration.declaration
        );
    }

    if listing.more > 0 {
        let _ = writeln!(out, "{indent}- and {} more", listing.more);
    }
}

fn located(spans: &[Span]) -> String {
    spans
        .iter()
        .map(|span| format!("{}:{}-{}", span.path, span.lines[0], span.lines[1]))
        .collect::<Vec<_>>()
        .join(", ")
}

fn status(status: Status) -> &'static str {
    match status {
        Status::Proven => "proven",
        Status::Unknown => "unknown",
        Status::Disproven => "disproven",
    }
}

#[cfg(test)]
mod tests {
    use super::topic;
    use crate::design::mcp::guide_matches;

    /// Every semantics topic a packet can name resolves through the
    /// `dsl_guide` lookup to exactly one section (§18.8).
    #[test]
    fn every_semantics_topic_names_exactly_one_guide_section() {
        for topic in topic::ALL {
            let matched: Vec<&str> = guide_matches(topic)
                .into_iter()
                .map(|(header, _)| header)
                .collect();

            assert_eq!(
                matched.len(),
                1,
                "`{topic}` names {} sections: {matched:?}",
                matched.len()
            );
        }
    }
}
