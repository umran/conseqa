use std::fmt;

use serde::{Deserialize, Serialize};

use crate::spec::{Id, ResultVariant};

use super::{Derivation, Effect, SelectorValue, Transaction, ValueRef, WriteOutboxEffect};

/// The one explicit control structure of an operation.
///
/// A program is a block of steps executed in order. Decisions —
/// `MatchResult` over a synchronous effect result, `Branch` over an
/// ordinary predicate — nest further blocks, and every reachable path
/// ends at an explicit terminal: `Return` for a request-driven
/// execution, `Complete` for one that returns nothing. The structure is
/// acyclic by construction: loops are deliberately deferred.
///
/// Control flow describes causality. It is not a durable workflow, a
/// checkpoint, or a program counter; a retry traverses the same
/// declared control from the start, and what it re-encounters is
/// judged by the transaction and effect replay rules.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationBlock {
    pub steps: Vec<OperationStep>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OperationStep {
    /// Declares and executes one atomic transaction at this point of
    /// the program, or resolves its prior keyed commit.
    Transaction(Transaction),

    /// Declares one logical effect contract and one concrete execution
    /// site: reaching the step constructs the instance and executes it.
    ExecuteEffect(ExecuteEffect),

    /// Constructs and initiates the same logical effect instance an
    /// `execute_effect` would, without waiting for the execution to
    /// complete. Binds only an operation-local asynchronous handle.
    ExecuteEffectAsync(ExecuteEffectAsync),

    /// Executes an already-established effect instance.
    ExecuteEffectIntent(ExecuteEffectIntent),

    /// Initiates execution of the exact instance captured by an
    /// established intent, without waiting for it to complete. Binds
    /// only an operation-local asynchronous handle.
    ExecuteEffectIntentAsync(ExecuteEffectIntentAsync),

    /// Waits for every referenced asynchronous execution to complete,
    /// optionally binding each result-bearing effect's result.
    JoinAll(JoinAll),

    /// Waits for the first referenced asynchronous execution to
    /// complete — first completion, not first success.
    Race(Race),

    /// Destructures a bound effect result into its `ok` and `err`
    /// arms.
    MatchResult(MatchResult),

    /// An ordinary control decision over modeled values.
    Branch(Branch),

    /// Terminates a request-driven execution with the request's
    /// declared result.
    Return(Return),

    /// Terminates an execution that returns nothing, as is natural for
    /// a subscription-driven operation.
    Complete,
}

/// Declares one logical effect contract and one concrete execution
/// site.
///
/// `effect_id` identifies the inline effect occurrence itself — for
/// value lineage, conformance, diagnostics, and proof evidence. It is
/// not a lookup into an operation-level effect registry, and it must
/// be unique within the operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecuteEffect {
    /// Stable identity of this inline effect occurrence.
    pub effect_id: Id,

    /// The logical effect contract declared at this site.
    pub effect: Effect,

    /// Provenance of the complete outgoing logical effect instance
    /// constructed and executed by this step.
    pub values: Derivation,

    /// Binds the effect's synchronous result, when the effect contract
    /// is result-bearing and the result is not deliberately ignored.
    ///
    /// The result type is inferred from the contract: a request effect
    /// inherits its target input's result, an external effect declares
    /// its own, a publication has none and cannot bind one. The bound
    /// id is an operation-local observation, not a transaction
    /// artifact.
    pub bind: Option<Id>,
}

/// Declares one logical effect contract and one asynchronous execution
/// site.
///
/// Reaching the step evaluates `values`, constructs the concrete
/// instance — fully determined at launch — and initiates its
/// execution; control proceeds without waiting for completion. Only
/// `start(effect) < start(next)` is established; no completion
/// relationship exists until a synchronization step declares one.
///
/// The step binds no result: at launch the synchronous result need not
/// exist yet. Results become available only through `join_all` or
/// `race`. `effect_id` and `handle` are independently meaningful:
/// the effect ID is the stable identity of the inline occurrence, the
/// handle an invocation-local synchronization artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecuteEffectAsync {
    /// Operation-local handle of this asynchronous execution
    /// occurrence, consumed only by synchronization steps.
    pub handle: Id,

    /// Stable identity of this inline effect occurrence.
    pub effect_id: Id,

    /// The logical effect contract declared at this site.
    pub effect: Effect,

    /// Provenance of the complete outgoing logical effect instance,
    /// evaluated when the launch is reached.
    pub values: Derivation,
}

