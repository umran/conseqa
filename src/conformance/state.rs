//! Conformance state (§10 of the conformance revision): what each
//! declaration's code is known to provide, and whether each proven
//! obligation holds of the code as well as of the model.
//!
//! The state is a pure function of four inputs read at one moment: the
//! spec, the scanned files, the records, and the policy (§13.1). Nothing
//! is derived from history and nothing persists between derivations, so
//! a branch's state is its files' state, and a merge's is the merged
//! files'.
//!
//! What the code must provide is the current proofs' reliance (Part I
//! §13.1). An attestable fact in it is required of each of its
//! implementers. A structural fact is required of each of its anchors
//! that can be bound, which provide it by claiming structural
//! conformance, since it follows from their content (§7). A model-wide
//! fact requires completeness claims (§9). A declaration's conditions
//! compare what is required of it with its regions and its record
//! (§10.1).
//!
//! An obligation can be proven in the model and not conformant in the
//! code, which is the distinction this revision exists to draw (§10.2).
//! A proven obligation is conformant when every fact of its basis holds
//! of the code:
//!
//! - an attestable fact, when every implementer's record attests it and
//!   has not drifted;
//! - a structural fact, when every bindable anchor's record claims
//!   structural conformance and is neither stale nor drifted;
//! - a model-wide fact, when the claims it requires hold;
//! - a relied-on obligation, when it is conformant itself.
//!
//! Reviews and disputes are conditions of their own, gated apart: they
//! say who else checked a record, not what it records.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fmt::Write as _;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::analyzer::basis::{
    Channel, ConformanceClass, DeclarationKey, Fact, Implementers, ObligationKey, TextualKey,
};
use crate::analyzer::report::{Obligation, Status};
use crate::design::change::diff::verified;
use crate::design::change::partition::Partition;
use crate::design::graph::SymbolGraph;
use crate::design::graph_build;
use crate::design::graph_query::{self, GraphQuery, QueryRow};
use crate::design::workspace::WorkspaceState;
use crate::spec::{Id, TransactionStep};

use super::completeness::{self, LISTED_MATCHES, Match, Matcher, RequiredScope};
use super::markers::{self, Region, ScanError};
use super::policy::{Policy, ReviewRule};
use super::record::{
    ClaimScope, Party, PartyKind, Record, RecordContent, RecordFile, RecordedRegion, Records,
    Review, ReviewTarget, Verdict,
};
use super::{ConformanceError, Digest, bindable};

/// The conformance state of a project (§10).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct State {
    /// Every bindable declaration of the spec, in partition order.
    pub declarations: Vec<DeclarationState>,

    /// The keys of regions and records naming no declaration of the
    /// spec: code, or a record, about something the spec no longer
    /// declares. Each is an `orphaned` condition, reported against the
    /// key (§10.1).
    pub orphaned: Vec<Orphan>,

    /// Every proven obligation, by key, with its conformance (§10.2).
    pub obligations: Vec<ObligationConformance>,

    /// The scan errors, by path, then line (§6.6).
    pub scan_errors: Vec<ScanError>,
}

/// One bindable declaration's conformance.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DeclarationState {
    pub declaration: TextualKey,

    /// The regions bound to it now.
    pub regions: Vec<RecordedRegion>,

    /// The facts its code must provide (§7.3), by canonical form: the
    /// attestable facts of the reliance it implements, and the
    /// structural facts of the reliance it anchors, which its structural
    /// claim provides.
    pub required: Vec<RequiredFact>,

    /// The completeness claims its record must carry (§9.4).
    pub claims: Vec<RequiredScope>,

    /// Its conditions, in §10.1's order. A declaration with none is
    /// current.
    pub conditions: Vec<Condition>,
}

impl DeclarationState {
    pub fn is_current(&self) -> bool {
        self.conditions.is_empty()
    }

    pub fn has(&self, kind: ConditionKind) -> bool {
        self.conditions
            .iter()
            .any(|condition| condition.kind() == kind)
    }
}

/// A fact required of a declaration's code, with the obligations whose
/// bases hold it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RequiredFact {
    pub fact: Fact,
    pub class: ConformanceClass,
    pub relied_on_by: Vec<ObligationKey>,
}

/// One derived fact about a declaration's conformance (§10.1).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "condition", rename_all = "snake_case")]
pub enum Condition {
    /// It has no region, and the policy requires its kind bound.
    Unbound,

    /// It is bound, and no readable record exists: none at all, or one
    /// of another version, set aside (§8.6).
    Unattested {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        set_aside: Option<SetAside>,
    },

    /// The record's residual is not the current residual: the code was
    /// checked against content the spec has since changed.
    Stale,

