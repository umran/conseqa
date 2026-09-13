//! The paths of an operation program.
//!
//! The program is a structured, acyclic block: every invocation
//! traverses one path through it, taking one arm at each decision and
//! ending at one terminal. Verification analyzes the program path by
//! path — a path is exactly the linear sequence the retired invocation
//! flows were, plus the decisions that selected it — so the replay
//! engine's forward pass applies unchanged, and what a decision rests
//! on is judged where it is taken.
//!
//! A rejectable transaction is a decision too: its execution site
//! forks into the committed continuation, which carries the
//! transaction step and everything it establishes, and the rejected
//! continuation, which carries nothing — no mutation, artifact, or
//! admission survives rejection — and enters the `rejected` block
//! before rejoining the enclosing block if it falls through.

use serde::{Deserialize, Serialize};

use crate::spec::{
    Arm, AsyncJoin, Condition, Derivation, Effect, Id, OperationBlock, OperationStep, ResultArm,
    ResultOutcome, StepLocation, Transaction, TransactionOutcome,
};

/// One path through the program: its linear steps in order and the
/// terminal it reaches.
#[derive(Debug, Clone)]
pub struct Path<'a> {
    pub steps: Vec<PathStep<'a>>,
    pub terminal: Terminal<'a>,
}

#[derive(Debug, Clone)]
pub enum PathStep<'a> {
    /// The inline transaction declared and executed at this step,
    /// committed on this path.
    Transaction {
        location: StepLocation,
        transaction: &'a Transaction,
    },

    ExecuteEffect {
        location: StepLocation,
        effect_id: &'a Id,
        effect: &'a Effect,
        values: &'a Derivation,
        bind: Option<&'a Id>,
    },

    ExecuteEffectIntent {
        location: StepLocation,
        intent: &'a Id,
        bind: Option<&'a Id>,
    },

    /// An asynchronous launch: the instance is constructed and its
    /// execution initiated here, but control does not wait for it.
    /// Only the handle is bound.
    ExecuteEffectAsync {
        location: StepLocation,
        handle: &'a Id,
        effect_id: &'a Id,
        effect: &'a Effect,
        values: &'a Derivation,
    },

    /// An asynchronous launch of an established intent's captured
    /// instance.
    ExecuteEffectIntentAsync {
        location: StepLocation,
        intent: &'a Id,
        handle: &'a Id,
    },

    /// The all-completion barrier: continuation follows completion of
    /// every referenced execution, and each entry may bind its
    /// effect's result.
    JoinAll {
        location: StepLocation,
        handles: &'a [AsyncJoin],
    },

    /// The first-completion barrier: continuation follows the first
    /// referenced execution to complete.
    Race {
        location: StepLocation,
        handles: &'a [Id],
        bind: Option<&'a Id>,
    },

    /// The path takes one arm of a decision here.
    Decision {
        location: StepLocation,
        decision: Decision<'a>,
    },
}

#[derive(Debug, Clone)]
pub enum Decision<'a> {
    Match {
        result: &'a Id,
        arm: ResultArm,
    },

    Branch {
        condition: &'a Condition,
        arm: Arm,
    },

    /// A rejectable transaction's outcome. On the committed
    /// continuation the decision follows the transaction step, since
    /// the execution is what decides it; on the rejected continuation
    /// no transaction step precedes it.
    Transaction {
        transaction: &'a Id,
        outcome: TransactionOutcome,
    },
}

#[derive(Debug, Clone)]
pub enum Terminal<'a> {
    Return {
        location: StepLocation,
        request: &'a Id,
        outcome: &'a ResultOutcome,
    },

    Complete {
        location: StepLocation,
    },

    /// The block fell through its last step with no terminal.
    /// Validation rejects this; verification stays conservative on a
    /// model it was not promised.
    None,
}

