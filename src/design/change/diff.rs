//! Declaration changes and the obligation diff (§8 and §11–§14 of the
//! spec change analysis revision): what changed in the architecture,
//! what changed in what it proves, and why.
//!
//! A declaration present only on head is added, one present only on
//! base is removed, and one present on both whose residuals differ is
//! modified, with a structural delta. Ids are identity: a renamed
//! declaration is a removal and an addition, and nothing pairs a
//! removal with an addition by similarity, file name, or any other
//! heuristic (§8.3). Authoring changes are listed apart, and never
//! count as semantic.
//!
//! Obligations are matched by key, never by their positional ids, so
//! reordering requirements changes nothing (§9). A matched obligation
//! proven on both sides is `held` when its basis is the same set of
//! facts and `rebased` when it is not: the verdict stayed, and what the
//! implementation must guarantee for it moved (§11.3). Both sides are
//! analyzed by this one build, and a verdict is a deterministic
//! function of the model, so a verdict that changed while nothing it
//! read from changed is an extraction defect. It is listed as
//! `unexplained`, never dropped and never given a guessed explanation
//! (§12.2).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::ops::Not;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::analyzer::basis::{
    BasisFact, BlockedOn, DeclarationKey, FACT_FORMAT, Fact, FactFormat, Layer, ObligationKey,
    ObligationProperty, ObligationSubject, RequirementFamily, TransactionId,
};
use crate::analyzer::report::{self, Obligation, Status};
use crate::analyzer::verification::ProofScope;
use crate::analyzer::{self, Diagnostic};
use crate::spec::{DSL_VERSION, DslVersion, ValueSource};

use super::delta::{DeltaEntry, delta};
use super::partition::{AuthoringItem, DeclarationLayer, Partition, layer};
use super::side::{Side, SideName, Source};

/// The diff document's format (§14), counted apart from the obligation
/// report's.
pub const FORMAT: u32 = 1;

/// How a declaration or an authoring item changed between the sides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Change {
    Added,
    Removed,
    Modified,
}

/// One changed declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeclarationChange {
    pub declaration: DeclarationKey,
    pub layer: DeclarationLayer,
    pub change: Change,

    /// What moved inside a modified declaration's residual.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub delta: Vec<DeltaEntry>,
}

/// One changed piece of authoring content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthoringChange {
    pub item: AuthoringItem,
    pub change: Change,
}

/// Every change between two partitions, in the order a diff lists them:
/// by layer, then member kind in §7.2's order, then id (§8.4); and the
/// authoring changes apart.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeclarationChanges {
    pub changes: Vec<DeclarationChange>,
    pub authoring: Vec<AuthoringChange>,
}

impl DeclarationChanges {
    /// Whether nothing semantic changed. Authoring changes do not count.
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }
}

/// The changes from `base` to `head`.
pub fn declaration_changes(base: &Partition, head: &Partition) -> DeclarationChanges {
    // `DeclarationKey` orders by member kind in §7.2's order, which is
    // already L0, then L1, then intent.
    let declarations: BTreeSet<&DeclarationKey> =
        base.members.keys().chain(head.members.keys()).collect();

    let changes = declarations
        .into_iter()
        .filter_map(|declaration| {
            let (change, delta) =
                match (base.members.get(declaration), head.members.get(declaration)) {
                    (None, Some(_)) => (Change::Added, Vec::new()),
                    (Some(_), None) => (Change::Removed, Vec::new()),
                    (Some(base), Some(head)) if base != head => {
                        (Change::Modified, delta(base, head))
                    }
                    (Some(_), Some(_)) | (None, None) => return None,
                };

            Some(DeclarationChange {
                declaration: declaration.clone(),
                layer: layer(declaration),
                change,
                delta,
            })
        })
        .collect();

    let items: BTreeSet<&AuthoringItem> =
        base.authoring.keys().chain(head.authoring.keys()).collect();

    let authoring = items
        .into_iter()
        .filter_map(|item| {
            let change = match (base.authoring.get(item), head.authoring.get(item)) {
                (None, Some(_)) => Change::Added,
                (Some(_), None) => Change::Removed,
                (Some(base), Some(head)) if base != head => Change::Modified,
                (Some(_), Some(_)) | (None, None) => return None,
            };

            Some(AuthoringChange {
                item: item.clone(),
                change,
            })
        })
        .collect();

    DeclarationChanges { changes, authoring }
}

// ---------------------------------------------------------------------
// The document
// ---------------------------------------------------------------------

/// The diff document (§14): what `conseqa diff --report` writes and the
/// design MCP tool `diff_spec` returns. The same two trees give a
/// byte-identical document on any machine (§6.6): every list in it has
/// a defined order, and nothing in it records when or where it was
/// made.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiffDocument {
    /// The document's format, counted apart from the obligation
    /// report's.
    pub format: u32,

    /// The DSL contract both sides are read and analyzed under (§6.5).
    pub dsl: DslVersion,

    /// The fact vocabulary the facts in the document are written in.
    pub fact_format: FactFormat,

    pub base: SideRecord,
    pub head: SideRecord,

    pub declarations: DeclarationChanges,
    pub obligations: ObligationDiff,

    /// For every changed declaration, the obligations of either side
    /// whose basis or blocked-on set anchors it (§12.3). Absent when the
    /// obligation diff is unavailable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impact: Option<Vec<Impact>>,

    /// Absent when the obligation diff is unavailable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reliance: Option<RelianceDelta>,
}

