//! The work list (§11 of the conformance revision): what the code owes
//! the spec, as items an agent takes one at a time.
//!
//! Work is derived, never stored (§3.3). There is one item per
//! declaration with conditions, carrying all of them, since an agent
//! changing a declaration's code should handle everything wrong with it
//! at once, and one per orphaned key (§11.1). An item's id is its key,
//! so it is the same item on every derivation, machine, and branch until
//! its conditions clear (§11.2).
//!
//! Items come by priority (§11.3), then in dependency order (§11.4):
//! what is declared before what uses it, and among transactions, effect
//! sites, and transitions, callees before callers, since a caller
//! implemented first has nothing correct to call. Each item names the
//! items it depends on and the files it will likely touch, so that a
//! scheduler can run items side by side in waves that do not collide.
//!
//! A Part I diff scopes the list to the code work its spec changes owe
//! (§11.5): the diff from a base to the working tree, for a pull request
//! changing spec and code together, or across a spec range, which leaves
//! out what other changes' leading specs owe on a shared trunk. A
//! non-empty list is a normal state: the spec may lead the code (§11.6).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::analyzer::basis::{BasisFact, DeclarationKey, ObligationKey, TextualKey};
use crate::design::change::diff::{
    self, DiffDocument, ObligationClass, ObligationDiff, Obligations, SideRecord,
};
use crate::design::change::side::{self, Side, SideName, Source};
use crate::design::graph::EdgeKind;
use crate::design::symbol::SymbolKey;
use crate::design::workspace::WorkspaceState;
use crate::realization::choice::Profile;

use super::completeness;
use super::state::{
    self, Condition, ConditionKind, DeclarationState, Derivation, ScopeFailure, State,
};

/// The work list (§11).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Plan {
    /// By priority, then in dependency order.
    pub items: Vec<Item>,
}

/// One work item (§11.1).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Item {
    /// Its id: its declaration's textual key, or the orphaned key
    /// (§11.2).
    pub key: TextualKey,

    pub priority: Priority,

    /// Its conditions by name, in §10.1's order.
    pub conditions: Vec<ConditionKind>,

    /// The proven obligations it keeps from being conformant: those
    /// naming its declaration among their blockers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<ObligationKey>,

    /// The items of the declarations its own references in the design
    /// graph, an operation it calls standing for that operation's
    /// transactions and effect sites. Only those before it in dependency
    /// order count, so the relation has no cycle.
    pub depends_on: Vec<TextualKey>,

    /// The files it will likely touch: those of its regions, of its
    /// record's regions, and of its claims' matches outside them, and,
    /// while it has no region, those of its container's regions.
    pub files: Vec<String>,
}

/// How urgent an item is (§11.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    /// It keeps a proven obligation from being conformant.
    Correctness,

    /// It is unbound, unattested, stale, or drifted, and blocks no
    /// proven obligation.
    Structure,

    /// Only reviews are owed.
    Review,

    /// An orphaned key, or facts no proof needs any longer: optional
    /// work.
    Cleanup,

    /// The project's stack cannot realize it (Part VI §18, item 7): the
    /// remedy is a decision about the spec or the stack, not code, so
    /// its work waits for that decision.
    Parked,
}

impl Plan {
    /// The work list in text: each item's priority, key, and
    /// conditions, then what it depends on.
    pub fn render(&self) -> String {
        let mut out = String::new();

        if self.items.is_empty() {
            out.push_str("no work: every declaration is current\n");
        }

        for item in &self.items {
            let priority = serde_json::to_value(item.priority)
                .ok()
                .and_then(|name| name.as_str().map(str::to_string))
                .unwrap_or_default();

            let _ = writeln!(
                out,
                "{priority:<12} {}  {}",
                item.key,
                item.conditions
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            );

            for dependency in &item.depends_on {
                let _ = writeln!(out, "{:13}after {dependency}", "");
            }
        }

        out
    }

    /// One item, by its id.
    pub fn item(&self, key: &TextualKey) -> Option<&Item> {
        self.items.iter().find(|item| &item.key == key)
    }