    /// The record's regions are not the current regions: as a multiset
    /// of digests, so editing, adding, or removing a region drifts it,
    /// and moving one does not.
    Drifted {
        /// The current regions no recorded region matches.
        added: Vec<RecordedRegion>,

        /// The recorded regions no current region matches.
        removed: Vec<RecordedRegion>,
    },

    /// Required facts the record does not attest, or the structural
    /// claim that structural facts of the reliance need from it. With
    /// no readable record, nothing is attested.
    MissingFacts {
        facts: Vec<Fact>,

        /// The structural facts its structural claim must provide.
        structural: Vec<Fact>,
    },

    /// A required completeness claim is absent, or does not hold now.
    Incomplete { scopes: Vec<IncompleteScope> },

    /// The record attests facts no proof relies on any longer. That is
    /// not a statement that the code may stop providing them (Part I
    /// §13.2).
    Surplus { facts: Vec<Fact> },

    /// What the policy requires reviewed, and no review by another party
    /// confirms at the current digests.
    Unreviewed { items: Vec<ReviewTarget> },

    /// Reviews at the current digests disputing the record, or items of
    /// it.
    Disputed { reviews: Vec<Dispute> },
}

impl Condition {
    pub fn kind(&self) -> ConditionKind {
        match self {
            Self::Unbound => ConditionKind::Unbound,
            Self::Unattested { .. } => ConditionKind::Unattested,
            Self::Stale => ConditionKind::Stale,
            Self::Drifted { .. } => ConditionKind::Drifted,
            Self::MissingFacts { .. } => ConditionKind::MissingFacts,
            Self::Incomplete { .. } => ConditionKind::Incomplete,
            Self::Surplus { .. } => ConditionKind::Surplus,
            Self::Unreviewed { .. } => ConditionKind::Unreviewed,
            Self::Disputed { .. } => ConditionKind::Disputed,
        }
    }
}

/// The conditions by name, in §10.1's order: what a summary counts, a
/// blocker names, and a gate fails on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConditionKind {
    Unbound,
    Orphaned,
    Unattested,
    Stale,
    Drifted,
    MissingFacts,
    Incomplete,
    Surplus,
    Unreviewed,
    Disputed,
}

impl ConditionKind {
    /// Every condition, in §10.1's order.
    pub const ALL: [Self; 10] = [
        Self::Unbound,
        Self::Orphaned,
        Self::Unattested,
        Self::Stale,
        Self::Drifted,
        Self::MissingFacts,
        Self::Incomplete,
        Self::Surplus,
        Self::Unreviewed,
        Self::Disputed,
    ];
}

impl fmt::Display for ConditionKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Unbound => "unbound",
            Self::Orphaned => "orphaned",
            Self::Unattested => "unattested",
            Self::Stale => "stale",
            Self::Drifted => "drifted",
            Self::MissingFacts => "missing_facts",
            Self::Incomplete => "incomplete",
            Self::Surplus => "surplus",
            Self::Unreviewed => "unreviewed",
            Self::Disputed => "disputed",
        })
    }
}

/// A record set aside unread, and why (§8.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SetAside {
    /// The record's path, relative to the project directory.
    pub record: String,

    /// Both versions, named.
    pub reason: String,
}

/// A required claim scope that is absent or does not hold.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct IncompleteScope {
    pub scope: ClaimScope,

    /// The facts of the reliance requiring it.
    pub because: Vec<Fact>,

    pub failure: ScopeFailure,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScopeFailure {
    /// No readable record carries a claim of the scope.
    Absent,

    /// These claims of the scope have matches outside every accepted
    /// region and excepted file.
    Unheld { claims: Vec<UnheldClaim> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UnheldClaim {
    /// The claim's index in the record's `completeness`.
    pub index: usize,

    /// The first matches outside, by path, then line.
    pub matches: Vec<Match>,

    /// How many more matches lie outside.
    pub more: usize,
}

/// A review at the current digests disputing what a record carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Dispute {
    pub by: Party,

    /// Its verdict on the record as a whole is `disputed`.
    pub record: bool,

    /// The items it disputes.
    pub items: Vec<ReviewTarget>,
}

/// A key naming no declaration of the spec, with the regions and the
/// record that name it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Orphan {
    pub key: TextualKey,
    pub regions: Vec<RecordedRegion>,

    /// The record's path, relative to the project directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record: Option<String>,
}

/// A proven obligation's conformance (§10.2).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ObligationConformance {
    pub obligation: ObligationKey,

    /// The report's one-line statement of the obligation.
    pub summary: String,

    pub conformant: bool,

    /// The declarations whose conditions keep facts of its own basis
    /// from holding of the code.
    pub blockers: Vec<Blocker>,

    /// The obligations it relies on that are not conformant.
    pub through: Vec<ObligationKey>,
}