/// Executes an already-established effect instance; the values were
/// fixed at establishment, so no derivation is declared here. The
/// result binding follows the underlying effect contract exactly as
/// for a direct execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecuteEffectIntent {
    /// The definitely available intent binding whose captured instance
    /// this step executes.
    pub intent: Id,

    /// Binds the effect's synchronous result, allowed only when the
    /// underlying effect contract is result-bearing.
    pub bind: Option<Id>,
}

/// Initiates execution of the exact effect instance captured by an
/// established intent, without waiting for it to complete.
///
/// An additional execution authority for an `EffectIntent`: the
/// captured instance is resolved and initiated, and control proceeds.
/// It alters no intent durability, replay, or recovery semantics, and
/// binds no result — results surface only at synchronization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecuteEffectIntentAsync {
    /// The definitely available intent binding whose captured instance
    /// this step initiates.
    pub intent: Id,

    /// Operation-local handle of this asynchronous execution
    /// occurrence.
    pub handle: Id,
}

/// A synchronization barrier over asynchronous executions: the
/// continuation follows completion of every referenced execution.
///
/// No relative completion order among the joined executions is
/// established, and the barrier does not serialize them. Each entry
/// may independently bind its effect's ordinary `Result`; no aggregate
/// value is constructed. An `Err` result is still a completed
/// interaction, so the barrier never short-circuits on one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JoinAll {
    pub handles: Vec<AsyncJoin>,
}

/// One joined handle and, when the underlying effect is result-bearing
/// and the result is not deliberately ignored, the binding under which
/// its ordinary `Result` becomes available after the barrier. The
/// result type is inferred from the underlying effect contract and
/// never restated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AsyncJoin {
    pub handle: Id,
    pub bind: Option<Id>,
}

/// A first-completion barrier over asynchronous executions: the
/// continuation follows the first referenced execution to complete —
/// first completion, not first success, so a winning `Err` is what a
/// result-binding race observes.
///
/// Racing establishes nothing about the losing executions: they are
/// not cancelled, remain part of the operation's side-effect blast
/// radius, and may still be joined later — the race consumes no
/// handle. Where `bind` is present, every candidate must expose the
/// same logical result contract, and the binding carries the winner's
/// exact result; the winning handle's identity is not exposed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Race {
    pub handles: Vec<Id>,
    pub bind: Option<Id>,
}

/// Explicit, exhaustive, mutually exclusive destructuring of a bound
/// result.
///
/// Inside `ok`, `effect_result_ok:<result>` is available and the `err`
/// payload is not; inside `err`, the reverse. Neither payload survives
/// the join after the match: data that must be generally available
/// later is exported through a transaction artifact instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MatchResult {
    pub result: Id,
    pub ok: OperationBlock,
    pub err: OperationBlock,
}

/// An ordinary control decision. `MatchResult` destructures a result;
/// `Branch` evaluates a predicate over modeled values. Neither is
/// overloaded to do the other's job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Branch {
    pub condition: Condition,
    pub then: OperationBlock,

    /// Absent, the branch falls through to the following step when the
    /// condition does not hold.
    pub otherwise: Option<OperationBlock>,
}

/// The predicate of a `Branch`.
///
/// The vocabulary is deliberately small and structurally exposes every
/// value the decision depends on, so replay analysis can judge whether
/// a retry takes the same arm without an expression language: `eq`,
/// `present`, `and`, and `not` are deterministic functions of their
/// references, and `unspecified` states that the model provides no
/// fact about how the decision is made.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Condition {
    /// The model provides no fact about the decision; which arm an
    /// invocation takes is unknown to the analyzer.
    Unspecified,

    /// Equality of a modeled value against a literal or another modeled
    /// value. `equals` accepts the selector-value surface: a map is a
    /// value reference, a scalar is a literal. Does not hold when
    /// either operand evaluates absent — absence is not a comparable
    /// value; presence is queried only through `present`.
    Eq {
        value: ValueRef,
        equals: SelectorValue,
    },

    And {
        conditions: Vec<Condition>,
    },

    Not {
        condition: Box<Condition>,
    },

    /// Holds iff the referenced path resolves to a value: absent iff
    /// any optional segment required to resolve it is absent.
    /// Presence is part of the logical value Conseqa observes, so
    /// attempts observing replay-equivalent roots agree on it. There
    /// is no `absent` kind; `not { present … }` composes.
    Present {
        value: ValueRef,
    },
}