    /// The items `scope` keeps (§11.5): those of the declarations its
    /// diff changed, a removed declaration's as an orphaned key, and
    /// those anchoring, implementing, or carrying a claim for a fact of
    /// the basis, on either side, of an obligation it moved. An item's
    /// dependencies on items left out are left out with them.
    pub fn scoped(&self, scope: &Scope, derivation: &Derivation) -> Plan {
        let mut owing: BTreeSet<DeclarationKey> = scope.changed.clone();

        for basis_fact in &scope.facts {
            owing.extend(basis_fact.anchors.iter().cloned());

            owing.extend(state::implementers(
                &basis_fact.fact,
                derivation.workspace,
                &derivation.graph,
            ));

            owing.extend(
                completeness::scopes(&basis_fact.fact, derivation.workspace)
                    .into_iter()
                    .map(|(carrier, _)| carrier.into_key()),
            );
        }

        let mut items: Vec<Item> = self
            .items
            .iter()
            .filter(|item| owing.contains(item.key.key()))
            .cloned()
            .collect();

        let kept: BTreeSet<TextualKey> = items.iter().map(|item| item.key.clone()).collect();

        for item in &mut items {
            item.depends_on.retain(|key| kept.contains(key));
        }

        Plan { items }
    }
}

/// The work list of a derivation (§11): every item, by priority, then
/// in dependency order.
pub fn plan(derivation: &Derivation) -> Plan {
    let blocked = blocked(&derivation.state);

    let mut items: Vec<Item> = derivation
        .state
        .declarations
        .iter()
        .filter(|state| !state.is_current())
        .map(|state| {
            let blocks = blocked.get(&state.declaration).cloned().unwrap_or_default();

            Item {
                key: state.declaration.clone(),
                priority: priority(state, &blocks),
                conditions: state.conditions.iter().map(Condition::kind).collect(),
                blocks,
                depends_on: Vec::new(),
                files: files(derivation, state),
            }
        })
        .collect();

    items.extend(derivation.state.orphaned.iter().map(|orphan| {
        let recorded = derivation
            .record(&orphan.key)
            .map(|record| record.regions.as_slice())
            .unwrap_or_default();

        let files: BTreeSet<&str> = orphan
            .regions
            .iter()
            .chain(recorded)
            .map(|region| region.path.as_str())
            .collect();

        Item {
            key: orphan.key.clone(),
            priority: Priority::Cleanup,
            conditions: vec![ConditionKind::Orphaned],
            blocks: Vec::new(),
            depends_on: Vec::new(),
            files: files.into_iter().map(str::to_string).collect(),
        }
    }));

    let order = Order::of(derivation);

    items.sort_by(|a, b| {
        (a.priority, order.position(a.key.key()), &a.key).cmp(&(
            b.priority,
            order.position(b.key.key()),
            &b.key,
        ))
    });

    let keys: BTreeSet<TextualKey> = items.iter().map(|item| item.key.clone()).collect();

    for item in &mut items {
        let declaration = item.key.key();

        if !derivation.partition.members.contains_key(declaration) {
            continue;
        }

        let before = (order.position(declaration), declaration);

        item.depends_on = references(derivation, declaration)
            .into_iter()
            .filter(|referenced| (order.position(referenced), referenced) < before)
            .filter_map(TextualKey::new)
            .filter(|key| keys.contains(key))
            .collect();
    }

    Plan { items }
}

/// An item's priority (§11.3). One that blocks a proven obligation is
/// correctness work. Otherwise its most urgent condition decides.
/// `missing_facts` and `incomplete` always block what relies on them,
/// so they read as structure only for completeness' sake.
pub(crate) fn priority(state: &DeclarationState, blocks: &[ObligationKey]) -> Priority {
    if state.has(ConditionKind::Unrealizable) {
        return Priority::Parked;
    }

    if !blocks.is_empty() {
        return Priority::Correctness;
    }

    state
        .conditions
        .iter()
        .map(|condition| match condition.kind() {
            ConditionKind::Unbound
            | ConditionKind::Unattested
            | ConditionKind::Stale
            | ConditionKind::Drifted
            | ConditionKind::MissingFacts
            | ConditionKind::Incomplete => Priority::Structure,

            ConditionKind::Unreviewed | ConditionKind::Disputed => Priority::Review,

            ConditionKind::Orphaned | ConditionKind::Surplus => Priority::Cleanup,

            ConditionKind::Unrealizable => Priority::Parked,
        })
        .min()
        .unwrap_or(Priority::Cleanup)
}