/// A declaration keeping facts of an obligation's basis from holding of
/// the code, by one of its conditions.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Blocker {
    pub declaration: TextualKey,
    pub condition: ConditionKind,

    /// The facts of the basis the condition involves.
    pub facts: Vec<Fact>,
}

/// Derives the conformance state of the project at `project_dir` from
/// its spec, `workspace`: the store's files for the CLI, the engine's
/// head for the design MCP tools (§15). The derivation fails when the
/// spec does not assemble and validate, since without proofs nothing is
/// required, and when the records or the policy do not load.
pub fn derive(project_dir: &Path, workspace: &WorkspaceState) -> Result<State, ConformanceError> {
    let report = verified(workspace).map_err(|reason| ConformanceError::Unverifiable { reason })?;

    let proven: BTreeMap<&ObligationKey, &Obligation> = report
        .obligations
        .iter()
        .filter(|obligation| obligation.status == Status::Proven)
        .map(|obligation| (&obligation.key, obligation))
        .collect();

    let partition = Partition::of(workspace);
    let graph = graph_build::build(workspace, None);
    let records = Records::of_project(project_dir).read_all()?;
    let policy = Policy::read(project_dir)?;

    let reliance = reliance(proven.values().copied());
    let required = required_facts(&reliance, workspace, &graph);
    let scopes = completeness::required(reliance.values().map(|relied| &relied.fact), workspace);

    // The claims of every required scope, matched as the scan reads the
    // files.
    let mut checked: Vec<(&RequiredScope, usize)> = Vec::new();

    for scope in &scopes {
        let Some(record) = readable(records.get(&scope.carrier)) else {
            continue;
        };

        for (index, claim) in record.completeness.iter().enumerate() {
            if claim.scope == scope.scope {
                checked.push((scope, index));
            }
        }
    }

    let claim = |(scope, index): &(&RequiredScope, usize)| {
        &readable(records.get(&scope.carrier))
            .expect("a checked claim's record is readable")
            .completeness[*index]
    };

    let mut matcher = Matcher::new(checked.iter().map(claim))
        .expect("a record's reader refuses a pattern that does not compile");

    let scan = markers::scan_with(project_dir, |path, text| matcher.visit(path, text))?;

    let mut unheld: BTreeMap<(&TextualKey, ClaimScope), Vec<UnheldClaim>> = BTreeMap::new();

    for (checked, matches) in checked.iter().zip(matcher.into_matches()) {
        let (scope, index) = checked;

        let declared = completeness::declared(scope.scope, scope.carrier.key(), workspace, &graph);

        let accepted: Vec<&Region> = scan
            .regions
            .iter()
            .filter(|region| declared.contains(region.key.key()))
            .collect();

        let outside = completeness::outside(claim(checked), &matches, &accepted);

        if !outside.is_empty() {
            unheld
                .entry((&scope.carrier, scope.scope))
                .or_default()
                .push(UnheldClaim {
                    index: *index,
                    more: outside.len().saturating_sub(LISTED_MATCHES),
                    matches: outside.into_iter().take(LISTED_MATCHES).collect(),
                });
        }
    }

    let failures: BTreeMap<(&TextualKey, ClaimScope), ScopeFailure> = scopes
        .iter()
        .filter_map(|scope| {
            let at = (&scope.carrier, scope.scope);

            let carried = readable(records.get(&scope.carrier)).is_some_and(|record| {
                record
                    .completeness
                    .iter()
                    .any(|claim| claim.scope == scope.scope)
            });

            let failure = if !carried {
                ScopeFailure::Absent
            } else {
                ScopeFailure::Unheld {
                    claims: unheld.remove(&at)?,
                }
            };

            Some((at, failure))
        })
        .collect();

    let inputs = Inputs {
        project_dir,
        partition: &partition,
        records: &records,
        policy: &policy,
        regions: &scan.regions,
        required: &required,
        scopes: &scopes,
        failures: &failures,
    };

    let declarations: Vec<DeclarationState> = partition
        .members
        .keys()
        .filter(|declaration| bindable(declaration))
        .filter_map(|declaration| TextualKey::new(declaration.clone()))
        .map(|declaration| inputs.declaration(declaration))
        .collect();

    let standings: BTreeMap<&TextualKey, Standing> = declarations
        .iter()
        .map(|state| (&state.declaration, Standing::of(state, &records)))
        .collect();

    let obligations = conformance(&proven, workspace, &graph, &standings, &failures);

    Ok(State {
        orphaned: orphans(project_dir, &partition, &scan.regions, &records),
        declarations,
        obligations,
        scan_errors: scan.errors,
    })
}

/// A fact of the reliance, with the union of its anchors and the
/// obligations relying on it.
struct Relied {
    fact: Fact,
    anchors: BTreeSet<DeclarationKey>,
    by: BTreeSet<ObligationKey>,
}