/// Where a side was read from, and whether it held a project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SideRecord {
    pub source: SourceRecord,

    /// The side held no project at the project's path (§6.4).
    pub empty: bool,
}

/// A side's source: a commit, with the expression that named it, or
/// the working tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRecord {
    /// The commit read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,

    /// The expression the side was named by.
    #[serde(rename = "ref", default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,

    /// The commit read is the merge base of `ref` and the head
    /// (§15.1), not what `ref` names.
    #[serde(default, skip_serializing_if = "Not::not")]
    pub merge_base: bool,

    #[serde(default, skip_serializing_if = "Not::not")]
    pub working_tree: bool,
}

impl SideRecord {
    fn of(side: &Side) -> Self {
        let source = match &side.source {
            Source::Commit(rev) => SourceRecord {
                commit: side.commit.clone(),
                reference: Some(rev.clone()),
                merge_base: side.merge_base,
                working_tree: false,
            },

            Source::WorkingTree => SourceRecord {
                commit: None,
                reference: None,
                merge_base: false,
                working_tree: true,
            },
        };

        Self {
            source,
            empty: side.is_empty(),
        }
    }
}

/// The obligation diff, or why there is none (§11.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ObligationDiff {
    Available {
        /// How many obligations fell in each class, the quiet ones
        /// included.
        counts: ClassCounts,

        /// Every obligation that changed, or that a changed declaration
        /// touches, in key order. A held obligation, or one unproven on
        /// both sides with the same blocked-on set, is quiet and left
        /// out when it carries no flag and no changed declaration
        /// anchors it.
        entries: Vec<ObligationEntry>,

        /// The removed obligations, grouped by the declaration whose
        /// removal or change took them (§11.4): never silent.
        removed: Vec<RemovedGroup>,

        /// Entries whose verdict changed with nothing they read from
        /// changing: analyzer defects (§12.2).
        unexplained: Vec<UnexplainedEntry>,
    },

    /// A side does not assemble into a model or does not validate, so
    /// neither side's verdicts are compared. The declaration diff is
    /// still produced.
    Unavailable {
        /// The first side that failed: the base when both did.
        side: SideName,

        reason: String,

        /// The other side's obligations by status, when it was
        /// analyzed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        other_side: Option<StatusCounts>,
    },
}

/// The classes of the obligation diff (§11.3). Every obligation of
/// either side is in exactly one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObligationClass {
    /// Its key is on head only.
    Added,

    /// Its key is on base only.
    Removed,

    /// Matched, with head's status below base's.
    Regressed,

    /// Matched, with head's status above base's.
    Improved,

    /// Matched, proven on both sides on the same basis.
    Held,

    /// Matched, proven on both sides on different bases.
    Rebased,

    /// Matched, unknown on both sides or disproven on both.
    Unproven,
}

impl ObligationClass {
    /// Whether an entry of this class must be explained (§12.2): its
    /// status or its basis changed, or, for an unproven one, its
    /// blocked-on set did.
    fn needs_explanation(self, blocked_on_changed: bool) -> bool {
        match self {
            Self::Regressed | Self::Improved | Self::Rebased => true,
            Self::Unproven => blocked_on_changed,
            Self::Added | Self::Removed | Self::Held => false,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClassCounts {
    pub added: usize,
    pub removed: usize,
    pub regressed: usize,
    pub improved: usize,
    pub held: usize,
    pub rebased: usize,
    pub unproven: usize,
}

impl ClassCounts {
    fn count(&mut self, class: ObligationClass) {
        let count = match class {
            ObligationClass::Added => &mut self.added,
            ObligationClass::Removed => &mut self.removed,
            ObligationClass::Regressed => &mut self.regressed,
            ObligationClass::Improved => &mut self.improved,
            ObligationClass::Held => &mut self.held,
            ObligationClass::Rebased => &mut self.rebased,
            ObligationClass::Unproven => &mut self.unproven,
        };

        *count += 1;
    }

    /// The obligations matched across the sides.
    pub fn matched(&self) -> usize {
        self.regressed + self.improved + self.held + self.rebased + self.unproven
    }
}

/// One side's obligations by status.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatusCounts {
    pub proven: usize,
    pub unknown: usize,
    pub disproven: usize,
}

/// One obligation of the diff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObligationEntry {
    pub key: ObligationKey,

    /// The obligation's positional id in base's report, linking the
    /// entry to it (§14).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_id: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_id: Option<String>,

    pub class: ObligationClass,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_status: Option<Status>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_status: Option<Status>,

    /// Whether the requirement's terms differ (§9.3). Present on
    /// matched obligations only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restated: Option<bool>,

    /// How a proof's scope moved, when it did (§11.3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<ScopeMove>,

    /// Whether the blocked-on sets differ. Present on `unproven`
    /// obligations only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_on_changed: Option<bool>,

    /// The facts of base's basis that head's lacks, for a rebased
    /// obligation (§11.5).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub withdrawn: Vec<BasisFact>,

    /// The facts of head's basis that base's lacks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub introduced: Vec<BasisFact>,

    /// The changed declarations explaining the entry (§12.1), or, for a
    /// held obligation, the changed declarations its basis anchors
    /// (§11.6).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub explained_by: Vec<DeclarationKey>,

    /// The obligations this one relies on whose own entries changed
    /// (§10.6): each is explained at its own entry.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub explained_through: Vec<ObligationKey>,
}

/// A proof's scope moving between `l0_only` and `runtime_dependent`
/// (§11.3): because an L1 fact entered or left its basis, or because
/// the scope of an obligation it relies on moved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopeMove {
    pub base: ProofScope,
    pub head: ProofScope,

