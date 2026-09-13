//! The transaction proof view-model: what a reader has to see to
//! understand *why* a transaction serializability or ordering verdict
//! holds, in a shape a drawing can be made from.
//!
//! The obligation report says that a verdict holds and lists the facts
//! it rests on as prose. A serializability argument, though, is a graph:
//! the transaction's conflict closure, the potential serialization
//! dependencies among its members, and the declared fact that
//! commit-orders each of them — or the cycle that no fact orders. An
//! ordering argument is that graph plus one guard step that makes the
//! commits follow the declared position. This module extracts exactly
//! that structure, per declared requirement, from the same conflict
//! index the provers read, so the front end draws the argument rather
//! than paraphrasing it.
//!
//! Everything here is derived from the model alone. The verdict overlay
//! comes from the obligation report, joined by obligation id; without a
//! report the graph still says which dependencies are constrained by
//! which declaration, which is a fact of the model.

use std::collections::BTreeSet;

use serde::Serialize;

use crate::analyzer::verification::transaction_conflicts::{
    AccessFields, CommitOrderEvidence, ConflictIndex, DependencyEvidence, DependencyGap,
    DependencyKind, DependencySide, SelectorOverlap, TransactionRef, isolation_label,
};
use crate::analyzer::verification::transaction_ordering::{
    self, TransactionOrderingProof, TransactionOrderingVerdict,
};
use crate::analyzer::verification::transaction_serializability::{
    self, TransactionSerializabilityVerdict,
};
use crate::spec::{CursorAdvanceRule, Id, Model, TransactionIsolation, TransactionStep, ValueRef};

/// Every declared transaction requirement of the model, as an argument
/// a drawing can be made from.
#[derive(Debug, Clone, Serialize)]
pub struct TransactionProofs {
    pub serializability: Vec<SerializabilityView>,
    pub ordering: Vec<OrderingView>,
}

/// One `SerializableBy(key)` requirement: its conflict closure, the
/// dependency graph over it, and how far the declared facts go.
#[derive(Debug, Clone, Serialize)]
pub struct SerializabilityView {
    /// The report obligation this argument belongs to
    /// (`oblig.<operation>.<transaction>.transaction_serializability.<n>`).
    pub obligation: String,
    pub operation: Id,
    pub transaction: Id,
    pub requirement: usize,
    /// The key, as `input.x.field`.
    pub key: String,
    pub proven: bool,
    /// `serializable_isolation` or `conflict_graph` for a proven
    /// argument; absent when no route closes it.
    pub route: Option<String>,
    /// One sentence on why the verdict is what it is.
    pub headline: String,
    /// The conflict closure, the requiring transaction first.
    pub nodes: Vec<ClosureNode>,
    /// Every potential dependency among the closure's members.
    pub edges: Vec<DependencyView>,
    /// The dependencies grouped by ordered transaction pair — what a
    /// drawing shows as one arrow, with the underlying dependencies
    /// behind it.
    pub pairs: Vec<PairView>,
    /// The strongly connected components a non-serializable history can
    /// still run through: each a list of transaction ids in closure
    /// order. Empty for a proven argument.
    pub cycles: Vec<Vec<Id>>,
    /// The checker's obstacle sentences, empty for a proven argument.
    pub obstacles: Vec<String>,
}

/// A member of a conflict closure.
#[derive(Debug, Clone, Serialize)]
pub struct ClosureNode {
    pub operation: Id,
    pub transaction: Id,
    /// The transaction step's location in its operation's program.
    pub location: String,
    pub isolation: String,
    pub serializable: bool,
    /// The transaction the requirement is declared on.
    pub root: bool,
    /// A member of an unconstrained cycle.
    pub in_cycle: bool,
}