/// The current proofs' reliance (Part I §13.1): every fact a proven
/// obligation's basis holds, by canonical form.
fn reliance<'a>(proven: impl Iterator<Item = &'a Obligation>) -> BTreeMap<String, Relied> {
    let mut reliance: BTreeMap<String, Relied> = BTreeMap::new();

    for obligation in proven {
        for basis_fact in obligation.basis.iter().flatten() {
            let relied = reliance
                .entry(basis_fact.fact.canonical())
                .or_insert_with(|| Relied {
                    fact: basis_fact.fact.clone(),
                    anchors: BTreeSet::new(),
                    by: BTreeSet::new(),
                });

            relied.anchors.extend(basis_fact.anchors.iter().cloned());
            relied.by.insert(obligation.key.clone());
        }
    }

    reliance
}

/// The facts required of each declaration's code (§7.3), by canonical
/// form: each attestable fact of the reliance, of each implementer; each
/// structural fact, of each anchor that can be bound.
fn required_facts(
    reliance: &BTreeMap<String, Relied>,
    workspace: &WorkspaceState,
    graph: &SymbolGraph,
) -> BTreeMap<DeclarationKey, BTreeMap<String, RequiredFact>> {
    let mut required: BTreeMap<DeclarationKey, BTreeMap<String, RequiredFact>> = BTreeMap::new();

    for (canonical, relied) in reliance {
        let class = relied.fact.conformance_class();

        let owners: Vec<DeclarationKey> = match class {
            ConformanceClass::Attestable => implementers(&relied.fact, workspace, graph)
                .into_iter()
                .collect(),

            ConformanceClass::Structural => relied
                .anchors
                .iter()
                .filter(|anchor| bindable(anchor))
                .cloned()
                .collect(),

            ConformanceClass::ModelWide | ConformanceClass::Internal => Vec::new(),
        };

        for owner in owners {
            required.entry(owner).or_default().insert(
                canonical.clone(),
                RequiredFact {
                    fact: relied.fact.clone(),
                    class,
                    relied_on_by: relied.by.iter().cloned().collect(),
                },
            );
        }
    }

    required
}

/// The declarations whose code must provide an attestable fact (§7.2),
/// resolved against the model: none for a fact of another class.
pub fn implementers(
    fact: &Fact,
    workspace: &WorkspaceState,
    graph: &SymbolGraph,
) -> BTreeSet<DeclarationKey> {
    let Some(rule) = fact.implementers() else {
        return BTreeSet::new();
    };

    match rule {
        Implementers::Named(declaration) => BTreeSet::from([declaration]),

        Implementers::CursorAdvancers {
            data_model,
            object,
            field,
        } => stepping(workspace, &data_model, |step| match step {
            TransactionStep::AdvanceCursor(advance) => {
                advance.target.object == object && advance.field == field
            }

            _ => false,
        }),

        Implementers::Fencers {
            data_model,
            object,
            field,
        } => stepping(workspace, &data_model, |step| match step {
            TransactionStep::Fence(fence) => fence.target.object == object && fence.field == field,

            _ => false,
        }),

        Implementers::Producers { channel, schema } => {
            let channel = match channel {
                Channel::Topic { topic } => topic,
                Channel::Outbox { outbox, .. } => outbox,
            };

            graph_query::run(workspace, graph, &GraphQuery::Publishers { topic: channel })
                .rows
                .into_iter()
                .filter_map(|row| match row {
                    QueryRow::Publisher(publisher) if publisher.schema == schema => {
                        Some(completeness::site_declaration(publisher.site))
                    }

                    _ => None,
                })
                .collect()
        }
    }
}

/// The transactions of a data model with a step `matches` accepts.
fn stepping(
    workspace: &WorkspaceState,
    data_model: &Id,
    matches: impl Fn(&TransactionStep) -> bool,
) -> BTreeSet<DeclarationKey> {
    let mut transactions = BTreeSet::new();

    for (operation, draft) in &workspace.operations {
        let Some(program) = &draft.program else {
            continue;
        };

        for (_, transaction) in program.transactions() {
            if transaction.data_model.as_ref() == Some(data_model)
                && transaction.steps.iter().any(&matches)
            {
                transactions.insert(DeclarationKey::Transaction {
                    operation: operation.clone(),
                    transaction: transaction.id.clone(),
                });
            }
        }
    }

    transactions
}

/// The record a file holds, unless it was set aside.
fn readable(file: Option<&RecordFile>) -> Option<&Record> {
    match &file?.content {
        RecordContent::Read(record) => Some(record),
        RecordContent::OtherVersion(_) => None,
    }
}