    /// The relied-on obligations whose own scope moved.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub through: Vec<ObligationKey>,
}

/// The removed obligations one declaration took (§11.4): a removed
/// operation, transaction, or requirement, or the requirement whose
/// change dropped them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemovedGroup {
    pub declaration: DeclarationKey,
    pub obligations: Vec<ObligationKey>,
}

/// An entry the extraction failed to explain, with what each side's
/// verdict rested on, for whoever fixes the extraction (§12.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnexplainedEntry {
    pub key: ObligationKey,
    pub class: ObligationClass,
    pub base: Grounds,
    pub head: Grounds,
}

/// One side's basis, or its blocked-on set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grounds {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub basis: Option<Vec<BasisFact>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_on: Option<BlockedOn>,
}

/// What happened to the obligations that rest on one changed
/// declaration (§12.3): the view a reviewer of one changed file wants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Impact {
    pub declaration: DeclarationKey,
    pub obligations: Vec<ImpactedObligation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImpactedObligation {
    pub key: ObligationKey,
    pub class: ObligationClass,
}

/// The change in what the implementation must guarantee for the proofs
/// to hold of it (§13): facts relied on by some proven obligation on
/// one side and by none on the other. A fact relied on by both sides is
/// left to the per-obligation entries, whatever its counts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelianceDelta {
    pub introduced: Vec<IntroducedReliance>,

    /// No proof needs these any longer. That is not a statement that
    /// the implementation may stop providing them (§13.2).
    pub withdrawn: Vec<WithdrawnReliance>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntroducedReliance {
    pub fact: Fact,
    pub layer: Layer,

    /// How many proven obligations on head rely on it.
    pub relied_on_by: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WithdrawnReliance {
    pub fact: Fact,
    pub layer: Layer,
}

/// A class of change `conseqa diff --fail-on` fails the run on (§15.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Gate {
    /// Any obligation regressed.
    Regressed,

    /// Any obligation was removed, whatever its base status.
    Removed,

    /// Any added obligation is not proven.
    AddedUnproven,

    /// Any obligation was rebased.
    Rebased,

    /// Any entry is unexplained.
    Unexplained,
}

impl fmt::Display for Gate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Regressed => "regressed",
            Self::Removed => "removed",
            Self::AddedUnproven => "added-unproven",
            Self::Rebased => "rebased",
            Self::Unexplained => "unexplained",
        })
    }
}

impl DiffDocument {
    /// The gates of `gates` that fail on this diff, in the order given
    /// (§15.3). A gate that cannot be evaluated fails: while the
    /// obligation diff is unavailable, every gate does.
    pub fn failed_gates(&self, gates: &[Gate]) -> Vec<Gate> {
        let mut failed: Vec<Gate> = Vec::new();

        for gate in gates {
            if failed.contains(gate) {
                continue;
            }

            let fails = match &self.obligations {
                ObligationDiff::Unavailable { .. } => true,

                ObligationDiff::Available {
                    counts,
                    entries,
                    unexplained,
                    ..
                } => match gate {
                    Gate::Regressed => counts.regressed > 0,
                    Gate::Removed => counts.removed > 0,

                    Gate::AddedUnproven => entries.iter().any(|entry| {
                        entry.class == ObligationClass::Added
                            && entry.head_status != Some(Status::Proven)
                    }),

                    Gate::Rebased => counts.rebased > 0,
                    Gate::Unexplained => !unexplained.is_empty(),
                },
            };

            if fails {
                failed.push(*gate);
            }
        }

        failed
    }
}

// ---------------------------------------------------------------------
// Diffing
// ---------------------------------------------------------------------

