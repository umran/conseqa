//! The realization half of a change (§13.4 of the spec change analysis
//! revision).
//!
//! When either side has a stack profile, the diff compares the two
//! sides' realization profiles as well as their specs: what the stack
//! became, how each realizable declaration's needs and chosen
//! realization moved, which declarations the head's stack cannot
//! realize, and which realization requirements the code must now
//! provide or no longer needs to. A spec change that adds a need the
//! chosen entry does not provide is reported here, at the spec gate,
//! rather than discovered by an implementing agent.
//!
//! Needs and choices are derived from each side's spec and realization
//! files alone, so the section does not wait on the obligation diff,
//! and an operation without a program yet simply asks nothing of its
//! data models (Part VI §6.3).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::analyzer::basis::{EntryRef, Fact, TextualKey};
use crate::design::graph_build;
use crate::realization::capability::{self, Capability, Need, Spec};
use crate::realization::choice::{Gap, Problem, State, Status, Valid};
use crate::realization::requirements;

use super::side::Side;

/// What changed in what realizes the spec (§13.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RealizationChanges {
    pub stack: StackChange,

    /// Every realizable declaration whose needs, or whose chosen
    /// realization or state, changed, in the partition's order.
    pub declarations: Vec<DeclarationRealization>,

    /// Every declaration the head's stack cannot realize, with its
    /// report (Part VI §10.4).
    pub unrealizable: Vec<Unrealizable>,

    /// The realization half of the reliance delta: configuration the
    /// code must now provide, or no longer needs to.
    pub requirements: RequirementChanges,
}

/// The technologies the head's stack names that the base's does not,
/// and the reverse, each `name@version`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StackChange {
    pub added: Vec<String>,
    pub removed: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeclarationRealization {
    pub declaration: TextualKey,
    pub needs: NeedChange,

    /// How the side realizes it, or `None` where the side has no stack
    /// profile or no such declaration.
    pub base: Option<Realizing>,
    pub head: Option<Realizing>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NeedChange {
    pub added: Vec<Capability>,
    pub removed: Vec<Capability>,
}

/// One side's realization of a declaration: its state, and the entry
/// realizing it when one does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Realizing {
    pub state: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<EntryRef>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<String>>,
}

impl Realizing {
    fn of(state: &State) -> Self {
        let realization = state.realization();

        Self {
            state: state.name().to_string(),
            entry: realization.map(|(entry, _)| entry.clone()),
            options: realization.map(|(_, options)| options.to_vec()),
        }
    }
}

/// A declaration the head's stack cannot realize: what it needs, its
/// state, and its report (Part VI §10.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Unrealizable {
    pub declaration: TextualKey,

    /// `ambiguous`, `invalid_choice`, or `infeasible`.
    pub state: String,

    pub needs: Vec<Capability>,

    /// For an ambiguous declaration, every valid candidate with each
    /// minimal option set.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub valid: Vec<Valid>,

    /// For an invalid choice, what no longer holds.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub problems: Vec<Problem>,

    /// For an infeasible declaration, what each candidate misses.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gaps: Vec<Gap>,
}