/// A path below the project directory, as a status names it.
fn relative(project_dir: &Path, path: &Path) -> String {
    let path = path.strip_prefix(project_dir).unwrap_or(path);

    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// Everything a declaration's conditions are derived from.
struct Inputs<'a> {
    project_dir: &'a Path,
    partition: &'a Partition,
    records: &'a BTreeMap<TextualKey, RecordFile>,
    policy: &'a Policy,
    regions: &'a [Region],
    required: &'a BTreeMap<DeclarationKey, BTreeMap<String, RequiredFact>>,
    scopes: &'a [RequiredScope],
    failures: &'a BTreeMap<(&'a TextualKey, ClaimScope), ScopeFailure>,
}

impl Inputs<'_> {
    fn declaration(&self, declaration: TextualKey) -> DeclarationState {
        let key = declaration.key();

        let regions: Vec<RecordedRegion> = self
            .regions
            .iter()
            .filter(|region| region.key.key() == key)
            .map(RecordedRegion::from)
            .collect();

        let required: Vec<RequiredFact> = self
            .required
            .get(key)
            .map(|facts| facts.values().cloned().collect())
            .unwrap_or_default();

        let claims: Vec<RequiredScope> = self
            .scopes
            .iter()
            .filter(|scope| scope.carrier == declaration)
            .cloned()
            .collect();

        let file = self.records.get(&declaration);
        let record = readable(file);

        let mut conditions = Vec::new();

        if regions.is_empty() && self.policy.requires_binding(key.kind()) {
            conditions.push(Condition::Unbound);
        }

        if !regions.is_empty() && record.is_none() {
            conditions.push(Condition::Unattested {
                set_aside: file.and_then(|file| match &file.content {
                    RecordContent::OtherVersion(mismatch) => Some(SetAside {
                        record: relative(self.project_dir, &file.path),
                        reason: mismatch.to_string(),
                    }),

                    RecordContent::Read(_) => None,
                }),
            });
        }

        if let Some(record) = record
            && Some(&record.residual) != self.partition.members.get(key)
        {
            conditions.push(Condition::Stale);
        }

        if let Some(record) = record {
            let (added, removed) = drift(&record.regions, &regions);

            if !added.is_empty() || !removed.is_empty() {
                conditions.push(Condition::Drifted { added, removed });
            }
        }

        let attested: BTreeSet<String> = record
            .map(|record| {
                record
                    .facts
                    .iter()
                    .map(|attested| attested.fact.canonical())
                    .collect()
            })
            .unwrap_or_default();

        let missing: Vec<Fact> = required
            .iter()
            .filter(|required| required.class == ConformanceClass::Attestable)
            .filter(|required| !attested.contains(&required.fact.canonical()))
            .map(|required| required.fact.clone())
            .collect();

        let structural: Vec<Fact> = if record.is_some_and(|record| record.structural) {
            Vec::new()
        } else {
            required
                .iter()
                .filter(|required| required.class == ConformanceClass::Structural)
                .map(|required| required.fact.clone())
                .collect()
        };

        if !missing.is_empty() || !structural.is_empty() {
            conditions.push(Condition::MissingFacts {
                facts: missing,
                structural,
            });
        }

        let incomplete: Vec<IncompleteScope> = claims
            .iter()
            .filter_map(|scope| {
                let failure = self.failures.get(&(&scope.carrier, scope.scope))?;

                Some(IncompleteScope {
                    scope: scope.scope,
                    because: scope.because.clone(),
                    failure: failure.clone(),
                })
            })
            .collect();

        if !incomplete.is_empty() {
            conditions.push(Condition::Incomplete { scopes: incomplete });
        }

        if let Some(record) = record {
            let needed: BTreeSet<String> = required
                .iter()
                .filter(|required| required.class == ConformanceClass::Attestable)
                .map(|required| required.fact.canonical())
                .collect();

            let surplus: Vec<Fact> = record
                .facts
                .iter()
                .filter(|attested| !needed.contains(&attested.fact.canonical()))
                .map(|attested| attested.fact.clone())
                .collect();

            if !surplus.is_empty() {
                conditions.push(Condition::Surplus { facts: surplus });
            }

            let current = digests(&regions);

            let items: Vec<ReviewTarget> = self
                .needs_review(record, &needed, &claims, &required)
                .into_iter()
                .filter(|target| {
                    !record
                        .reviews
                        .iter()
                        .any(|review| confirms(review, record, *target, &current))
                })
                .collect();

            if !items.is_empty() {
                conditions.push(Condition::Unreviewed { items });
            }

            let disputes: Vec<Dispute> = record
                .reviews
                .iter()
                .filter(|review| sorted(&review.digests) == current)
                .filter_map(|review| {
                    let items: Vec<ReviewTarget> = review
                        .items
                        .iter()
                        .filter(|item| item.verdict == Verdict::Disputed)
                        .map(|item| item.target)
                        .collect();

                    let whole = review.verdict == Verdict::Disputed;

                    (whole || !items.is_empty()).then(|| Dispute {
                        by: review.by.clone(),
                        record: whole,
                        items,
                    })
                })
                .collect();

            if !disputes.is_empty() {
                conditions.push(Condition::Disputed { reviews: disputes });
            }
        }

        DeclarationState {
            declaration,
            regions,
            required,
            claims,
            conditions,
        }
    }

    /// What the policy requires reviewed in a record (§14.4): the facts
    /// it attests that a proof relies on, the claims a proof requires,
    /// and its structural claim where a relied-on structural fact rests
    /// on it. A fact or claim no proof needs needs no review, except a
    /// realization fact, which its realization demands.
    fn needs_review(
        &self,
        record: &Record,
        needed: &BTreeSet<String>,
        claims: &[RequiredScope],
        required: &[RequiredFact],
    ) -> Vec<ReviewTarget> {
        let review = &self.policy.review;
        let mut targets = Vec::new();

        for (index, attested) in record.facts.iter().enumerate() {
            let rule = match attested.fact {
                Fact::Realization { .. } => review.realization,
                _ if needed.contains(&attested.fact.canonical()) => review.relied_on_facts,
                _ => ReviewRule::Optional,
            };

            if rule == ReviewRule::Required {
                targets.push(ReviewTarget::Fact(index));
            }
        }

        if review.completeness == ReviewRule::Required {
            for (index, claim) in record.completeness.iter().enumerate() {
                if claims.iter().any(|scope| scope.scope == claim.scope) {
                    targets.push(ReviewTarget::Completeness(index));
                }
            }
        }

        let relied_on = required
            .iter()
            .any(|required| required.class == ConformanceClass::Structural);

        if review.structural == ReviewRule::Required && record.structural && relied_on {
            targets.push(ReviewTarget::Structural);
        }

        targets
    }
}