/// The diff of two sides (§6–§14).
pub fn diff(base: &Side, head: &Side) -> DiffDocument {
    let base_partition = base
        .workspace
        .as_ref()
        .map(Partition::of)
        .unwrap_or_default();

    let head_partition = head
        .workspace
        .as_ref()
        .map(Partition::of)
        .unwrap_or_default();

    let declarations = declaration_changes(&base_partition, &head_partition);

    let (obligations, impact, reliance) = match (analyze(base), analyze(head)) {
        (Ok(base_analysis), Ok(head_analysis)) => {
            let sides = Sides {
                base: &base_analysis,
                head: &head_analysis,
                base_partition: &base_partition,
                head_partition: &head_partition,
                changed: declarations
                    .changes
                    .iter()
                    .map(|change| (change.declaration.clone(), change.change))
                    .collect(),
            };

            let (obligations, classes) = sides.obligation_diff();
            let impact = sides.impact(&declarations, &classes);
            let reliance = sides.reliance();

            (obligations, Some(impact), Some(reliance))
        }

        (Err(reason), other) => (
            ObligationDiff::Unavailable {
                side: SideName::Base,
                reason,
                other_side: other.ok().map(|analysis| analysis.counts()),
            },
            None,
            None,
        ),

        (Ok(base_analysis), Err(reason)) => (
            ObligationDiff::Unavailable {
                side: SideName::Head,
                reason,
                other_side: Some(base_analysis.counts()),
            },
            None,
            None,
        ),
    };

    DiffDocument {
        format: FORMAT,
        dsl: DSL_VERSION,
        fact_format: FACT_FORMAT,
        base: SideRecord::of(base),
        head: SideRecord::of(head),
        declarations,
        obligations,
        impact,
        reliance,
    }
}

/// One side's obligations, by key.
#[derive(Debug, Default)]
struct Analysis {
    obligations: BTreeMap<ObligationKey, Obligation>,
}

impl Analysis {
    fn counts(&self) -> StatusCounts {
        let mut counts = StatusCounts::default();

        for obligation in self.obligations.values() {
            match obligation.status {
                Status::Proven => counts.proven += 1,
                Status::Unknown => counts.unknown += 1,
                Status::Disproven => counts.disproven += 1,
            }
        }

        counts
    }
}

/// Analyzes a side under `conseqa check`'s preconditions (§11.1): it
/// must assemble into a model, and the model must validate. The empty
/// project has no obligations.
fn analyze(side: &Side) -> Result<Analysis, String> {
    let Some(workspace) = &side.workspace else {
        return Ok(Analysis::default());
    };

    let model = workspace
        .assemble_model()
        .map_err(|error| error.to_string())?;

    let errors = analyzer::validate(&model);

    if let Some(first) = errors.first() {
        let first = Diagnostic::from(first.clone());

        let subject = first
            .subject
            .as_ref()
            .map(|subject| format!("[{subject}] "))
            .unwrap_or_default();

        return Err(format!(
            "the model is invalid: {} validation error{}, the first: {subject}{}",
            errors.len(),
            if errors.len() == 1 { "" } else { "s" },
            first.message
        ));
    }

    let verification = analyzer::verification::verify(&model);
    let report = report::obligations(&model, &verification);

    Ok(Analysis {
        obligations: report
            .obligations
            .into_iter()
            .map(|obligation| (obligation.key.clone(), obligation))
            .collect(),
    })
}

/// Both sides, analyzed, with what changed between them.
struct Sides<'a> {
    base: &'a Analysis,
    head: &'a Analysis,
    base_partition: &'a Partition,
    head_partition: &'a Partition,
    changed: BTreeMap<DeclarationKey, Change>,
}