/// The proven obligations each declaration keeps from being conformant.
pub(crate) fn blocked(state: &State) -> BTreeMap<TextualKey, Vec<ObligationKey>> {
    let mut blocked: BTreeMap<TextualKey, BTreeSet<ObligationKey>> = BTreeMap::new();

    for conformance in &state.obligations {
        for blocker in &conformance.blockers {
            blocked
                .entry(blocker.declaration.clone())
                .or_default()
                .insert(conformance.obligation.clone());
        }
    }

    blocked
        .into_iter()
        .map(|(declaration, obligations)| (declaration, obligations.into_iter().collect()))
        .collect()
}

/// The files an item will likely touch.
fn files(derivation: &Derivation, state: &DeclarationState) -> Vec<String> {
    let mut files: BTreeSet<&str> = state
        .regions
        .iter()
        .map(|region| region.path.as_str())
        .collect();

    if let Some(record) = derivation.record(&state.declaration) {
        files.extend(record.regions.iter().map(|region| region.path.as_str()));
    }

    for condition in &state.conditions {
        let Condition::Incomplete { scopes } = condition else {
            continue;
        };

        for scope in scopes {
            if let ScopeFailure::Unheld { claims } = &scope.failure {
                for claim in claims {
                    files.extend(claim.matches.iter().map(|found| found.path.as_str()));
                }
            }
        }
    }

    if state.regions.is_empty()
        && let Some(container) = container(state.declaration.key())
    {
        files.extend(
            derivation
                .scan
                .regions
                .iter()
                .filter(|region| region.key.key() == &container)
                .map(|region| region.path.as_str()),
        );
    }

    files.into_iter().map(str::to_string).collect()
}

/// The declaration whose code an unbound declaration's code most likely
/// goes into: a transaction's or effect site's program, a program's or
/// input's operation, a data object's or outbox's data model, and a
/// transition's state machine.
fn container(declaration: &DeclarationKey) -> Option<DeclarationKey> {
    match declaration {
        DeclarationKey::Transaction { operation, .. }
        | DeclarationKey::EffectSite { operation, .. } => {
            Some(DeclarationKey::Program(operation.clone()))
        }

        DeclarationKey::Program(operation) | DeclarationKey::Input { operation, .. } => {
            Some(DeclarationKey::Operation(operation.clone()))
        }

        DeclarationKey::DataObject { data_model, .. }
        | DeclarationKey::Outbox { data_model, .. } => {
            Some(DeclarationKey::DataModel(data_model.clone()))
        }

        DeclarationKey::Transition { machine, .. } => {
            Some(DeclarationKey::StateMachine(machine.clone()))
        }

        DeclarationKey::Service(_)
        | DeclarationKey::Schema(_)
        | DeclarationKey::DataModel(_)
        | DeclarationKey::Topic(_)
        | DeclarationKey::StateMachine(_)
        | DeclarationKey::Operation(_)
        | DeclarationKey::Requirement(_)
        | DeclarationKey::TopicRuntime(_)
        | DeclarationKey::SubscriptionRuntime { .. }
        | DeclarationKey::OutboxRuntime { .. }
        | DeclarationKey::ExecutionPool(_)
        | DeclarationKey::Router(_)
        | DeclarationKey::StorageLayout(_)
        | DeclarationKey::PromptObligation(_) => None,
    }
}

/// The declarations a declaration's node points to in the design graph,
/// an operation it calls standing for that operation's transactions and
/// effect sites.
fn references(derivation: &Derivation, declaration: &DeclarationKey) -> BTreeSet<DeclarationKey> {
    let graph = &derivation.graph;

    let Some(node) = symbol(declaration).and_then(|symbol| graph.node_id(&symbol)) else {
        return BTreeSet::new();
    };

    let mut referenced = BTreeSet::new();

    for edge in graph.outgoing_of(node) {
        match (edge.kind, &graph.node_at(edge.to).key) {
            (EdgeKind::CallsOperation, SymbolKey::Operation(called)) => {
                referenced.extend(
                    derivation
                        .partition
                        .members
                        .keys()
                        .filter(|member| match member {
                            DeclarationKey::Transaction { operation, .. }
                            | DeclarationKey::EffectSite { operation, .. } => operation == called,

                            _ => false,
                        })
                        .cloned(),
                );
            }

            (_, target) => referenced.extend(member(target)),
        }
    }

    referenced.remove(declaration);

    referenced
}