/// The regions no region of the other side matches by digest, counted
/// as a multiset: the current ones, then the recorded ones.
fn drift(
    recorded: &[RecordedRegion],
    current: &[RecordedRegion],
) -> (Vec<RecordedRegion>, Vec<RecordedRegion>) {
    let mut unmatched: Vec<&RecordedRegion> = recorded.iter().collect();
    let mut added = Vec::new();

    for region in current {
        match unmatched
            .iter()
            .position(|other| other.digest == region.digest)
        {
            Some(position) => {
                unmatched.remove(position);
            }

            None => added.push(region.clone()),
        }
    }

    (added, unmatched.into_iter().cloned().collect())
}

fn digests(regions: &[RecordedRegion]) -> Vec<Digest> {
    sorted(
        &regions
            .iter()
            .map(|region| region.digest)
            .collect::<Vec<_>>(),
    )
}

fn sorted(digests: &[Digest]) -> Vec<Digest> {
    let mut digests = digests.to_vec();

    digests.sort();

    digests
}

/// Whether a review confirms one thing a record carries (§14.3, §14.5):
/// it saw the current digests, it is by another party than the attester,
/// and its verdict on the thing is `confirmed`. An item's verdict
/// overrides the review's verdict on the record as a whole. A checker
/// review satisfies only the realization requirements its checker
/// covers, and no catalogue checker exists yet.
fn confirms(review: &Review, record: &Record, target: ReviewTarget, current: &[Digest]) -> bool {
    let items: Vec<Verdict> = review
        .items
        .iter()
        .filter(|item| item.target == target)
        .map(|item| item.verdict)
        .collect();

    let verdict = if items.is_empty() {
        review.verdict
    } else if items.contains(&Verdict::Disputed) {
        Verdict::Disputed
    } else {
        Verdict::Confirmed
    };

    sorted(&review.digests) == current
        && review.by != record.attested.by
        && review.by.kind != PartyKind::Checker
        && verdict == Verdict::Confirmed
}

/// What an obligation's conformance reads of one declaration.
struct Standing {
    attested: BTreeSet<String>,
    structural: bool,
    stale: bool,
    drifted: bool,
}

impl Standing {
    fn of(state: &DeclarationState, records: &BTreeMap<TextualKey, RecordFile>) -> Self {
        let record = readable(records.get(&state.declaration));

        Self {
            attested: record
                .map(|record| {
                    record
                        .facts
                        .iter()
                        .map(|attested| attested.fact.canonical())
                        .collect()
                })
                .unwrap_or_default(),
            structural: record.is_some_and(|record| record.structural),
            stale: state.has(ConditionKind::Stale),
            drifted: state.has(ConditionKind::Drifted),
        }
    }
}