impl Sides<'_> {
    /// The obligation diff, and the class of every obligation of either
    /// side.
    fn obligation_diff(&self) -> (ObligationDiff, BTreeMap<ObligationKey, ObligationClass>) {
        let keys: BTreeSet<&ObligationKey> = self
            .base
            .obligations
            .keys()
            .chain(self.head.obligations.keys())
            .collect();

        let mut counts = ClassCounts::default();
        let mut classes = BTreeMap::new();
        let mut all = BTreeMap::new();
        let mut removed: BTreeMap<DeclarationKey, Vec<ObligationKey>> = BTreeMap::new();

        for key in keys {
            let base = self.base.obligations.get(key);
            let head = self.head.obligations.get(key);

            let entry = match (base, head) {
                (Some(base), Some(head)) => self.matched(key, base, head),
                (None, Some(head)) => self.unmatched(key, None, Some(head)),
                (Some(base), None) => self.unmatched(key, Some(base), None),
                (None, None) => continue,
            };

            counts.count(entry.class);
            classes.insert(key.clone(), entry.class);

            if entry.class == ObligationClass::Removed {
                removed
                    .entry(self.removed_by(key))
                    .or_default()
                    .push(key.clone());
            }

            all.insert(key.clone(), entry);
        }

        self.explain_through(&mut all);

        let unexplained = unexplained(&all)
            .into_iter()
            .map(|key| UnexplainedEntry {
                key: key.clone(),
                class: all[key].class,
                base: grounds(&self.base.obligations[key]),
                head: grounds(&self.head.obligations[key]),
            })
            .collect();

        let entries = all.into_values().filter(|entry| !quiet(entry)).collect();

        let removed = removed
            .into_iter()
            .map(|(declaration, obligations)| RemovedGroup {
                declaration,
                obligations,
            })
            .collect();

        (
            ObligationDiff::Available {
                counts,
                entries,
                removed,
                unexplained,
            },
            classes,
        )
    }

    /// Names, on every entry that must be explained, the obligations it
    /// relies on whose own entry changed (§10.6): a change to a
    /// relied-on obligation is explained at its own entry, and the
    /// dependent entry names it rather than inlining its basis.
    fn explain_through(&self, entries: &mut BTreeMap<ObligationKey, ObligationEntry>) {
        let changed: BTreeSet<ObligationKey> = entries
            .values()
            .filter(|entry| changed(entry))
            .map(|entry| entry.key.clone())
            .collect();

        for entry in entries.values_mut() {
            if !must_explain(entry) {
                continue;
            }

            let relied_on: BTreeSet<&ObligationKey> = [&self.base, &self.head]
                .into_iter()
                .filter_map(|analysis| analysis.obligations.get(&entry.key))
                .flat_map(relied_on)
                .collect();

            entry.explained_through = relied_on
                .into_iter()
                .filter(|key| **key != entry.key && changed.contains(*key))
                .cloned()
                .collect();
        }
    }

    /// An obligation on both sides.
    fn matched(
        &self,
        key: &ObligationKey,
        base: &Obligation,
        head: &Obligation,
    ) -> ObligationEntry {
        let class = match rank(base.status).cmp(&rank(head.status)) {
            std::cmp::Ordering::Greater => ObligationClass::Regressed,
            std::cmp::Ordering::Less => ObligationClass::Improved,

            std::cmp::Ordering::Equal if base.status == Status::Proven => {
                if same_facts(
                    base.basis.as_deref().unwrap_or_default(),
                    head.basis.as_deref().unwrap_or_default(),
                ) {
                    ObligationClass::Held
                } else {
                    ObligationClass::Rebased
                }
            }

            std::cmp::Ordering::Equal => ObligationClass::Unproven,
        };

        let (withdrawn, introduced) = match class {
            ObligationClass::Rebased => (
                difference(
                    base.basis.as_deref().unwrap_or_default(),
                    head.basis.as_deref().unwrap_or_default(),
                ),
                difference(
                    head.basis.as_deref().unwrap_or_default(),
                    base.basis.as_deref().unwrap_or_default(),
                ),
            ),

            ObligationClass::Added
            | ObligationClass::Removed
            | ObligationClass::Regressed
            | ObligationClass::Improved
            | ObligationClass::Held
            | ObligationClass::Unproven => (Vec::new(), Vec::new()),
        };

        let blocked_on_changed = (class == ObligationClass::Unproven)
            .then(|| !same_blocked_on(base.blocked_on.as_ref(), head.blocked_on.as_ref()));

        let mut read: BTreeSet<DeclarationKey> = read_from(base).chain(read_from(head)).collect();

        // An absence query holding on one side is falsified on the
        // other by what that side declares: read both.
        for fact in facts(base).chain(facts(head)) {
            read.extend(absence_members(&fact.fact, self.base_partition));
            read.extend(absence_members(&fact.fact, self.head_partition));
        }

        ObligationEntry {
            key: key.clone(),
            base_id: Some(base.id.clone()),
            head_id: Some(head.id.clone()),
            class,
            base_status: Some(base.status),
            head_status: Some(head.status),
            restated: Some(terms(self.base_partition, key) != terms(self.head_partition, key)),
            scope: self.scope_move(base, head),
            blocked_on_changed,
            withdrawn,
            introduced,
            explained_by: read
                .into_iter()
                .filter(|declaration| self.changed.contains_key(declaration))
                .collect(),
            explained_through: Vec::new(),
        }
    }

    /// An obligation on one side only, explained by the requirement
    /// that produced it, when that changed in any way, and by its
    /// subject declarations added or removed with it (§12.1).
    fn unmatched(
        &self,
        key: &ObligationKey,
        base: Option<&Obligation>,
        head: Option<&Obligation>,
    ) -> ObligationEntry {
        let (class, with) = match head {
            Some(_) => (ObligationClass::Added, Change::Added),
            None => (ObligationClass::Removed, Change::Removed),
        };

        let requirement = DeclarationKey::Requirement(key.requirement());

        let explained_by = subject_declarations(&key.subject)
            .into_iter()
            .filter(|declaration| self.changed.get(declaration) == Some(&with))
            .chain(
                self.changed
                    .contains_key(&requirement)
                    .then_some(requirement),
            )
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();

        ObligationEntry {
            key: key.clone(),
            base_id: base.map(|obligation| obligation.id.clone()),
            head_id: head.map(|obligation| obligation.id.clone()),
            class,
            base_status: base.map(|obligation| obligation.status),
            head_status: head.map(|obligation| obligation.status),
            restated: None,
            scope: None,
            blocked_on_changed: None,
            withdrawn: Vec::new(),
            introduced: Vec::new(),
            explained_by,
            explained_through: Vec::new(),
        }
    }

    /// The declaration a removed obligation is listed under (§11.4): its
    /// outermost removed subject, or else its requirement.
    fn removed_by(&self, key: &ObligationKey) -> DeclarationKey {
        subject_declarations(&key.subject)
            .into_iter()
            .find(|declaration| self.changed.get(declaration) == Some(&Change::Removed))
            .unwrap_or_else(|| DeclarationKey::Requirement(key.requirement()))
    }

    /// How a matched proof's scope moved, naming the relied-on
    /// obligations whose own scope moved.
    fn scope_move(&self, base: &Obligation, head: &Obligation) -> Option<ScopeMove> {
        let (Some(base_scope), Some(head_scope)) = (base.scope, head.scope) else {
            return None;
        };

        if base_scope == head_scope {
            return None;
        }

        let relied_on: BTreeSet<&ObligationKey> = base
            .basis
            .iter()
            .chain(head.basis.iter())
            .flatten()
            .filter_map(|fact| match &fact.fact {
                Fact::ReliesOn { obligation } => Some(obligation),
                _ => None,
            })
            .collect();

        let through = relied_on
            .into_iter()
            .filter(|key| {
                let scope = |analysis: &Analysis| {
                    analysis
                        .obligations
                        .get(*key)
                        .and_then(|obligation| obligation.scope)
                };

                matches!(
                    (scope(self.base), scope(self.head)),
                    (Some(base), Some(head)) if base != head
                )
            })
            .cloned()
            .collect();

        Some(ScopeMove {
            base: base_scope,
            head: head_scope,
            through,
        })
    }

    /// For every changed declaration, the obligations of either side
    /// whose basis or blocked-on set anchors it, each with its class
    /// (§12.3).
    fn impact(
        &self,
        declarations: &DeclarationChanges,
        classes: &BTreeMap<ObligationKey, ObligationClass>,
    ) -> Vec<Impact> {
        let mut anchoring: BTreeMap<&DeclarationKey, BTreeSet<&ObligationKey>> = BTreeMap::new();

        for obligation in self
            .base
            .obligations
            .values()
            .chain(self.head.obligations.values())
        {
            for anchor in anchors(obligation) {
                if let Some((declaration, _)) = self.changed.get_key_value(&anchor) {
                    anchoring
                        .entry(declaration)
                        .or_default()
                        .insert(&obligation.key);
                }
            }
        }

        declarations
            .changes
            .iter()
            .map(|change| Impact {
                declaration: change.declaration.clone(),
                obligations: anchoring
                    .get(&change.declaration)
                    .into_iter()
                    .flatten()
                    .map(|key| ImpactedObligation {
                        key: (*key).clone(),
                        class: classes[*key],
                    })
                    .collect(),
            })
            .collect()
    }

    /// The facts relied on by some proven obligation on one side and by
    /// none on the other (§13.1).
    fn reliance(&self) -> RelianceDelta {
        let base = reliance(self.base);
        let head = reliance(self.head);

        RelianceDelta {
            introduced: head
                .iter()
                .filter(|(text, _)| !base.contains_key(*text))
                .map(|(_, (fact, count))| IntroducedReliance {
                    fact: fact.clone(),
                    layer: fact.layer(),
                    relied_on_by: *count,
                })
                .collect(),

            withdrawn: base
                .iter()
                .filter(|(text, _)| !head.contains_key(*text))
                .map(|(_, (fact, _))| WithdrawnReliance {
                    fact: fact.clone(),
                    layer: fact.layer(),
                })
                .collect(),
        }
    }
}