/// Dependency order (§11.4): by kind, in five groups, then, among
/// transactions, effect sites, and transitions, callees before callers,
/// then by key.
struct Order {
    /// Each operation's and transition's place in call order, callees
    /// first.
    calls: BTreeMap<DeclarationKey, usize>,
}

impl Order {
    /// Call order. An operation calls the operations its effect sites
    /// request and the transitions its transactions apply, and a
    /// transition calls the operations its side effects request. Calls
    /// that form a cycle come in key order, callees of the cycle first.
    fn of(derivation: &Derivation) -> Self {
        let graph = &derivation.graph;
        let mut callees: BTreeMap<DeclarationKey, BTreeSet<DeclarationKey>> = BTreeMap::new();

        for operation in derivation.workspace.operations.keys() {
            callees
                .entry(DeclarationKey::Operation(operation.clone()))
                .or_default();
        }

        for (machine, state_machine) in &derivation.workspace.state_machines {
            for transition in state_machine.transitions.keys() {
                callees
                    .entry(DeclarationKey::Transition {
                        machine: machine.clone(),
                        transition: transition.clone(),
                    })
                    .or_default();
            }
        }

        for node in &graph.nodes {
            let caller = match &node.key {
                SymbolKey::Transaction { operation, .. }
                | SymbolKey::EffectSite { operation, .. } => {
                    DeclarationKey::Operation(operation.clone())
                }

                SymbolKey::Transition {
                    machine,
                    transition,
                } => DeclarationKey::Transition {
                    machine: machine.clone(),
                    transition: transition.clone(),
                },

                _ => continue,
            };

            let Some(id) = graph.node_id(&node.key) else {
                continue;
            };

            for edge in graph.outgoing_of(id) {
                let callee = match (edge.kind, &graph.node_at(edge.to).key) {
                    (EdgeKind::CallsOperation, SymbolKey::Operation(operation)) => {
                        DeclarationKey::Operation(operation.clone())
                    }

                    (
                        EdgeKind::AppliesTransition,
                        SymbolKey::Transition {
                            machine,
                            transition,
                        },
                    ) => DeclarationKey::Transition {
                        machine: machine.clone(),
                        transition: transition.clone(),
                    },

                    _ => continue,
                };

                if callee != caller {
                    callees.entry(caller.clone()).or_default().insert(callee);
                }
            }
        }

        let components = components(&callees);

        let component_of: BTreeMap<&DeclarationKey, usize> = components
            .iter()
            .enumerate()
            .flat_map(|(index, members)| members.iter().map(move |member| (member, index)))
            .collect();

        // Each component's callees outside it, as components.
        let after: Vec<BTreeSet<usize>> = components
            .iter()
            .enumerate()
            .map(|(index, members)| {
                members
                    .iter()
                    .flat_map(|member| &callees[member])
                    .filter_map(|callee| component_of.get(callee).copied())
                    .filter(|callee| *callee != index)
                    .collect()
            })
            .collect();

        // Callees first; among components ready at once, the one holding
        // the least key; within one, by key.
        let mut placed: BTreeSet<usize> = BTreeSet::new();
        let mut calls = BTreeMap::new();

        while placed.len() < components.len() {
            let next = (0..components.len())
                .filter(|index| !placed.contains(index))
                .filter(|index| after[*index].iter().all(|callee| placed.contains(callee)))
                .min_by_key(|index| components[*index].first())
                .expect("the components of a graph form a directed acyclic graph");

            placed.insert(next);

            for member in &components[next] {
                calls.insert(member.clone(), calls.len());
            }
        }

        Self { calls }
    }