/// Every proven obligation's conformance (§10.2).
///
/// An obligation's own blockers come from its own basis. Relied-on
/// obligations are read as proofs read them, coinductively: obligations
/// relying on each other in a cycle are conformant unless something in
/// the cycle fails, and failure spreads to every obligation relying on a
/// failing one.
fn conformance(
    proven: &BTreeMap<&ObligationKey, &Obligation>,
    workspace: &WorkspaceState,
    graph: &SymbolGraph,
    standings: &BTreeMap<&TextualKey, Standing>,
    failures: &BTreeMap<(&TextualKey, ClaimScope), ScopeFailure>,
) -> Vec<ObligationConformance> {
    // An implementer or anchor outside the partition has no state: it
    // reads as attesting and claiming nothing.
    let unread = Standing {
        attested: BTreeSet::new(),
        structural: false,
        stale: false,
        drifted: false,
    };

    let mut own: BTreeMap<&ObligationKey, (Vec<Blocker>, BTreeSet<&ObligationKey>)> =
        BTreeMap::new();

    for (key, obligation) in proven {
        let mut blocked: BTreeMap<(TextualKey, ConditionKind), BTreeMap<String, Fact>> =
            BTreeMap::new();
        let mut relies = BTreeSet::new();

        let mut block = |declaration: &DeclarationKey, condition: ConditionKind, fact: &Fact| {
            if let Some(declaration) = TextualKey::new(declaration.clone()) {
                blocked
                    .entry((declaration, condition))
                    .or_default()
                    .insert(fact.canonical(), fact.clone());
            }
        };

        for basis_fact in obligation.basis.iter().flatten() {
            let fact = &basis_fact.fact;

            match fact.conformance_class() {
                ConformanceClass::Attestable => {
                    for implementer in implementers(fact, workspace, graph) {
                        let standing = TextualKey::new(implementer.clone())
                            .and_then(|key| standings.get(&key))
                            .unwrap_or(&unread);

                        if !standing.attested.contains(&fact.canonical()) {
                            block(&implementer, ConditionKind::MissingFacts, fact);
                        } else if standing.drifted {
                            block(&implementer, ConditionKind::Drifted, fact);
                        }
                    }
                }

                ConformanceClass::Structural => {
                    for anchor in basis_fact.anchors.iter().filter(|anchor| bindable(anchor)) {
                        let standing = TextualKey::new(anchor.clone())
                            .and_then(|key| standings.get(&key))
                            .unwrap_or(&unread);

                        if !standing.structural {
                            block(anchor, ConditionKind::MissingFacts, fact);
                            continue;
                        }

                        if standing.stale {
                            block(anchor, ConditionKind::Stale, fact);
                        }

                        if standing.drifted {
                            block(anchor, ConditionKind::Drifted, fact);
                        }
                    }
                }

                ConformanceClass::ModelWide => {
                    for (carrier, scope) in completeness::scopes(fact, workspace) {
                        if failures.contains_key(&(&carrier, scope)) {
                            block(carrier.key(), ConditionKind::Incomplete, fact);
                        }
                    }
                }

                ConformanceClass::Internal => {
                    if let Fact::ReliesOn { obligation } = fact {
                        relies.insert(obligation);
                    }
                }
            }
        }

        let blockers = blocked
            .into_iter()
            .map(|((declaration, condition), facts)| Blocker {
                declaration,
                condition,
                facts: facts.into_values().collect(),
            })
            .collect();

        own.insert(key, (blockers, relies));
    }

    // Failure spreads to every obligation relying on a failing one, or
    // on one that is not proven: what is never left is conformant.
    let mut failing: BTreeSet<&ObligationKey> = own
        .iter()
        .filter(|(_, (blockers, _))| !blockers.is_empty())
        .map(|(key, _)| *key)
        .collect();

    loop {
        let spread: Vec<&ObligationKey> = own
            .iter()
            .filter(|(key, _)| !failing.contains(*key))
            .filter(|(_, (_, relies))| {
                relies
                    .iter()
                    .any(|relied| failing.contains(relied) || !proven.contains_key(relied))
            })
            .map(|(key, _)| *key)
            .collect();

        if spread.is_empty() {
            break;
        }

        failing.extend(spread);
    }

    own.into_iter()
        .map(|(key, (blockers, relies))| ObligationConformance {
            obligation: key.clone(),
            summary: proven[key].summary.clone(),
            conformant: !failing.contains(key),
            blockers,
            through: relies
                .into_iter()
                .filter(|relied| failing.contains(relied) || !proven.contains_key(relied))
                .cloned()
                .collect(),
        })
        .collect()
}