/// Whether an entry must be explained (§12.2).
fn must_explain(entry: &ObligationEntry) -> bool {
    entry
        .class
        .needs_explanation(entry.blocked_on_changed == Some(true))
}

/// Whether an entry's obligation changed: added, removed, or changed in
/// status, basis, or blocked-on set.
fn changed(entry: &ObligationEntry) -> bool {
    matches!(
        entry.class,
        ObligationClass::Added | ObligationClass::Removed
    ) || must_explain(entry)
}

/// The entries that must be explained and are not (§12.2). An entry is
/// explained by a changed declaration it read, or through a relied-on
/// obligation whose own change is explained; an added or removed
/// obligation is explained by its requirement. Explanation is
/// well-founded: obligations that changed only through each other, with
/// no changed declaration under any of them, stay unexplained.
fn unexplained(entries: &BTreeMap<ObligationKey, ObligationEntry>) -> Vec<&ObligationKey> {
    let mut explained: BTreeSet<&ObligationKey> = entries
        .values()
        .filter(|entry| {
            matches!(
                entry.class,
                ObligationClass::Added | ObligationClass::Removed
            ) || !entry.explained_by.is_empty()
        })
        .map(|entry| &entry.key)
        .collect();

    loop {
        let through: Vec<&ObligationKey> = entries
            .values()
            .filter(|entry| {
                !explained.contains(&entry.key)
                    && entry
                        .explained_through
                        .iter()
                        .any(|key| explained.contains(key))
            })
            .map(|entry| &entry.key)
            .collect();

        if through.is_empty() {
            break;
        }

        explained.extend(through);
    }

    entries
        .values()
        .filter(|entry| must_explain(entry) && !explained.contains(&entry.key))
        .map(|entry| &entry.key)
        .collect()
}