    /// A declaration's group, and its place in call order within the
    /// group of transactions, effect sites, and transitions.
    fn position(&self, declaration: &DeclarationKey) -> (u8, usize) {
        let call = |key: &DeclarationKey| self.calls.get(key).copied().unwrap_or(usize::MAX);

        match declaration {
            DeclarationKey::Transaction { operation, .. }
            | DeclarationKey::EffectSite { operation, .. } => {
                (1, call(&DeclarationKey::Operation(operation.clone())))
            }

            DeclarationKey::Transition { .. } => (1, call(declaration)),

            DeclarationKey::Schema(_)
            | DeclarationKey::DataModel(_)
            | DeclarationKey::DataObject { .. }
            | DeclarationKey::Outbox { .. }
            | DeclarationKey::Topic(_)
            | DeclarationKey::StateMachine(_) => (0, 0),

            DeclarationKey::Program(_)
            | DeclarationKey::Input { .. }
            | DeclarationKey::Operation(_) => (2, 0),

            DeclarationKey::TopicRuntime(_)
            | DeclarationKey::SubscriptionRuntime { .. }
            | DeclarationKey::OutboxRuntime { .. }
            | DeclarationKey::ExecutionPool(_)
            | DeclarationKey::Router(_)
            | DeclarationKey::StorageLayout(_) => (3, 0),

            DeclarationKey::Service(_)
            | DeclarationKey::Requirement(_)
            | DeclarationKey::PromptObligation(_) => (4, 0),
        }
    }
}

/// The strongly connected components of a call graph, each a set of
/// nodes calling one another, by Tarjan's algorithm.
fn components(
    callees: &BTreeMap<DeclarationKey, BTreeSet<DeclarationKey>>,
) -> Vec<BTreeSet<DeclarationKey>> {
    struct Walk<'a> {
        callees: &'a BTreeMap<DeclarationKey, BTreeSet<DeclarationKey>>,
        index: BTreeMap<&'a DeclarationKey, usize>,
        low: BTreeMap<&'a DeclarationKey, usize>,
        stack: Vec<&'a DeclarationKey>,
        on_stack: BTreeSet<&'a DeclarationKey>,
        components: Vec<BTreeSet<DeclarationKey>>,
    }

    impl<'a> Walk<'a> {
        fn visit(&mut self, node: &'a DeclarationKey) {
            let index = self.index.len();

            self.index.insert(node, index);
            self.low.insert(node, index);
            self.stack.push(node);
            self.on_stack.insert(node);

            for callee in &self.callees[node] {
                if !self.callees.contains_key(callee) {
                    continue;
                }

                if !self.index.contains_key(callee) {
                    self.visit(callee);

                    let low = self.low[node].min(self.low[callee]);
                    self.low.insert(node, low);
                } else if self.on_stack.contains(callee) {
                    let low = self.low[node].min(self.index[callee]);
                    self.low.insert(node, low);
                }
            }

            if self.low[node] == self.index[node] {
                let mut component = BTreeSet::new();

                while let Some(member) = self.stack.pop() {
                    self.on_stack.remove(member);
                    component.insert(member.clone());

                    if member == node {
                        break;
                    }
                }

                self.components.push(component);
            }
        }
    }

    let mut walk = Walk {
        callees,
        index: BTreeMap::new(),
        low: BTreeMap::new(),
        stack: Vec::new(),
        on_stack: BTreeSet::new(),
        components: Vec::new(),
    };

    for node in callees.keys() {
        if !walk.index.contains_key(node) {
            walk.visit(node);
        }
    }

    walk.components
}

/// What a Part I diff scopes the work list to (§11.5).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Scope {
    /// Where the diff's sides were read from.
    pub base: SideRecord,
    pub head: SideRecord,

    /// The declarations the diff changed: added, removed, or modified.
    pub changed: BTreeSet<DeclarationKey>,

    /// The obligations it reports added, removed, regressed, improved,
    /// or rebased: every one whose verdict or basis it moved. Adding a
    /// requirement changes only the requirement, so the code its new
    /// proof needs is in scope only through its obligation.
    pub moved: BTreeSet<ObligationKey>,

    /// The facts of the moved obligations' bases, on either side, each
    /// with its anchors there: head's for an added obligation, base's
    /// for a removed one.
    #[serde(skip)]
    pub facts: Vec<BasisFact>,
}