/// The keys of regions and records naming no declaration of the spec.
fn orphans(
    project_dir: &Path,
    partition: &Partition,
    regions: &[Region],
    records: &BTreeMap<TextualKey, RecordFile>,
) -> Vec<Orphan> {
    let mut orphans: BTreeMap<TextualKey, Orphan> = BTreeMap::new();

    for region in regions {
        if !partition.members.contains_key(region.key.key()) {
            orphan(&mut orphans, &region.key)
                .regions
                .push(RecordedRegion::from(region));
        }
    }

    for (key, file) in records {
        if !partition.members.contains_key(key.key()) {
            orphan(&mut orphans, key).record = Some(relative(project_dir, &file.path));
        }
    }

    orphans.into_values().collect()
}

fn orphan<'a>(orphans: &'a mut BTreeMap<TextualKey, Orphan>, key: &TextualKey) -> &'a mut Orphan {
    orphans.entry(key.clone()).or_insert_with(|| Orphan {
        key: key.clone(),
        regions: Vec::new(),
        record: None,
    })
}

impl State {
    /// One declaration's state, if it is a bindable declaration of the
    /// spec.
    pub fn declaration(&self, key: &TextualKey) -> Option<&DeclarationState> {
        self.declarations
            .iter()
            .find(|state| &state.declaration == key)
    }

    /// One proven obligation's conformance.
    pub fn obligation(&self, key: &ObligationKey) -> Option<&ObligationConformance> {
        self.obligations
            .iter()
            .find(|conformance| &conformance.obligation == key)
    }

    /// How many declarations have each condition, and how many keys are
    /// orphaned. A declaration with several conditions counts under
    /// each.
    pub fn counts(&self) -> BTreeMap<ConditionKind, usize> {
        let mut counts = BTreeMap::new();

        for state in &self.declarations {
            for condition in &state.conditions {
                *counts.entry(condition.kind()).or_insert(0) += 1;
            }
        }

        if !self.orphaned.is_empty() {
            counts.insert(ConditionKind::Orphaned, self.orphaned.len());
        }

        counts
    }

    /// The status summary (§10.3): declarations by condition, proven
    /// obligations by conformance, and what blocks each obligation that
    /// is not conformant.
    pub fn summary(&self) -> String {
        let mut out = String::new();
        let counts = self.counts();

        let current = self
            .declarations
            .iter()
            .filter(|state| state.is_current())
            .count();

        let mut tally = vec![format!("{current} current")];

        for kind in ConditionKind::ALL {
            if kind == ConditionKind::Orphaned {
                continue;
            }

            if let Some(count) = counts.get(&kind) {
                tally.push(format!("{count} {kind}"));
            }
        }

        let _ = writeln!(
            out,
            "declarations   {} bindable: {}",
            self.declarations.len(),
            tally.join(", ")
        );

        if !self.orphaned.is_empty() {
            let _ = writeln!(
                out,
                "orphaned       {} key{}: {}",
                self.orphaned.len(),
                if self.orphaned.len() == 1 { "" } else { "s" },
                self.orphaned
                    .iter()
                    .map(|orphan| orphan.key.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }

        if !self.scan_errors.is_empty() {
            let _ = writeln!(out, "scan errors    {}", self.scan_errors.len());

            for error in &self.scan_errors {
                let _ = writeln!(out, "  {error}");
            }
        }

        let conformant = self
            .obligations
            .iter()
            .filter(|conformance| conformance.conformant)
            .count();

        let _ = writeln!(
            out,
            "obligations    {} proven: {conformant} conformant, {} not conformant",
            self.obligations.len(),
            self.obligations.len() - conformant
        );

        for conformance in self.obligations.iter().filter(|c| !c.conformant) {
            let _ = writeln!(out, "  ! {}", conformance.obligation);

            for blocker in &conformance.blockers {
                let paths = match self
                    .declaration(&blocker.declaration)
                    .and_then(|state| drifted_paths(state, blocker.condition))
                {
                    Some(paths) => format!("   {paths}"),
                    None => String::new(),
                };

                let _ = writeln!(
                    out,
                    "      {:<14} {}{paths}",
                    blocker.condition.to_string(),
                    blocker.declaration
                );

                for fact in &blocker.facts {
                    let _ = writeln!(out, "{:21}{fact}", "");
                }
            }

            for relied in &conformance.through {
                let _ = writeln!(out, "      {:<14} {relied}", "through");
            }
        }

        out
    }
}

/// The files of a drifted declaration's changed regions, for a drifted
/// blocker's line.
fn drifted_paths(state: &DeclarationState, condition: ConditionKind) -> Option<String> {
    if condition != ConditionKind::Drifted {
        return None;
    }

    state
        .conditions
        .iter()
        .find_map(|condition| match condition {
            Condition::Drifted { added, removed } => {
                let paths: BTreeSet<&str> = added
                    .iter()
                    .chain(removed)
                    .map(|region| region.path.as_str())
                    .collect();

                Some(paths.into_iter().collect::<Vec<_>>().join(", "))
            }

            _ => None,
        })
}