/// The obligations an obligation's basis or blocked-on set relies on
/// (§10.6).
fn relied_on(obligation: &Obligation) -> impl Iterator<Item = &ObligationKey> {
    facts(obligation).filter_map(|fact| match &fact.fact {
        Fact::ReliesOn { obligation } => Some(obligation),
        _ => None,
    })
}

/// Whether an entry is quiet, and left out of the listing: held, or
/// unproven with the same blocked-on set, with no flag and no changed
/// declaration among what it read.
fn quiet(entry: &ObligationEntry) -> bool {
    let unchanged = match entry.class {
        ObligationClass::Held => true,
        ObligationClass::Unproven => entry.blocked_on_changed == Some(false),

        ObligationClass::Added
        | ObligationClass::Removed
        | ObligationClass::Regressed
        | ObligationClass::Improved
        | ObligationClass::Rebased => false,
    };

    unchanged
        && entry.restated != Some(true)
        && entry.scope.is_none()
        && entry.explained_by.is_empty()
}

/// Statuses in their order (§11.2): proven above unknown above
/// disproven.
fn rank(status: Status) -> u8 {
    match status {
        Status::Proven => 2,
        Status::Unknown => 1,
        Status::Disproven => 0,
    }
}

/// Whether two lists hold the same facts. Anchors take no part in a
/// fact's identity (§10.2).
fn same_facts(base: &[BasisFact], head: &[BasisFact]) -> bool {
    canonical(base) == canonical(head)
}

fn canonical(facts: &[BasisFact]) -> BTreeSet<String> {
    facts.iter().map(|fact| fact.fact.canonical()).collect()
}

/// The facts of `from` that `other` lacks, with `from`'s anchors.
fn difference(from: &[BasisFact], other: &[BasisFact]) -> Vec<BasisFact> {
    let other = canonical(other);

    from.iter()
        .filter(|fact| !other.contains(&fact.fact.canonical()))
        .cloned()
        .collect()
}

/// Whether two blocked-on sets are the same: the same present facts,
/// the same missing facts, and the same declarations read.
fn same_blocked_on(base: Option<&BlockedOn>, head: Option<&BlockedOn>) -> bool {
    let empty = BlockedOn::default();
    let (base, head) = (base.unwrap_or(&empty), head.unwrap_or(&empty));

    same_facts(&base.present, &head.present)
        && same_facts(&base.missing, &head.missing)
        && base.anchors == head.anchors
}

/// A requirement's terms (§9.3): everything of its residual but its
/// result, which belongs to the result replay obligation, not to the
/// one restated.
fn terms(partition: &Partition, key: &ObligationKey) -> Option<Value> {
    let mut residual = partition
        .members
        .get(&DeclarationKey::Requirement(key.requirement()))?
        .clone();

    if let Value::Object(fields) = &mut residual {
        fields.remove("result");
    }

    Some(residual)
}

/// The operation, and the transaction for a transaction subject: the
/// declarations an obligation is added or removed with.
fn subject_declarations(subject: &ObligationSubject) -> Vec<DeclarationKey> {
    match subject {
        ObligationSubject::Operation { operation } => {
            vec![DeclarationKey::Operation(operation.clone())]
        }

        ObligationSubject::Transaction {
            operation,
            transaction,
        } => vec![
            DeclarationKey::Operation(operation.clone()),
            DeclarationKey::Transaction {
                operation: operation.clone(),
                transaction: transaction.clone(),
            },
        ],
    }
}

/// Every declaration an obligation's verdict read from: its facts'
/// anchors, its obstacles' anchors, and the members of its query
/// results (§12.1).
fn read_from(obligation: &Obligation) -> impl Iterator<Item = DeclarationKey> + '_ {
    let members = facts(obligation).flat_map(|fact| query_members(&fact.fact));

    anchors(obligation).into_iter().chain(members)
}

/// The declarations an obligation's basis or blocked-on set anchors.
fn anchors(obligation: &Obligation) -> BTreeSet<DeclarationKey> {
    let mut anchors: BTreeSet<DeclarationKey> = facts(obligation)
        .flat_map(|fact| fact.anchors.iter().cloned())
        .collect();

    if let Some(blocked_on) = &obligation.blocked_on {
        anchors.extend(blocked_on.anchors.iter().cloned());
    }

    anchors
}

/// Every fact an obligation's basis or blocked-on set holds.
fn facts(obligation: &Obligation) -> impl Iterator<Item = &BasisFact> {
    let blocked_on = obligation
        .blocked_on
        .iter()
        .flat_map(|blocked_on| blocked_on.present.iter().chain(&blocked_on.missing));

    obligation.basis.iter().flatten().chain(blocked_on)
}