/// One potential serialization dependency, with the fact that fixes
/// the commit order across it or the gaps that leave it open.
#[derive(Debug, Clone, Serialize)]
pub struct DependencyView {
    pub id: String,
    /// Transaction ids; equal for a dependency between two concurrent
    /// executions of one transaction.
    pub source: Id,
    pub target: Id,
    /// `wr`, `rw`, or `ww`.
    pub kind: String,
    /// The kind, said: `write → read`, `read → write (anti-dependency)`,
    /// `write → write`.
    pub kind_label: String,
    pub object: Id,
    /// The overlapping fields, `all fields`, or `unknown fields`.
    pub fields: String,
    /// One-based steps of the two accesses, and their access modes.
    pub source_step: usize,
    pub target_step: usize,
    pub source_mode: String,
    pub target_mode: String,
    /// `same instance`, `may overlap`, or `disjoint`.
    pub overlap: String,
    pub constrained: bool,
    /// A short label for the commit-order evidence: `strict lock`,
    /// `version validation`, `ordered cursor`, `atomic write order`,
    /// `committed read`. Absent when nothing constrains the edge.
    pub evidence: Option<String>,
    /// Short labels for what is missing on an unconstrained edge.
    pub gaps: Vec<String>,
    /// The evidence, or the gaps, as sentences.
    pub explanation: String,
    /// A fence is recorded on the edge; never evidence on its own.
    pub fence: bool,
}

/// The dependencies from one transaction to another (or to a concurrent
/// execution of itself), summarized for one arrow of the drawing.
#[derive(Debug, Clone, Serialize)]
pub struct PairView {
    pub id: String,
    pub source: Id,
    pub target: Id,
    /// Two concurrent executions of one transaction.
    pub self_loop: bool,
    /// How many dependencies the arrow stands for, and how many of them
    /// no declared fact commit-orders.
    pub dependencies: usize,
    pub open: usize,
    pub constrained: bool,
    /// The distinct kinds present, in `wr`, `rw`, `ww` order.
    pub kinds: Vec<String>,
    pub objects: Vec<Id>,
    /// The distinct evidence labels across the constrained dependencies.
    pub evidence: Vec<String>,
    /// The distinct gap labels across the open dependencies.
    pub gaps: Vec<String>,
    /// The ids of the underlying dependencies, in edge order.
    pub edge_ids: Vec<String>,
    /// One line for the arrow's label or tooltip.
    pub summary: String,
}

/// One `OrderedBy(key, position)` requirement: the guard that persists
/// the position, over the serializability argument it rests on.
#[derive(Debug, Clone, Serialize)]
pub struct OrderingView {
    pub obligation: String,
    pub operation: Id,
    pub transaction: Id,
    pub requirement: usize,
    pub key: String,
    pub position: String,
    pub proven: bool,
    pub headline: String,
    /// The cursor or fence step that carries the position, when the
    /// transaction has one — present even when the argument fails for
    /// another reason, so the drawing can show what is there.
    pub mechanism: Option<MechanismView>,
    /// The serializability argument over the ordering key: an ordered
    /// history is one particular serial order, so this must hold first.
    pub serializability: SerializabilityView,
    pub obstacles: Vec<String>,
}

/// The guard step that makes commits follow the position.
#[derive(Debug, Clone, Serialize)]
pub struct MechanismView {
    /// `cursor` or `fence`.
    pub kind: String,
    pub object: Id,
    pub field: String,
    /// `successor` or `monotonic_after` for a cursor; absent for a fence.
    pub rule: Option<String>,
    /// One-based step in the transaction.
    pub step: usize,
    /// The value the guard compares against the stored one, as text.
    pub incoming: String,
    /// Whether that value is the requirement's position.
    pub carries_position: bool,
}