impl Condition {
    /// Every value reference the decision observes.
    pub fn roots(&self) -> Vec<&ValueRef> {
        match self {
            Self::Unspecified => Vec::new(),

            Self::Eq { value, equals } => {
                let mut roots = vec![value];

                if let SelectorValue::Value(reference) = equals {
                    roots.push(reference);
                }

                roots
            }

            Self::And { conditions } => conditions.iter().flat_map(Condition::roots).collect(),

            Self::Not { condition } => condition.roots(),

            Self::Present { value } => vec![value],
        }
    }

    /// Whether the decision is a deterministic function of its roots.
    /// Only `unspecified` is not.
    pub fn is_deterministic(&self) -> bool {
        match self {
            Self::Unspecified => false,
            Self::Eq { .. } | Self::Present { .. } => true,
            Self::And { conditions } => conditions.iter().all(Condition::is_deterministic),
            Self::Not { condition } => condition.is_deterministic(),
        }
    }
}

/// Terminates a request-driven execution by constructing the request
/// input's declared result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Return {
    /// The operation-owned request input whose result is returned.
    pub request: Id,

    pub outcome: ResultOutcome,
}

/// Which variant a `Return` constructs, and the provenance of its
/// payload: `Ok` builds the request's `ok` schema, `Err` its `err`
/// schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResultOutcome {
    Ok { values: Derivation },
    Err { values: Derivation },
}

impl ResultOutcome {
    pub fn variant(&self) -> ResultVariant {
        match self {
            Self::Ok { .. } => ResultVariant::Ok,
            Self::Err { .. } => ResultVariant::Err,
        }
    }

    pub fn values(&self) -> &Derivation {
        match self {
            Self::Ok { values } | Self::Err { values } => values,
        }
    }
}

/// The arm of a decision a nested block belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Arm {
    Ok,
    Err,
    Then,
    Otherwise,
}

impl Arm {
    pub fn of(variant: ResultVariant) -> Self {
        match variant {
            ResultVariant::Ok => Self::Ok,
            ResultVariant::Err => Self::Err,
        }
    }
}

impl fmt::Display for Arm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Ok => "ok",
            Self::Err => "err",
            Self::Then => "then",
            Self::Otherwise => "otherwise",
        })
    }
}

/// Where a step sits in a program: one hop per nesting level, each the
/// step's position in its block and, for every level but the last, the
/// arm entered beneath it.
///
/// Steps carry no ids of their own, so this is how diagnostics and
/// reports name them. It renders one-based, as `3.ok.1`: the first step
/// of the `ok` arm of the third top-level step.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StepLocation(pub Vec<StepHop>);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepHop {
    pub step: usize,
    pub arm: Option<Arm>,
}

impl StepLocation {
    pub fn root() -> Self {
        Self(Vec::new())
    }

    /// The location of step `step` in the block this location's step
    /// opens through `arm`, or in the program itself at the root.
    fn descend(&self, arm: Option<Arm>, step: usize) -> Self {
        let mut hops = self.0.clone();

        if let Some(last) = hops.last_mut() {
            last.arm = arm;
        }

        hops.push(StepHop { step, arm: None });

        Self(hops)
    }
}

impl fmt::Display for StepLocation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (position, hop) in self.0.iter().enumerate() {
            if position > 0 {
                f.write_str(".")?;
            }

            write!(f, "{}", hop.step + 1)?;

            if let Some(arm) = hop.arm {
                write!(f, ".{arm}")?;
            }
        }

        Ok(())
    }
}

impl OperationBlock {
    /// Every step of the program with its location, depth first in
    /// program order.
    pub fn steps_with_locations(&self) -> Vec<(StepLocation, &OperationStep)> {
        let mut out = Vec::new();

        self.collect(&StepLocation::root(), None, &mut out);

        out
    }

    fn collect<'a>(
        &'a self,
        parent: &StepLocation,
        arm: Option<Arm>,
        out: &mut Vec<(StepLocation, &'a OperationStep)>,
    ) {
        for (index, step) in self.steps.iter().enumerate() {
            let location = parent.descend(arm, index);

            out.push((location.clone(), step));

            match step {
                OperationStep::MatchResult(matched) => {
                    matched.ok.collect(&location, Some(Arm::Ok), out);
                    matched.err.collect(&location, Some(Arm::Err), out);
                }

                OperationStep::Branch(branch) => {
                    branch.then.collect(&location, Some(Arm::Then), out);

                    if let Some(otherwise) = &branch.otherwise {
                        otherwise.collect(&location, Some(Arm::Otherwise), out);
                    }
                }

                _ => {}
            }
        }
    }