impl<'a> Path<'a> {
    /// The path as a proof or obstacle names it: the decisions taken.
    pub fn reference(&self) -> PathRef {
        PathRef {
            decisions: self
                .steps
                .iter()
                .filter_map(|step| match step {
                    PathStep::Decision { location, decision } => Some(match decision {
                        Decision::Match { result, arm } => DecisionTaken::Match {
                            location: location.clone(),
                            result: (*result).clone(),
                            arm: arm.clone(),
                        },

                        Decision::Branch { arm, .. } => DecisionTaken::Branch {
                            location: location.clone(),
                            arm: arm.clone(),
                        },

                        Decision::Transaction {
                            transaction,
                            outcome,
                        } => DecisionTaken::Transaction {
                            location: location.clone(),
                            transaction: (*transaction).clone(),
                            outcome: *outcome,
                        },
                    }),

                    _ => None,
                })
                .collect(),
        }
    }

    /// Whether an invocation triggered by `input` can take this path:
    /// it ends at `complete`, or at a `return` for that input. A path
    /// returning another request input's result is not one an
    /// invocation of `input` completes. An unterminated path is
    /// admitted conservatively, so its work is still analyzed.
    pub fn admitted_for(&self, input: &Id) -> bool {
        match &self.terminal {
            Terminal::Return { request, .. } => *request == input,
            Terminal::Complete { .. } | Terminal::None => true,
        }
    }

    /// Whether the path ends at a `return` for `input`.
    pub fn returns_for(&self, input: &Id) -> Option<&'a ResultOutcome> {
        match &self.terminal {
            Terminal::Return {
                request, outcome, ..
            } if *request == input => Some(outcome),

            _ => None,
        }
    }
}

/// The identity of a path within its program: the arm taken at each
/// decision along it, in order. Empty for a program with no decisions.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathRef {
    pub decisions: Vec<DecisionTaken>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DecisionTaken {
    Match {
        location: StepLocation,
        result: Id,
        arm: ResultArm,
    },

    Branch {
        location: StepLocation,
        arm: Arm,
    },

    Transaction {
        location: StepLocation,
        transaction: Id,
        outcome: TransactionOutcome,
    },
}

/// Every path through the program, in program order: the first arm of
/// each decision before its second, and a transaction's committed
/// continuation before its rejected one.
pub fn paths(program: &OperationBlock) -> Vec<Path<'_>> {
    let mut out = Vec::new();

    let open = walk(
        Vec::new(),
        &program.steps,
        &StepLocation::root(),
        None,
        &mut out,
    );

    for steps in open {
        out.push(Path {
            steps,
            terminal: Terminal::None,
        });
    }

    out
}