/// Extracts the serializability and ordering arguments of every
/// declared transaction requirement.
pub fn extract(model: &Model) -> TransactionProofs {
    let index = ConflictIndex::build(model);

    let serializability = transaction_serializability::check(model)
        .into_iter()
        .map(|check| {
            let (proven, obstacles) = match &check.verdict {
                TransactionSerializabilityVerdict::Proven { .. } => (true, Vec::new()),
                TransactionSerializabilityVerdict::Unproven { .. } => (
                    false,
                    check
                        .diagnostic()
                        .map(|diagnostic| {
                            diagnostic
                                .evidence
                                .into_iter()
                                .map(|evidence| evidence.message)
                                .collect()
                        })
                        .unwrap_or_default(),
                ),
            };

            serializability_view(
                &index,
                &check.operation,
                &check.transaction,
                &check.key,
                check.requirement,
                "transaction_serializability",
                proven,
                obstacles,
            )
        })
        .collect();

    let ordering = transaction_ordering::check(model)
        .into_iter()
        .map(|check| {
            let root = template_position(&index, &check.operation, &check.transaction);

            let obstacles = check
                .diagnostic()
                .map(|diagnostic| {
                    diagnostic
                        .evidence
                        .into_iter()
                        .map(|evidence| evidence.message)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();

            let proven = matches!(check.verdict, TransactionOrderingVerdict::Proven { .. });

            // The serializability argument over the ordering key, which
            // the ordering verdict presupposes.
            let closure_proven = root.is_some_and(|root| {
                transaction_serializability::prove(&index, root, &check.key).is_ok()
            });

            let serializability = serializability_view(
                &index,
                &check.operation,
                &check.transaction,
                &check.key,
                check.requirement,
                "transaction_ordering",
                closure_proven,
                Vec::new(),
            );

            let mechanism = mechanism_view(&index, root, &check.position, &check.verdict);

            let headline = match (&check.verdict, &mechanism) {
                (TransactionOrderingVerdict::Proven { .. }, Some(mechanism)) => format!(
                    "Proven: {} on {}.{} {} within each {}, over a serializable closure.",
                    if mechanism.kind == "cursor" {
                        "the cursor"
                    } else {
                        "the fence"
                    },
                    mechanism.object,
                    mechanism.field,
                    match mechanism.rule.as_deref() {
                        Some("successor") => "applies exactly the next position",
                        Some(_) => "applies only a greater position",
                        None => "refuses a stale token",
                    },
                    value_ref_label(&check.key)
                ),
                (TransactionOrderingVerdict::Proven { .. }, None) => {
                    "Proven over a serializable closure.".to_string()
                }
                (TransactionOrderingVerdict::Unproven { .. }, _) => check
                    .diagnostic()
                    .map(|diagnostic| diagnostic.message)
                    .unwrap_or_else(|| "Unproven.".to_string()),
            };

            OrderingView {
                obligation: format!(
                    "oblig.{}.{}.transaction_ordering.{}",
                    check.operation, check.transaction, check.requirement
                ),
                operation: check.operation.clone(),
                transaction: check.transaction.clone(),
                requirement: check.requirement,
                key: value_ref_label(&check.key),
                position: value_ref_label(&check.position),
                proven,
                headline,
                mechanism,
                serializability,
                obstacles,
            }
        })
        .collect();

    TransactionProofs {
        serializability,
        ordering,
    }
}

fn template_position(index: &ConflictIndex<'_>, operation: &Id, transaction: &Id) -> Option<usize> {
    index.templates.iter().position(|template| {
        template.reference.operation == *operation && template.reference.transaction == *transaction
    })
}

#[allow(clippy::too_many_arguments)]
fn serializability_view(
    index: &ConflictIndex<'_>,
    operation: &Id,
    transaction: &Id,
    key: &ValueRef,
    requirement: usize,
    slug: &str,
    proven: bool,
    obstacles: Vec<String>,
) -> SerializabilityView {
    let obligation = format!("oblig.{operation}.{transaction}.{slug}.{requirement}");

    let Some(root) = template_position(index, operation, transaction) else {
        return SerializabilityView {
            obligation,
            operation: operation.clone(),
            transaction: transaction.clone(),
            requirement,
            key: value_ref_label(key),
            proven,
            route: None,
            headline: "The transaction is not in the conflict index.".to_string(),
            nodes: Vec::new(),
            edges: Vec::new(),
            pairs: Vec::new(),
            cycles: Vec::new(),
            obstacles,
        };
    };

    let closure = index.closure(root);
    let dependencies = index.dependencies(&closure);
    let cycles = index.unconstrained_cycles(&closure, &dependencies);

    let all_serializable = closure.iter().all(|&position| {
        index.templates[position].transaction.isolation == TransactionIsolation::Serializable
    });

    let in_cycle: BTreeSet<&TransactionRef> = cycles
        .iter()
        .flat_map(|cycle| cycle.members.iter())
        .collect();

    // The root first, then the rest in closure order.
    let mut order: Vec<usize> = vec![root];
    order.extend(closure.iter().copied().filter(|&position| position != root));

    let nodes = order
        .iter()
        .map(|&position| {
            let template = &index.templates[position];
            let isolation = template.transaction.isolation;

            ClosureNode {
                operation: template.reference.operation.clone(),
                transaction: template.reference.transaction.clone(),
                location: template.reference.location.to_string(),
                isolation: isolation_label(isolation).to_string(),
                serializable: isolation == TransactionIsolation::Serializable,
                root: position == root,
                in_cycle: in_cycle.contains(&template.reference),
            }
        })
        .collect();

    let edges: Vec<DependencyView> = dependencies
        .iter()
        .enumerate()
        .map(|(position, dependency)| dependency_view(index, position, dependency))
        .collect();

    let pairs = pair_views(&edges);

    let route = if !proven {
        None
    } else if all_serializable {
        Some("serializable_isolation".to_string())
    } else {
        Some("conflict_graph".to_string())
    };

    let headline = match route.as_deref() {
        Some("serializable_isolation") => format!(
            "Proven: every transaction in the conflict closure ({}) declares serializable \
             isolation, so the database orders the closure itself.",
            closure.len()
        ),
        Some(_) => format!(
            "Proven from the serialization graph: each of the {} dependencies among the \
             {} transactions of the closure that could close a cycle is commit-ordered by a \
             declared fact.",
            dependencies.len(),
            closure.len()
        ),
        None if cycles.is_empty() => {
            "Unproven: the checker could not establish the argument.".to_string()
        }
        None => {
            let cycle = &cycles[0];

            let unconstrained = cycle.unconstrained.len();

            format!(
                "Unproven: the cycle {} contains {} no declared fact commit-orders, so a \
                 non-serializable committed history through it cannot be excluded.",
                transaction_serializability::chain(&cycle.members),
                if unconstrained == 1 {
                    "a dependency".to_string()
                } else {
                    format!("{unconstrained} dependencies")
                }
            )
        }
    };

    SerializabilityView {
        obligation,
        operation: operation.clone(),
        transaction: transaction.clone(),
        requirement,
        key: value_ref_label(key),
        proven,
        route,
        headline,
        nodes,
        edges,
        pairs,
        cycles: cycles
            .iter()
            .map(|cycle| {
                cycle
                    .members
                    .iter()
                    .map(|member| member.transaction.clone())
                    .collect()
            })
            .collect(),
        obstacles,
    }
}

fn dependency_view(
    index: &ConflictIndex<'_>,
    position: usize,
    dependency: &DependencyEvidence,
) -> DependencyView {
    let constrained = dependency.constrained();

    let (kind, kind_label) = match dependency.kind {
        DependencyKind::WriteRead => ("wr", "write → read"),
        DependencyKind::ReadWriteAntiDependency => ("rw", "read → write (anti-dependency)"),
        DependencyKind::WriteWrite => ("ww", "write → write"),
    };

    let evidence = match &dependency.evidence {
        CommitOrderEvidence::IntrinsicCommittedRead { .. } => Some("committed read"),
        CommitOrderEvidence::AtomicWriteOrder { .. } => Some("atomic write order"),
        CommitOrderEvidence::StrictLock { .. } => Some("strict lock"),
        CommitOrderEvidence::VersionValidation { .. } => Some("version validation"),
        CommitOrderEvidence::OrderedCursor { .. } => Some("ordered cursor"),
        CommitOrderEvidence::None => None,
    };

    let gaps: Vec<String> = dependency.gaps.iter().map(gap_label).collect();

    let explanation = if constrained {
        transaction_serializability::evidence_sentence(dependency)
    } else {
        transaction_serializability::gap_sentences(&dependency.gaps)
    };

    DependencyView {
        id: format!("dep.{position}"),
        source: dependency.source.transaction.clone(),
        target: dependency.target.transaction.clone(),
        kind: kind.to_string(),
        kind_label: kind_label.to_string(),
        object: dependency.object.clone(),
        fields: fields_label(index, dependency),
        source_step: dependency.source_step + 1,
        target_step: dependency.target_step + 1,
        source_mode: dependency.source_mode.to_string(),
        target_mode: dependency.target_mode.to_string(),
        overlap: match dependency.selector_overlap {
            SelectorOverlap::Disjoint => "disjoint",
            SelectorOverlap::Overlapping => "same instance",
            SelectorOverlap::Unknown => "may overlap",
        }
        .to_string(),
        constrained,
        evidence: evidence.map(str::to_string),
        gaps,
        explanation,
        fence: dependency.fence.is_some(),
    }
}

/// Groups the dependencies by ordered transaction pair, keeping the
/// order in which pairs first appear.
fn pair_views(edges: &[DependencyView]) -> Vec<PairView> {
    let mut pairs: Vec<PairView> = Vec::new();

    for edge in edges {
        let pair = match pairs
            .iter_mut()
            .find(|pair| pair.source == edge.source && pair.target == edge.target)
        {
            Some(pair) => pair,
            None => {
                pairs.push(PairView {
                    id: format!("pair.{}", pairs.len()),
                    source: edge.source.clone(),
                    target: edge.target.clone(),
                    self_loop: edge.source == edge.target,
                    dependencies: 0,
                    open: 0,
                    constrained: true,
                    kinds: Vec::new(),
                    objects: Vec::new(),
                    evidence: Vec::new(),
                    gaps: Vec::new(),
                    edge_ids: Vec::new(),
                    summary: String::new(),
                });

                pairs.last_mut().expect("just pushed")
            }
        };

        pair.dependencies += 1;
        pair.edge_ids.push(edge.id.clone());

        if !pair.objects.contains(&edge.object) {
            pair.objects.push(edge.object.clone());
        }

        if !pair.kinds.contains(&edge.kind) {
            pair.kinds.push(edge.kind.clone());
        }

        if edge.constrained {
            if let Some(evidence) = &edge.evidence
                && !pair.evidence.contains(evidence)
            {
                pair.evidence.push(evidence.clone());
            }
        } else {
            pair.open += 1;
            pair.constrained = false;

            for gap in &edge.gaps {
                if !pair.gaps.contains(gap) {
                    pair.gaps.push(gap.clone());
                }
            }
        }
    }

    for pair in &mut pairs {
        pair.kinds
            .sort_by_key(|kind| ["wr", "rw", "ww"].iter().position(|k| k == kind));

        let objects = pair
            .objects
            .iter()
            .map(|object| object.to_string())
            .collect::<Vec<_>>()
            .join(", ");

        let noun = if pair.dependencies == 1 {
            "dependency"
        } else {
            "dependencies"
        };

        pair.summary = if pair.constrained {
            format!(
                "{} {noun} on {objects}, all commit-ordered by {}",
                pair.dependencies,
                join_labels(&pair.evidence)
            )
        } else {
            format!(
                "{} of {} {noun} on {objects} unconstrained: {}",
                pair.open,
                pair.dependencies,
                join_labels(&pair.gaps)
            )
        };
    }

    pairs
}

/// `a`, `a and b`, or `a, b, and c`.
fn join_labels(labels: &[String]) -> String {
    match labels {
        [] => "nothing".to_string(),
        [one] => one.clone(),
        [a, b] => format!("{a} and {b}"),
        [init @ .., last] => format!("{}, and {last}", init.join(", ")),
    }
}

fn gap_label(gap: &DependencyGap) -> String {
    match gap {
        DependencyGap::LockCoverageMissing { side, .. } => match side {
            DependencySide::Reader => "no lock on the reader".to_string(),
            DependencySide::Writer => "no exclusive lock on the writer".to_string(),
        },
        DependencyGap::LockAcquiredAfterProtectedAccess { side, .. } => {
            format!("{side} lock taken after the access")
        }
        DependencyGap::VersionValidationMissing { .. } => "no version validation".to_string(),
        DependencyGap::VersionBumpMissing { .. } => "no version bump".to_string(),
        DependencyGap::IsolationUnspecified { .. } => "isolation unspecified".to_string(),
        DependencyGap::TransactionConflictUnknownSelectorOverlap { .. } => {
            "overlap not proven disjoint".to_string()
        }
        DependencyGap::TransactionConflictUnknownFieldOverlap { .. } => {
            "field footprint unknown".to_string()
        }
    }
}

/// The fields two accesses meet on, from the access index.
fn fields_label(index: &ConflictIndex<'_>, dependency: &DependencyEvidence) -> String {
    let access = |reference: &TransactionRef, step: usize| {
        index
            .templates
            .iter()
            .find(|template| template.reference == *reference)
            .and_then(|template| {
                template
                    .accesses
                    .iter()
                    .find(|access| access.step == step && access.object == dependency.object)
            })
    };

    let (Some(source), Some(target)) = (
        access(&dependency.source, dependency.source_step),
        access(&dependency.target, dependency.target_step),
    ) else {
        return "unknown fields".to_string();
    };

    match (&source.fields, &target.fields) {
        (AccessFields::Unknown, _) | (_, AccessFields::Unknown) => "unknown fields".to_string(),
        (AccessFields::All, AccessFields::All) => "all fields".to_string(),
        (AccessFields::All, AccessFields::Only(fields))
        | (AccessFields::Only(fields), AccessFields::All) => join_fields(fields.iter()),
        (AccessFields::Only(a), AccessFields::Only(b)) => {
            let shared: Vec<_> = a
                .iter()
                .filter(|field| {
                    b.iter().any(|other| {
                        field.0.starts_with(&other.0) || other.0.starts_with(&field.0)
                    })
                })
                .collect();

            if shared.is_empty() {
                "no shared field".to_string()
            } else {
                join_fields(shared.into_iter())
            }
        }
    }
}

fn join_fields<'a>(fields: impl Iterator<Item = &'a crate::spec::FieldPath>) -> String {
    fields
        .map(|field| field.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The cursor or fence of the requiring transaction: the one the proof
/// cites when it holds, else the first guard the transaction carries.
fn mechanism_view(
    index: &ConflictIndex<'_>,
    root: Option<usize>,
    position: &ValueRef,
    verdict: &TransactionOrderingVerdict,
) -> Option<MechanismView> {
    let template = &index.templates[root?];

    let operation = index.model.operations.get(&template.reference.operation)?;

    let proven_step = match verdict {
        TransactionOrderingVerdict::Proven { proof, .. } => Some(match proof {
            TransactionOrderingProof::Cursor { step, .. }
            | TransactionOrderingProof::Fence { step, .. } => *step,
        }),
        TransactionOrderingVerdict::Unproven { .. } => None,
    };

    let mut first = None;

    for (step, inner) in template.transaction.steps.iter().enumerate() {
        let view = match inner {
            TransactionStep::AdvanceCursor(advance) => MechanismView {
                kind: "cursor".to_string(),
                object: advance.target.object.clone(),
                field: advance.field.to_string(),
                rule: Some(
                    match advance.rule {
                        CursorAdvanceRule::Successor => "successor",
                        CursorAdvanceRule::MonotonicAfter => "monotonic_after",
                    }
                    .to_string(),
                ),
                step: step + 1,
                incoming: value_ref_label(&advance.incoming),
                carries_position: index.same_value(operation, &advance.incoming, position),
            },

            TransactionStep::Fence(fence) => MechanismView {
                kind: "fence".to_string(),
                object: fence.target.object.clone(),
                field: fence.field.to_string(),
                rule: None,
                step: step + 1,
                incoming: value_ref_label(&fence.token),
                carries_position: index.same_value(operation, &fence.token, position),
            },

            _ => continue,
        };

        if proven_step == Some(step) {
            return Some(view);
        }

        if first.is_none() {
            first = Some(view);
        }
    }

    first
}

fn value_ref_label(value: &ValueRef) -> String {
    format!("{}.{}", value.source.id(), value.path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flash_checkout() -> Model {
        let source = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/flash_checkout.yaml"
        ))
        .expect("fixture readable");

        crate::parser::yaml::parse(&source).expect("fixture parses")
    }

    fn view<'a>(views: &'a [SerializabilityView], transaction: &str) -> &'a SerializabilityView {
        views
            .iter()
            .find(|view| view.transaction.0 == transaction)
            .unwrap_or_else(|| panic!("no view for {transaction}"))
    }

    #[test]
    fn apply_payment_is_drawn_as_a_constrained_graph() {
        let proofs = extract(&flash_checkout());

        let apply = view(&proofs.serializability, "tx.apply_payment");

        assert!(apply.proven);
        assert_eq!(apply.route.as_deref(), Some("conflict_graph"));
        assert_eq!(
            apply.obligation,
            "oblig.operation.apply_payment.tx.apply_payment.transaction_serializability.0"
        );
        assert!(apply.nodes[0].root);
        assert_eq!(apply.nodes[0].transaction.0, "tx.apply_payment");
        assert!(apply.nodes.iter().any(|node| node.transaction.0 == "tx.cancel_order"));
        assert!(apply.cycles.is_empty());
        assert!(!apply.edges.is_empty());
        assert!(apply.edges.iter().all(|edge| edge.constrained));
        assert!(
            apply
                .edges
                .iter()
                .any(|edge| edge.evidence.as_deref() == Some("version validation")),
            "{:#?}",
            apply.edges
        );

        // One arrow per ordered pair, each standing for its dependencies.
        let to_cancel = apply
            .pairs
            .iter()
            .find(|pair| {
                pair.source.0 == "tx.apply_payment" && pair.target.0 == "tx.cancel_order"
            })
            .expect("apply_payment depends on cancel_order");

        assert!(to_cancel.constrained);
        assert_eq!(to_cancel.open, 0);
        assert_eq!(
            to_cancel.dependencies,
            to_cancel.edge_ids.len(),
            "{to_cancel:#?}"
        );
        assert!(to_cancel.evidence.contains(&"version validation".to_string()));
        assert!(to_cancel.summary.contains("all commit-ordered by"), "{}", to_cancel.summary);
        assert!(apply.pairs.iter().any(|pair| pair.self_loop));
        assert_eq!(
            apply.pairs.iter().map(|pair| pair.dependencies).sum::<usize>(),
            apply.edges.len()
        );
    }

    #[test]
    fn reserve_inventory_is_drawn_with_its_unconstrained_cycle() {
        let proofs = extract(&flash_checkout());

        let reserve = view(&proofs.serializability, "tx.reserve_inventory");

        assert!(!reserve.proven);
        assert_eq!(reserve.route, None);
        assert!(!reserve.cycles.is_empty(), "{reserve:#?}");
        assert!(reserve.headline.starts_with("Unproven: the cycle"));
        assert!(!reserve.obstacles.is_empty());

        let open: Vec<_> = reserve.edges.iter().filter(|edge| !edge.constrained).collect();

        assert!(!open.is_empty());
        assert!(open.iter().all(|edge| edge.evidence.is_none() && !edge.gaps.is_empty()));
        assert!(open.iter().any(|edge| edge.kind == "rw"));
        assert!(reserve.nodes.iter().any(|node| node.in_cycle));

        let self_race = reserve
            .pairs
            .iter()
            .find(|pair| pair.self_loop)
            .expect("reserve_inventory races its own concurrent execution");

        assert!(!self_race.constrained);
        assert!(self_race.gaps.contains(&"no version validation".to_string()));
        assert!(self_race.summary.contains("unconstrained:"), "{}", self_race.summary);
    }

    #[test]
    fn apply_payment_ordering_shows_its_cursor_over_the_closure() {
        let proofs = extract(&flash_checkout());

        let ordering = &proofs.ordering[0];

        assert_eq!(ordering.transaction.0, "tx.apply_payment");
        assert!(ordering.proven);
        assert_eq!(ordering.position, "input.apply_payment.captured.sequence");

        let mechanism = ordering.mechanism.as_ref().expect("a cursor");

        assert_eq!(mechanism.kind, "cursor");
        assert_eq!(mechanism.rule.as_deref(), Some("successor"));
        assert_eq!(mechanism.field, "last_applied_sequence");
        assert!(mechanism.carries_position);
        assert!(ordering.serializability.proven);
        assert_eq!(
            ordering.obligation,
            "oblig.operation.apply_payment.tx.apply_payment.transaction_ordering.0"
        );
    }
}