impl Unrealizable {
    fn of(declaration: &TextualKey, needs: &[Need], state: &State) -> Self {
        let (valid, problems, gaps) = match state {
            State::Ambiguous { valid } => (valid.clone(), Vec::new(), Vec::new()),
            State::InvalidChoice { problems, .. } => (Vec::new(), problems.clone(), Vec::new()),
            State::Infeasible { gaps } => (Vec::new(), Vec::new(), gaps.clone()),
            State::Chosen { .. } | State::Automatic { .. } | State::Unneeded => {
                (Vec::new(), Vec::new(), Vec::new())
            }
        };

        Self {
            declaration: declaration.clone(),
            state: state.name().to_string(),
            needs: needs.iter().map(|need| need.capability).collect(),
            valid,
            problems,
            gaps,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequirementChanges {
    pub introduced: Vec<Fact>,
    pub withdrawn: Vec<Fact>,
}

/// What one side says of realization.
#[derive(Default)]
struct Realized {
    technologies: BTreeSet<String>,
    needs: BTreeMap<TextualKey, Vec<Need>>,
    status: Option<Status>,
    requirements: BTreeMap<String, Fact>,
}

impl Realized {
    fn of(side: &Side) -> Self {
        let Some(workspace) = &side.workspace else {
            return Self {
                technologies: side
                    .realization
                    .iter()
                    .flat_map(|profile| profile.stack.named())
                    .collect(),
                ..Self::default()
            };
        };

        let needs = capability::needs_of(&Spec::of_workspace(workspace));

        let Some(profile) = &side.realization else {
            return Self {
                needs,
                ..Self::default()
            };
        };

        let status = Status::with_needs(profile, needs.clone());
        let graph = graph_build::build(workspace, None);

        let requirements = requirements::placed(&status, &profile.catalogue, workspace, &graph)
            .into_iter()
            .map(|placed| (placed.fact.canonical(), placed.fact))
            .collect();

        Self {
            technologies: profile.stack.named().into_iter().collect(),
            needs,
            status: Some(status),
            requirements,
        }
    }

    fn state(&self, declaration: &TextualKey) -> Option<&State> {
        self.status
            .as_ref()?
            .declarations
            .iter()
            .find_map(|realized| (&realized.declaration == declaration).then_some(&realized.state))
    }
}

/// The realization section of a diff, present when either side has a
/// stack profile.
pub fn changes(base: &Side, head: &Side) -> Option<RealizationChanges> {
    if base.realization.is_none() && head.realization.is_none() {
        return None;
    }

    let base = Realized::of(base);
    let head = Realized::of(head);

    let declarations: BTreeSet<&TextualKey> = base.needs.keys().chain(head.needs.keys()).collect();

    let capabilities = |realized: &Realized, declaration: &TextualKey| -> BTreeSet<Capability> {
        realized
            .needs
            .get(declaration)
            .into_iter()
            .flatten()
            .map(|need| need.capability)
            .collect()
    };

    let changed = declarations
        .into_iter()
        .filter_map(|declaration| {
            let before = capabilities(&base, declaration);
            let after = capabilities(&head, declaration);

            let change = DeclarationRealization {
                declaration: declaration.clone(),
                needs: NeedChange {
                    added: after.difference(&before).copied().collect(),
                    removed: before.difference(&after).copied().collect(),
                },
                base: base.state(declaration).map(Realizing::of),
                head: head.state(declaration).map(Realizing::of),
            };

            let moved = !change.needs.added.is_empty()
                || !change.needs.removed.is_empty()
                || change.base != change.head;

            moved.then_some(change)
        })
        .collect();

    let unrealizable = head
        .status
        .iter()
        .flat_map(|status| &status.declarations)
        .filter(|realized| realized.state.fails())
        .map(|realized| Unrealizable::of(&realized.declaration, &realized.needs, &realized.state))
        .collect();

    let only = |one: &BTreeMap<String, Fact>, other: &BTreeMap<String, Fact>| -> Vec<Fact> {
        one.iter()
            .filter(|(canonical, _)| !other.contains_key(*canonical))
            .map(|(_, fact)| fact.clone())
            .collect()
    };

    Some(RealizationChanges {
        stack: StackChange {
            added: head
                .technologies
                .difference(&base.technologies)
                .cloned()
                .collect(),
            removed: base
                .technologies
                .difference(&head.technologies)
                .cloned()
                .collect(),
        },
        declarations: changed,
        unrealizable,
        requirements: RequirementChanges {
            introduced: only(&head.requirements, &base.requirements),
            withdrawn: only(&base.requirements, &head.requirements),
        },
    })
}

impl RealizationChanges {
    /// Whether any declaration on head is infeasible or has an invalid
    /// choice: what the `infeasible` gate fails on (§15.3).
    pub fn infeasible(&self) -> bool {
        self.unrealizable.iter().any(|unrealizable| {
            matches!(unrealizable.state.as_str(), "infeasible" | "invalid_choice")
        })
    }

    /// Whether any declaration on head is ambiguous: what the
    /// `ambiguous` gate fails on (§15.3).
    pub fn ambiguous(&self) -> bool {
        self.unrealizable
            .iter()
            .any(|unrealizable| unrealizable.state == "ambiguous")
    }
}