/// Extends `prefix` through `steps`, emitting every terminated path and
/// returning the prefixes that fall through the block's end.
fn walk<'a>(
    prefix: Vec<PathStep<'a>>,
    steps: &'a [OperationStep],
    parent: &StepLocation,
    arm: Option<Arm>,
    out: &mut Vec<Path<'a>>,
) -> Vec<Vec<PathStep<'a>>> {
    let mut open = vec![prefix];

    for (index, step) in steps.iter().enumerate() {
        let location = OperationBlock::location(parent, arm.clone(), index);

        let mut next = Vec::new();

        for mut prefix in open {
            match step {
                OperationStep::Transaction(execute) => {
                    let transaction = &execute.transaction;

                    match &execute.rejected {
                        None => {
                            prefix.push(PathStep::Transaction {
                                location: location.clone(),
                                transaction,
                            });

                            next.push(prefix);
                        }

                        Some(rejected) => {
                            let mut committed = prefix.clone();

                            committed.push(PathStep::Transaction {
                                location: location.clone(),
                                transaction,
                            });

                            committed.push(PathStep::Decision {
                                location: location.clone(),
                                decision: Decision::Transaction {
                                    transaction: &transaction.id,
                                    outcome: TransactionOutcome::Committed,
                                },
                            });

                            next.push(committed);

                            let mut refused = prefix;

                            refused.push(PathStep::Decision {
                                location: location.clone(),
                                decision: Decision::Transaction {
                                    transaction: &transaction.id,
                                    outcome: TransactionOutcome::Rejected,
                                },
                            });

                            next.extend(walk(
                                refused,
                                &rejected.steps,
                                &location,
                                Some(Arm::Rejected),
                                out,
                            ));
                        }
                    }
                }

                OperationStep::ExecuteEffect(step) => {
                    prefix.push(PathStep::ExecuteEffect {
                        location: location.clone(),
                        effect_id: &step.effect_id,
                        effect: &step.effect,
                        values: &step.values,
                        bind: step.bind.as_ref(),
                    });

                    next.push(prefix);
                }

                OperationStep::ExecuteEffectIntent(step) => {
                    prefix.push(PathStep::ExecuteEffectIntent {
                        location: location.clone(),
                        intent: &step.intent,
                        bind: step.bind.as_ref(),
                    });

                    next.push(prefix);
                }

                OperationStep::ExecuteEffectAsync(step) => {
                    prefix.push(PathStep::ExecuteEffectAsync {
                        location: location.clone(),
                        handle: &step.handle,
                        effect_id: &step.effect_id,
                        effect: &step.effect,
                        values: &step.values,
                    });

                    next.push(prefix);
                }

                OperationStep::ExecuteEffectIntentAsync(step) => {
                    prefix.push(PathStep::ExecuteEffectIntentAsync {
                        location: location.clone(),
                        intent: &step.intent,
                        handle: &step.handle,
                    });

                    next.push(prefix);
                }

                OperationStep::JoinAll(step) => {
                    prefix.push(PathStep::JoinAll {
                        location: location.clone(),
                        handles: &step.handles,
                    });

                    next.push(prefix);
                }

                OperationStep::Race(step) => {
                    prefix.push(PathStep::Race {
                        location: location.clone(),
                        handles: &step.handles,
                        bind: step.bind.as_ref(),
                    });

                    next.push(prefix);
                }

                OperationStep::MatchResult(step) => {
                    let arms = std::iter::once((ResultArm::Ok, &step.ok)).chain(
                        step.errors
                            .iter()
                            .map(|(error, block)| (ResultArm::err(error), block)),
                    );

                    for (result_arm, block) in arms {
                        let mut taken = prefix.clone();

                        taken.push(PathStep::Decision {
                            location: location.clone(),
                            decision: Decision::Match {
                                result: &step.result,
                                arm: result_arm.clone(),
                            },
                        });

                        next.extend(walk(
                            taken,
                            &block.steps,
                            &location,
                            Some(Arm::of(&result_arm)),
                            out,
                        ));
                    }
                }

                OperationStep::Branch(step) => {
                    let mut then = prefix.clone();

                    then.push(PathStep::Decision {
                        location: location.clone(),
                        decision: Decision::Branch {
                            condition: &step.condition,
                            arm: Arm::Then,
                        },
                    });

                    next.extend(walk(
                        then,
                        &step.then.steps,
                        &location,
                        Some(Arm::Then),
                        out,
                    ));

                    let mut otherwise = prefix;

                    otherwise.push(PathStep::Decision {
                        location: location.clone(),
                        decision: Decision::Branch {
                            condition: &step.condition,
                            arm: Arm::Otherwise,
                        },
                    });

                    match &step.otherwise {
                        Some(block) => next.extend(walk(
                            otherwise,
                            &block.steps,
                            &location,
                            Some(Arm::Otherwise),
                            out,
                        )),

                        None => next.push(otherwise),
                    }
                }

                OperationStep::Return(step) => out.push(Path {
                    steps: prefix,
                    terminal: Terminal::Return {
                        location: location.clone(),
                        request: &step.request,
                        outcome: &step.outcome,
                    },
                }),

                OperationStep::Complete => out.push(Path {
                    steps: prefix,
                    terminal: Terminal::Complete {
                        location: location.clone(),
                    },
                }),
            }
        }

        open = next;

        if open.is_empty() {
            // Everything terminated; whatever follows is unreachable.
            return open;
        }
    }

    open
}