    /// The location of step `index` of the block that `parent` opens
    /// through `arm`; `parent` is the root for the program's own steps.
    pub fn location(parent: &StepLocation, arm: Option<Arm>, index: usize) -> StepLocation {
        parent.descend(arm, index)
    }

    /// Every inline transaction of the program with its location, in
    /// program order. Derived from the program on demand; the program
    /// remains the source of truth.
    pub fn transactions(&self) -> Vec<(StepLocation, &Transaction)> {
        self.steps_with_locations()
            .into_iter()
            .filter_map(|(location, step)| match step {
                OperationStep::Transaction(transaction) => Some((location, transaction)),
                _ => None,
            })
            .collect()
    }

    /// The inline transaction with the given stable ID, wherever it
    /// sits in the program.
    pub fn transaction(&self, id: &Id) -> Option<&Transaction> {
        self.transactions()
            .into_iter()
            .find_map(|(_, transaction)| (&transaction.id == id).then_some(transaction))
    }

    /// Mutable access to the inline transaction with the given stable
    /// ID.
    pub fn transaction_mut(&mut self, id: &Id) -> Option<&mut Transaction> {
        fn contains(block: &OperationBlock, id: &Id) -> bool {
            block.transaction(id).is_some()
        }

        let position = self.steps.iter().position(|step| match step {
            OperationStep::Transaction(transaction) => &transaction.id == id,
            OperationStep::MatchResult(matched) => {
                contains(&matched.ok, id) || contains(&matched.err, id)
            }
            OperationStep::Branch(branch) => {
                contains(&branch.then, id)
                    || branch
                        .otherwise
                        .as_ref()
                        .is_some_and(|block| contains(block, id))
            }
            _ => false,
        })?;

        match &mut self.steps[position] {
            OperationStep::Transaction(transaction) => Some(transaction),

            OperationStep::MatchResult(matched) => {
                if contains(&matched.ok, id) {
                    matched.ok.transaction_mut(id)
                } else {
                    matched.err.transaction_mut(id)
                }
            }

            OperationStep::Branch(branch) => {
                if contains(&branch.then, id) {
                    branch.then.transaction_mut(id)
                } else {
                    branch.otherwise.as_mut()?.transaction_mut(id)
                }
            }

            _ => None,
        }
    }

    /// Every operation-owned inline effect declaration of the program,
    /// with its id: direct execution sites and intent establishment
    /// sites, in program order. Transition-owned effects are not
    /// operation declarations and are not listed.
    pub fn effect_declarations(&self) -> Vec<(&Id, &Effect)> {
        let mut out = Vec::new();

        for (_, step) in self.steps_with_locations() {
            match step {
                OperationStep::ExecuteEffect(step) => {
                    out.push((&step.effect_id, &step.effect));
                }

                OperationStep::ExecuteEffectAsync(step) => {
                    out.push((&step.effect_id, &step.effect));
                }

                OperationStep::Transaction(transaction) => {
                    for inner in &transaction.steps {
                        if let super::TransactionStep::EstablishEffectIntent(establish) = inner {
                            out.push((&establish.effect_id, &establish.effect));
                        }
                    }
                }

                _ => {}
            }
        }

        out
    }

    /// Every transactional outbox-write declaration of the program,
    /// with the inline transaction that carries it, in program order.
    ///
    /// Kept apart from [`effect_declarations`](Self::effect_declarations)
    /// because a `write_outbox` step declares the specific
    /// `OutboxWriteEffect` contract rather than the general effect
    /// enum, and its one legal execution context — the containing
    /// transaction — is part of what a consumer needs to know.
    pub fn outbox_write_declarations(&self) -> Vec<(&Id, &WriteOutboxEffect)> {
        let mut out = Vec::new();

        for (_, transaction) in self.transactions() {
            for inner in &transaction.steps {
                if let super::TransactionStep::WriteOutbox(write) = inner {
                    out.push((&transaction.id, write));
                }
            }
        }

        out
    }
}