/// The members of a query fact's result.
fn query_members(fact: &Fact) -> Vec<DeclarationKey> {
    let transaction = |member: &TransactionId| DeclarationKey::Transaction {
        operation: member.operation.clone(),
        transaction: member.transaction.clone(),
    };

    match fact {
        Fact::ConflictClosure { root, members, .. } => std::iter::once(root)
            .chain(members)
            .map(transaction)
            .collect(),

        Fact::CyclicComponents {
            root, components, ..
        } => std::iter::once(root)
            .chain(components.iter().flatten())
            .map(transaction)
            .collect(),

        // The absence queries hold of an empty result; what falsifies
        // one is read from both sides by `absence_members`.
        Fact::NoDeleters { .. }
        | Fact::KeyedRequirement { .. }
        | Fact::Isolation { .. }
        | Fact::Lock { .. }
        | Fact::GuardedMutation { .. }
        | Fact::Cursor { .. }
        | Fact::Fence { .. }
        | Fact::AdmitsNoMessages { .. }
        | Fact::NoAdmittedPaths { .. }
        | Fact::ReturnsNoResult { .. }
        | Fact::Terminal { .. }
        | Fact::StableRoot { .. }
        | Fact::Delivery { .. }
        | Fact::MessageIdentity { .. }
        | Fact::DeduplicatedBy { .. }
        | Fact::NaturallyReplayable { .. }
        | Fact::ArtifactRoute { .. }
        | Fact::OutboxRedrive { .. }
        | Fact::RetryMayRepeat { .. }
        | Fact::ExternalGuarantee { .. }
        | Fact::Propagation { .. }
        | Fact::ReliesOn { .. }
        | Fact::Realization { .. } => Vec::new(),
    }
}

/// The members of an absence query's result on one side: the
/// declarations whose presence falsifies it there (§12.1). A deleter
/// falsifies `no_deleters`; a requirement of the property keyed from
/// the input falsifies the absence `keyed_requirement` was found
/// missing for. Every other fact has none.
fn absence_members(fact: &Fact, partition: &Partition) -> Vec<DeclarationKey> {
    match fact {
        Fact::NoDeleters { data_model, object } => partition
            .members
            .iter()
            .filter(|(declaration, residual)| {
                matches!(declaration, DeclarationKey::Transaction { .. })
                    && residual["data_model"] == data_model.0.as_str()
                    && residual["steps"].as_array().is_some_and(|steps| {
                        steps.iter().any(|step| {
                            step["kind"] == "delete"
                                && step["target"]["object"] == object.0.as_str()
                        })
                    })
            })
            .map(|(declaration, _)| declaration.clone())
            .collect(),

        Fact::KeyedRequirement {
            operation,
            input,
            property,
        } => partition
            .members
            .iter()
            .filter(|(declaration, residual)| {
                let DeclarationKey::Requirement(identity) = declaration else {
                    return false;
                };

                let keyed = identity.subject
                    == (ObligationSubject::Operation {
                        operation: operation.clone(),
                    })
                    && identity.family == RequirementFamily::Idempotency
                    && identity
                        .requirement_key
                        .first()
                        .is_some_and(|root| root.source == ValueSource::Input(input.clone()));

                keyed
                    && match property {
                        ObligationProperty::ResultReplay => {
                            residual["result"] == "replay_consistent"
                        }
                        ObligationProperty::Idempotency
                        | ObligationProperty::TransactionSerializability
                        | ObligationProperty::TransactionOrdering
                        | ObligationProperty::Recoverability => true,
                    }
            })
            .map(|(declaration, _)| declaration.clone())
            .collect(),

        Fact::Isolation { .. }
        | Fact::Lock { .. }
        | Fact::GuardedMutation { .. }
        | Fact::Cursor { .. }
        | Fact::Fence { .. }
        | Fact::ConflictClosure { .. }
        | Fact::CyclicComponents { .. }
        | Fact::AdmitsNoMessages { .. }
        | Fact::NoAdmittedPaths { .. }
        | Fact::ReturnsNoResult { .. }
        | Fact::Terminal { .. }
        | Fact::StableRoot { .. }
        | Fact::Delivery { .. }
        | Fact::MessageIdentity { .. }
        | Fact::DeduplicatedBy { .. }
        | Fact::NaturallyReplayable { .. }
        | Fact::ArtifactRoute { .. }
        | Fact::OutboxRedrive { .. }
        | Fact::RetryMayRepeat { .. }
        | Fact::ExternalGuarantee { .. }
        | Fact::Propagation { .. }
        | Fact::ReliesOn { .. }
        | Fact::Realization { .. } => Vec::new(),
    }
}

fn grounds(obligation: &Obligation) -> Grounds {
    Grounds {
        basis: obligation.basis.clone(),
        blocked_on: obligation.blocked_on.clone(),
    }
}

/// A side's reliance (§13.1): every fact some proven obligation's basis
/// holds, by canonical form, with how many obligations hold it.
fn reliance(analysis: &Analysis) -> BTreeMap<String, (Fact, usize)> {
    let mut reliance: BTreeMap<String, (Fact, usize)> = BTreeMap::new();

    for obligation in analysis.obligations.values() {
        let Some(basis) = &obligation.basis else {
            continue;
        };

        for fact in basis {
            reliance
                .entry(fact.fact.canonical())
                .or_insert_with(|| (fact.fact.clone(), 0))
                .1 += 1;
        }
    }

    reliance
}