impl Scope {
    /// The scope of the change from `base` to `head` (§11.5): from a
    /// base commit to the working tree, for a pull request changing spec
    /// and code together, or across a spec range of two commits. When a
    /// side does not assemble and validate, there are no verdicts to
    /// compare, and only the changed declarations are in scope.
    pub fn between(base: &Side, head: &Side) -> Self {
        let (document, sides) = diff::analyzed(base, head);

        Self::of(&document, sides.as_ref())
    }

    /// The scope of the change from a base commit to the working tree,
    /// whose spec is `head`: the view of a pull request that changes
    /// spec and code together. With `merge_base`, the base is read at
    /// its merge base with the working tree, so what the base's branch
    /// changed since is left out.
    pub fn since(
        project_dir: &Path,
        base: &str,
        merge_base: bool,
        head: &WorkspaceState,
    ) -> Result<Self, String> {
        let head = Side {
            name: SideName::Head,
            source: Source::WorkingTree,
            commit: None,
            merge_base: false,
            workspace: Some(head.clone()),
            realization: Profile::read(project_dir).map_err(|error| error.to_string())?,
        };

        let base = if merge_base {
            side::read_merge_base(project_dir, base, &Source::WorkingTree)
        } else {
            side::read(
                project_dir,
                SideName::Base,
                &Source::Commit(base.to_string()),
            )
        }
        .map_err(|error| error.to_string())?;

        Ok(Self::between(&base, &head))
    }

    /// The scope of a spec range `<a>..<b>`, both ends commits: on a
    /// trunk that carries other changes' leading specs, what one
    /// change's own spec commits owe.
    pub fn range(project_dir: &Path, range: &str) -> Result<Self, String> {
        let (base, head) = range
            .split_once("..")
            .filter(|(base, head)| !base.is_empty() && !head.is_empty() && !head.starts_with('.'))
            .ok_or_else(|| format!("`{range}` is not a spec range `<a>..<b>`"))?;

        let base = side::read(
            project_dir,
            SideName::Base,
            &Source::Commit(base.to_string()),
        )
        .map_err(|error| error.to_string())?;

        let head = side::read(
            project_dir,
            SideName::Head,
            &Source::Commit(head.to_string()),
        )
        .map_err(|error| error.to_string())?;

        Ok(Self::between(&base, &head))
    }

    fn of(document: &DiffDocument, sides: Option<&[Obligations; 2]>) -> Self {
        let changed = document
            .declarations
            .changes
            .iter()
            .map(|change| change.declaration.clone())
            .collect();

        let moved: BTreeSet<ObligationKey> = match &document.obligations {
            ObligationDiff::Available { entries, .. } => entries
                .iter()
                .filter(|entry| {
                    matches!(
                        entry.class,
                        ObligationClass::Added
                            | ObligationClass::Removed
                            | ObligationClass::Regressed
                            | ObligationClass::Improved
                            | ObligationClass::Rebased
                    )
                })
                .map(|entry| entry.key.clone())
                .collect(),

            ObligationDiff::Unavailable { .. } => BTreeSet::new(),
        };

        let facts = sides
            .into_iter()
            .flatten()
            .flat_map(|obligations| moved.iter().filter_map(|key| obligations.get(key)))
            .flat_map(|obligation| obligation.basis.iter().flatten().cloned())
            .collect();

        Self {
            base: document.base.clone(),
            head: document.head.clone(),
            changed,
            moved,
            facts,
        }
    }
}

/// The design graph's node for a declaration, if it has one.
pub(crate) fn symbol(declaration: &DeclarationKey) -> Option<SymbolKey> {
    Some(match declaration.clone() {
        DeclarationKey::Service(id) => SymbolKey::Service(id),
        DeclarationKey::Schema(id) => SymbolKey::Schema(id),
        DeclarationKey::DataModel(id) => SymbolKey::DataModel(id),
        DeclarationKey::DataObject { data_model, object } => {
            SymbolKey::DataObject { data_model, object }
        }
        DeclarationKey::Outbox { data_model, outbox } => SymbolKey::Outbox { data_model, outbox },
        DeclarationKey::Topic(id) => SymbolKey::Topic(id),
        DeclarationKey::StateMachine(id) => SymbolKey::StateMachine(id),
        DeclarationKey::Transition {
            machine,
            transition,
        } => SymbolKey::Transition {
            machine,
            transition,
        },
        DeclarationKey::Operation(id) => SymbolKey::Operation(id),
        DeclarationKey::Input { operation, input } => SymbolKey::Input { operation, input },
        DeclarationKey::Program(operation) => SymbolKey::OperationProgram(operation),
        DeclarationKey::Transaction {
            operation,
            transaction,
        } => SymbolKey::Transaction {
            operation,
            transaction,
        },
        DeclarationKey::EffectSite { operation, effect } => {
            SymbolKey::EffectSite { operation, effect }
        }
        DeclarationKey::TopicRuntime(id) => SymbolKey::TopicRuntime(id),
        DeclarationKey::SubscriptionRuntime { operation, input } => {
            SymbolKey::SubscriptionRuntime { operation, input }
        }
        DeclarationKey::OutboxRuntime { operation, input } => {
            SymbolKey::OutboxRuntime { operation, input }
        }
        DeclarationKey::ExecutionPool(id) => SymbolKey::ExecutionPool(id),
        DeclarationKey::Router(id) => SymbolKey::Router(id),
        DeclarationKey::StorageLayout(id) => SymbolKey::StorageLayout(id),

        // A requirement's node is keyed by its content, and intent has
        // no code.
        DeclarationKey::Requirement(_) | DeclarationKey::PromptObligation(_) => return None,
    })
}

/// The partition member a design graph node is, if it is one.
pub(crate) fn member(symbol: &SymbolKey) -> Option<DeclarationKey> {
    Some(match symbol.clone() {
        SymbolKey::Service(id) => DeclarationKey::Service(id),
        SymbolKey::Schema(id) => DeclarationKey::Schema(id),
        SymbolKey::DataModel(id) => DeclarationKey::DataModel(id),
        SymbolKey::DataObject { data_model, object } => {
            DeclarationKey::DataObject { data_model, object }
        }
        SymbolKey::Outbox { data_model, outbox } => DeclarationKey::Outbox { data_model, outbox },
        SymbolKey::Topic(id) => DeclarationKey::Topic(id),
        SymbolKey::StateMachine(id) => DeclarationKey::StateMachine(id),
        SymbolKey::Transition {
            machine,
            transition,
        } => DeclarationKey::Transition {
            machine,
            transition,
        },
        SymbolKey::Operation(id) => DeclarationKey::Operation(id),
        SymbolKey::OperationProgram(operation) => DeclarationKey::Program(operation),
        SymbolKey::Input { operation, input } => DeclarationKey::Input { operation, input },
        SymbolKey::Transaction {
            operation,
            transaction,
        } => DeclarationKey::Transaction {
            operation,
            transaction,
        },
        SymbolKey::EffectSite { operation, effect } => {
            DeclarationKey::EffectSite { operation, effect }
        }
        SymbolKey::TopicRuntime(id) => DeclarationKey::TopicRuntime(id),
        SymbolKey::SubscriptionRuntime { operation, input } => {
            DeclarationKey::SubscriptionRuntime { operation, input }
        }
        SymbolKey::OutboxRuntime { operation, input } => {
            DeclarationKey::OutboxRuntime { operation, input }
        }
        SymbolKey::ExecutionPool(id) => DeclarationKey::ExecutionPool(id),
        SymbolKey::Router(id) => DeclarationKey::Router(id),
        SymbolKey::StorageLayout(id) => DeclarationKey::StorageLayout(id),

        // Parts of an operation's content, bindings, requirements keyed
        // by content, derived summaries, and intent are no member's
        // node.
        SymbolKey::OperationInterface(_)
        | SymbolKey::OperationRequirements(_)
        | SymbolKey::Binding { .. }
        | SymbolKey::Requirement { .. }
        | SymbolKey::OperationSummary(_)
        | SymbolKey::PromptObligation(_) => return None,
    })
}
