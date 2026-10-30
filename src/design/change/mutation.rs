//! Model mutations: the fixed catalogue the explanation corpus applies
//! (§19.2 of the spec change analysis revision), and the fact
//! withdrawals a work item's load-bearing check applies (§12.2 of the
//! conformance revision).
//!
//! The corpus applies every mutation at every site of every fixture
//! and diffs the result against the fixture: every changed verdict must
//! be explained by a changed declaration (§12.2), or the extraction
//! failed to anchor something the verifier read. A work item states
//! what deleting a detail would cost by withdrawing the fact the detail
//! provides and verifying the result. Both read one definition of what
//! withdrawing a fact means, which is why the catalogue is library code
//! and not a test's.
//!
//! A mutation names its site by ids, never by a path into the files, so
//! it reads as the change it is. It edits the authoring workspace, so
//! the result goes through the same partition, assembly, and validation
//! as any edit. A mutation can leave a model that does not validate;
//! its diff then has no obligation diff, and the corpus skips it.

use std::collections::BTreeSet;
use std::fmt;

use crate::analyzer::basis::{
    self, Channel, Fact, Guarantee, Mechanism, ObligationProperty, ObligationSubject,
    RequirementFamily, RequirementIdentity, Site,
};
use crate::design::workspace::WorkspaceState;
use crate::spec::{
    Delete, DeliverySemantics, Effect, ExecuteTransaction, ExternalIdempotency,
    ExternalResultReplay, Id, IdempotencyGuarantee, LockMode, MessageIdentity, ObjectSelector,
    OperationBlock, OperationStep, RetrySemantics, SelectorPredicate, Transaction,
    TransactionIsolation, TransactionRequirements, TransactionStep, TransitionSideEffect, Update,
    ValueSource,
};

/// One mutation at one site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mutation {
    /// Removes every lock step of a transaction taking `mode` over
    /// `selector`: what withdrawing one `lock` fact means.
    RemoveLock {
        operation: Id,
        transaction: Id,
        selector: ObjectSelector,
        mode: LockMode,
    },

    /// Sets a transaction's isolation: to `unspecified` when dropping
    /// it, one level down when weakening it.
    SetIsolation {
        operation: Id,
        transaction: Id,
        isolation: TransactionIsolation,
    },

    /// Turns every `compare_and_set` of a transaction on `object` into
    /// an `update`, dropping its comparison, and drops the rejection
    /// block the transaction no longer needs.
    CompareAndSetToUpdate {
        operation: Id,
        transaction: Id,
        object: Id,
    },

    /// Adds a transaction right after `after` that deletes what
    /// `selector` selects: a deleter of a versioned object.
    AddDeleter {
        operation: Id,
        after: Id,
        data_model: Id,
        selector: ObjectSelector,
    },

    /// Sets a subscription's delivery.
    SetDelivery {
        operation: Id,
        input: Id,
        delivery: DeliverySemantics,
    },

    /// Removes a transaction's keyed commit deduplication.
    RemoveDeduplicatedBy { operation: Id, transaction: Id },

    /// Sets an external effect's idempotency.
    SetExternalIdempotency {
        operation: Id,
        effect: Id,
        idempotency: ExternalIdempotency,
    },

    /// Sets an external effect's result replay.
    SetExternalResultReplay {
        operation: Id,
        effect: Id,
        result_replay: ExternalResultReplay,
    },

    /// Sets a request effect's retry to `unspecified`.
    UnspecifyRetry { site: Site },

    /// Sets a channel's message identity to `unspecified`.
    UnspecifyMessageIdentity { channel: Channel },

    /// Widens one step's selector to every instance of its object.
    WidenSelector {
        operation: Id,
        transaction: Id,
        step: usize,
    },

    /// Removes one declared requirement.
    RemoveRequirement { requirement: RequirementIdentity },
}

impl fmt::Display for Mutation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RemoveLock {
                operation,
                transaction,
                selector,
                mode,
            } => write!(
                f,
                "remove the {} lock on {} from {operation} / {transaction}",
                snake(mode),
                selector.object
            ),

            Self::SetIsolation {
                operation,
                transaction,
                isolation,
            } => write!(
                f,
                "set the isolation of {operation} / {transaction} to {}",
                snake(isolation)
            ),

            Self::CompareAndSetToUpdate {
                operation,
                transaction,
                object,
            } => write!(
                f,
                "turn the compare_and_set of {object} in {operation} / {transaction} into an update"
            ),

            Self::AddDeleter {
                operation,
                after,
                selector,
                ..
            } => write!(
                f,
                "add a transaction deleting {} after {operation} / {after}",
                selector.object
            ),

            Self::SetDelivery {
                operation,
                input,
                delivery,
            } => write!(
                f,
                "set the delivery of {operation} / {input} to {}",
                snake(delivery)
            ),

            Self::RemoveDeduplicatedBy {
                operation,
                transaction,
            } => write!(
                f,
                "remove the deduplicated_by of {operation} / {transaction}"
            ),

            Self::SetExternalIdempotency {
                operation,
                effect,
                idempotency,
            } => write!(
                f,
                "set the idempotency of {operation} / {effect} to {}",
                snake(idempotency)
            ),

            Self::SetExternalResultReplay {
                operation,
                effect,
                result_replay,
            } => write!(
                f,
                "set the result replay of {operation} / {effect} to {}",
                snake(result_replay)
            ),

            Self::UnspecifyRetry { site } => write!(f, "set the retry of {site} to unspecified"),

            Self::UnspecifyMessageIdentity { channel } => {
                write!(f, "set the message identity of {channel} to unspecified")
            }

            Self::WidenSelector {
                operation,
                transaction,
                step,
            } => write!(
                f,
                "widen the selector of {operation} / {transaction} steps[{step}] to all"
            ),

            Self::RemoveRequirement { requirement } => {
                write!(f, "remove the requirement {requirement}")
            }
        }
    }
}

impl Mutation {
    /// The workspace with the mutation applied, or `None` when the
    /// workspace has no such site, or the mutation would change
    /// nothing there.
    pub fn apply(&self, workspace: &WorkspaceState) -> Option<WorkspaceState> {
        let mut mutated = workspace.clone();

        let changed = match self {
            Self::RemoveLock {
                operation,
                transaction,
                selector,
                mode,
            } => with_transaction(&mut mutated, operation, transaction, |execute| {
                let steps = &mut execute.transaction.steps;
                let before = steps.len();

                steps.retain(|step| {
                    !matches!(
                        step,
                        TransactionStep::Lock(lock) if lock.target == *selector && lock.mode == *mode
                    )
                });

                steps.len() < before
            }),

            Self::SetIsolation {
                operation,
                transaction,
                isolation,
            } => with_transaction(&mut mutated, operation, transaction, |execute| {
                let changed = execute.transaction.isolation != *isolation;

                execute.transaction.isolation = *isolation;

                changed
            }),

            Self::CompareAndSetToUpdate {
                operation,
                transaction,
                object,
            } => with_transaction(&mut mutated, operation, transaction, |execute| {
                let mut changed = false;

                for step in &mut execute.transaction.steps {
                    if let TransactionStep::CompareAndSet(cas) = step
                        && cas.target.object == *object
                    {
                        *step = TransactionStep::Update(Update {
                            target: cas.target.clone(),
                            fields: cas.fields.clone(),
                            values: cas.values.clone(),
                        });

                        changed = true;
                    }
                }

                // A transaction that can no longer reject must not
                // name a rejection block.
                if changed && !execute.transaction.rejects() {
                    execute.rejected = None;
                }

                changed
            }),

            Self::AddDeleter {
                operation,
                after,
                data_model,
                selector,
            } => add_deleter(&mut mutated, operation, after, data_model, selector),

            Self::SetDelivery {
                operation,
                input,
                delivery,
            } => mutated
                .runtime
                .subscriptions
                .get_mut(operation)
                .and_then(|inputs| inputs.get_mut(input))
                .is_some_and(|subscription| {
                    let changed = subscription.delivery != *delivery;

                    subscription.delivery = *delivery;

                    changed
                }),

            Self::RemoveDeduplicatedBy {
                operation,
                transaction,
            } => with_transaction(&mut mutated, operation, transaction, |execute| {
                let deduplicated = matches!(
                    execute.transaction.idempotency,
                    IdempotencyGuarantee::DeduplicatedBy { .. }
                );

                if deduplicated {
                    execute.transaction.idempotency = IdempotencyGuarantee::Unspecified;
                }

                deduplicated
            }),

            Self::SetExternalIdempotency {
                operation,
                effect,
                idempotency,
            } => with_effect(&mut mutated, operation, effect, |contract| match contract {
                Effect::External(external) => {
                    let changed = external.idempotency != *idempotency;

                    external.idempotency = *idempotency;

                    changed
                }

                Effect::Publication(_) | Effect::Request(_) | Effect::OutboxWrite(_) => false,
            }),

            Self::SetExternalResultReplay {
                operation,
                effect,
                result_replay,
            } => with_effect(&mut mutated, operation, effect, |contract| match contract {
                Effect::External(external) => {
                    let changed = external.result_replay != *result_replay;

                    external.result_replay = *result_replay;

                    changed
                }

                Effect::Publication(_) | Effect::Request(_) | Effect::OutboxWrite(_) => false,
            }),

            Self::UnspecifyRetry { site } => unspecify_retry(&mut mutated, site),

            Self::UnspecifyMessageIdentity { channel } => {
                let identity = match channel {
                    Channel::Topic { topic } => mutated
                        .topics
                        .get_mut(topic)
                        .map(|topic| &mut topic.message_identity),

                    Channel::Outbox { data_model, outbox } => mutated
                        .data_models
                        .get_mut(data_model)
                        .and_then(|declared| declared.outboxes.get_mut(outbox))
                        .map(|outbox| &mut outbox.message_identity),
                };

                identity.is_some_and(|identity| {
                    let changed = *identity != MessageIdentity::Unspecified;

                    *identity = MessageIdentity::Unspecified;

                    changed
                })
            }

            Self::WidenSelector {
                operation,
                transaction,
                step,
            } => with_transaction(&mut mutated, operation, transaction, |execute| {
                let Some(target) = execute.transaction.steps.get_mut(*step).and_then(widenable)
                else {
                    return false;
                };

                let changed = target.predicate != SelectorPredicate::All;

                target.predicate = SelectorPredicate::All;

                changed
            }),

            Self::RemoveRequirement { requirement } => {
                remove_requirement(&mut mutated, requirement)
            }
        };

        changed.then_some(mutated)
    }

    /// Whether applying the mutation makes `fact` false of the model:
    /// the fact names exactly what the mutation removes or changes.
    pub fn falsifies(&self, fact: &Fact) -> bool {
        match (self, fact) {
            (
                Self::RemoveLock {
                    operation,
                    transaction,
                    selector,
                    mode,
                },
                Fact::Lock {
                    operation: fact_operation,
                    transaction: fact_transaction,
                    selector: fact_selector,
                    mode: fact_mode,
                    ..
                },
            ) => {
                operation == fact_operation
                    && transaction == fact_transaction
                    && selector == fact_selector
                    && mode == fact_mode
            }

            (
                Self::SetIsolation {
                    operation,
                    transaction,
                    isolation,
                },
                Fact::Isolation {
                    operation: fact_operation,
                    transaction: fact_transaction,
                    level,
                },
            ) => {
                operation == fact_operation && transaction == fact_transaction && isolation != level
            }

            (
                Self::CompareAndSetToUpdate {
                    operation,
                    transaction,
                    object,
                },
                Fact::GuardedMutation {
                    operation: fact_operation,
                    transaction: fact_transaction,
                    object: fact_object,
                    mechanism: Mechanism::CompareAndSet,
                    ..
                },
            ) => {
                operation == fact_operation
                    && transaction == fact_transaction
                    && object == fact_object
            }

            (
                Self::AddDeleter {
                    data_model,
                    selector,
                    ..
                },
                Fact::NoDeleters {
                    data_model: fact_data_model,
                    object,
                },
            ) => data_model == fact_data_model && selector.object == *object,

            (
                Self::SetDelivery {
                    operation,
                    input,
                    delivery,
                },
                Fact::Delivery {
                    operation: fact_operation,
                    input: fact_input,
                    semantics,
                },
            ) => operation == fact_operation && input == fact_input && delivery != semantics,

            (
                Self::RemoveDeduplicatedBy {
                    operation,
                    transaction,
                },
                Fact::DeduplicatedBy {
                    operation: fact_operation,
                    transaction: fact_transaction,
                    ..
                },
            ) => operation == fact_operation && transaction == fact_transaction,

            (
                Self::SetExternalIdempotency {
                    operation,
                    effect,
                    idempotency,
                },
                Fact::ExternalGuarantee {
                    operation: fact_operation,
                    effect: fact_effect,
                    guarantee,
                    ..
                },
            ) => {
                let declared = match guarantee {
                    Guarantee::Idempotent => Some(ExternalIdempotency::IdenticalPerIdentity),
                    Guarantee::SideEffectFree => Some(ExternalIdempotency::SideEffectFree),
                    Guarantee::ReplayStable => None,
                };

                operation == fact_operation
                    && effect == fact_effect
                    && declared.is_some_and(|declared| declared != *idempotency)
            }

            (
                Self::SetExternalResultReplay {
                    operation,
                    effect,
                    result_replay,
                },
                Fact::ExternalGuarantee {
                    operation: fact_operation,
                    effect: fact_effect,
                    guarantee: Guarantee::ReplayStable,
                    ..
                },
            ) => {
                operation == fact_operation
                    && effect == fact_effect
                    && *result_replay != ExternalResultReplay::ReplayStable
            }

            (Self::UnspecifyRetry { site }, Fact::RetryMayRepeat { site: fact_site }) => {
                site == fact_site
            }

            (
                Self::UnspecifyMessageIdentity { channel },
                Fact::MessageIdentity {
                    channel: fact_channel,
                    ..
                },
            ) => channel == fact_channel,

            (Self::RemoveRequirement { requirement }, Fact::ReliesOn { obligation }) => {
                obligation.requirement() == *requirement
            }

            _ => false,
        }
    }
}

/// The mutation withdrawing `fact` (Part IV §12.2), or `None` for a
/// kind without one. A guarded mutation's withdrawal drops a
/// `compare_and_set`'s comparison; a transition, cursor advance, fence,
/// or upsert has no comparison to drop that would withdraw the fact,
/// so it has none.
pub fn withdrawal(fact: &Fact) -> Option<Mutation> {
    match fact {
        Fact::Isolation {
            operation,
            transaction,
            level,
        } => (*level != TransactionIsolation::Unspecified).then(|| Mutation::SetIsolation {
            operation: operation.clone(),
            transaction: transaction.clone(),
            isolation: TransactionIsolation::Unspecified,
        }),

        Fact::Lock {
            operation,
            transaction,
            selector,
            mode,
            ..
        } => Some(Mutation::RemoveLock {
            operation: operation.clone(),
            transaction: transaction.clone(),
            selector: selector.clone(),
            mode: *mode,
        }),

        Fact::GuardedMutation {
            operation,
            transaction,
            object,
            mechanism,
            ..
        } => match mechanism {
            Mechanism::CompareAndSet => Some(Mutation::CompareAndSetToUpdate {
                operation: operation.clone(),
                transaction: transaction.clone(),
                object: object.clone(),
            }),

            Mechanism::Transition
            | Mechanism::AdvanceCursor
            | Mechanism::Fence
            | Mechanism::Upsert => None,
        },

        Fact::DeduplicatedBy {
            operation,
            transaction,
            ..
        } => Some(Mutation::RemoveDeduplicatedBy {
            operation: operation.clone(),
            transaction: transaction.clone(),
        }),

        Fact::Delivery {
            operation,
            input,
            semantics,
        } => (*semantics != DeliverySemantics::Unspecified).then(|| Mutation::SetDelivery {
            operation: operation.clone(),
            input: input.clone(),
            delivery: DeliverySemantics::Unspecified,
        }),

        Fact::ExternalGuarantee {
            operation,
            effect,
            guarantee,
            ..
        } => Some(match guarantee {
            Guarantee::Idempotent | Guarantee::SideEffectFree => Mutation::SetExternalIdempotency {
                operation: operation.clone(),
                effect: effect.clone(),
                idempotency: ExternalIdempotency::Unspecified,
            },

            Guarantee::ReplayStable => Mutation::SetExternalResultReplay {
                operation: operation.clone(),
                effect: effect.clone(),
                result_replay: ExternalResultReplay::Unspecified,
            },
        }),

        Fact::RetryMayRepeat { site } => Some(Mutation::UnspecifyRetry { site: site.clone() }),

        Fact::MessageIdentity { channel, .. } => Some(Mutation::UnspecifyMessageIdentity {
            channel: channel.clone(),
        }),

        Fact::Cursor { .. }
        | Fact::Fence { .. }
        | Fact::ConflictClosure { .. }
        | Fact::CyclicComponents { .. }
        | Fact::NoDeleters { .. }
        | Fact::KeyedRequirement { .. }
        | Fact::AdmitsNoMessages { .. }
        | Fact::NoAdmittedPaths { .. }
        | Fact::ReturnsNoResult { .. }
        | Fact::Terminal { .. }
        | Fact::StableRoot { .. }
        | Fact::NaturallyReplayable { .. }
        | Fact::ArtifactRoute { .. }
        | Fact::OutboxRedrive { .. }
        | Fact::Propagation { .. }
        | Fact::ReliesOn { .. }
        | Fact::Realization { .. } => None,
    }
}

/// The workspace with `fact` withdrawn (Part IV §12.2), or `None` when
/// its kind has no withdrawal or the workspace does not declare it.
pub fn withdraw(workspace: &WorkspaceState, fact: &Fact) -> Option<WorkspaceState> {
    withdrawal(fact)?.apply(workspace)
}

/// Every mutation of the catalogue at every site of `workspace` where
/// it applies (§19.2), in a fixed order: the catalogue's, then the
/// workspace's.
///
/// - remove a lock step; drop or weaken isolation; replace a
///   `compare_and_set` with an `update`; add a transaction deleting a
///   versioned object;
/// - change a subscription's delivery, to each other value; remove a
///   `deduplicated_by`; change an external effect's idempotency, to
///   each other value;
/// - widen a read, update, delete, or lock selector to every instance;
///   remove a requirement;
/// - and the remaining withdrawals of Part IV §12.2: an external
///   effect's result replay, a request's retry, a channel's message
///   identity.
pub fn catalogue(workspace: &WorkspaceState) -> Vec<Mutation> {
    let transactions = transactions(workspace);

    let mut mutations = Vec::new();

    // Remove a lock step.
    for (operation, transaction) in &transactions {
        let mut seen: Vec<(&ObjectSelector, LockMode)> = Vec::new();

        for step in &transaction.steps {
            if let TransactionStep::Lock(lock) = step
                && !seen.contains(&(&lock.target, lock.mode))
            {
                seen.push((&lock.target, lock.mode));

                mutations.push(Mutation::RemoveLock {
                    operation: (*operation).clone(),
                    transaction: transaction.id.clone(),
                    selector: lock.target.clone(),
                    mode: lock.mode,
                });
            }
        }
    }

    // Drop or weaken isolation.
    for (operation, transaction) in &transactions {
        let weaker = match transaction.isolation {
            TransactionIsolation::Serializable => Some(TransactionIsolation::Snapshot),
            TransactionIsolation::Snapshot => Some(TransactionIsolation::ReadCommitted),
            TransactionIsolation::ReadCommitted | TransactionIsolation::Unspecified => None,
        };

        let dropped = (transaction.isolation != TransactionIsolation::Unspecified)
            .then_some(TransactionIsolation::Unspecified);

        for isolation in dropped.into_iter().chain(weaker) {
            mutations.push(Mutation::SetIsolation {
                operation: (*operation).clone(),
                transaction: transaction.id.clone(),
                isolation,
            });
        }
    }

    // Replace a compare_and_set with an update.
    for (operation, transaction) in &transactions {
        let objects: BTreeSet<&Id> = transaction
            .steps
            .iter()
            .filter_map(|step| match step {
                TransactionStep::CompareAndSet(cas) => Some(&cas.target.object),
                _ => None,
            })
            .collect();

        for object in objects {
            mutations.push(Mutation::CompareAndSetToUpdate {
                operation: (*operation).clone(),
                transaction: transaction.id.clone(),
                object: object.clone(),
            });
        }
    }

    // Add a transaction deleting a versioned object, after each
    // transaction that selects it by values still in scope once the
    // transaction has committed.
    for (operation, transaction) in &transactions {
        let Some(data_model) = &transaction.data_model else {
            continue;
        };

        let mut seen = BTreeSet::new();

        for step in &transaction.steps {
            let Some(selector) = step.selector() else {
                continue;
            };

            let versioned = workspace
                .data_models
                .get(data_model)
                .and_then(|declared| declared.objects.get(&selector.object))
                .is_some_and(|object| object.version.is_some());

            let in_scope = selector
                .predicate
                .roots()
                .iter()
                .all(|root| !matches!(root.source, ValueSource::TransactionRead(_)));

            if versioned && in_scope && seen.insert(selector.object.clone()) {
                mutations.push(Mutation::AddDeleter {
                    operation: (*operation).clone(),
                    after: transaction.id.clone(),
                    data_model: data_model.clone(),
                    selector: selector.clone(),
                });
            }
        }
    }

    // Change a subscription's delivery.
    for (operation, inputs) in &workspace.runtime.subscriptions {
        for (input, subscription) in inputs {
            for delivery in [
                DeliverySemantics::Unspecified,
                DeliverySemantics::AtMostOnce,
                DeliverySemantics::AtLeastOnce,
            ] {
                if delivery != subscription.delivery {
                    mutations.push(Mutation::SetDelivery {
                        operation: operation.clone(),
                        input: input.clone(),
                        delivery,
                    });
                }
            }
        }
    }

    // Remove a deduplicated_by.
    for (operation, transaction) in &transactions {
        if matches!(
            transaction.idempotency,
            IdempotencyGuarantee::DeduplicatedBy { .. }
        ) {
            mutations.push(Mutation::RemoveDeduplicatedBy {
                operation: (*operation).clone(),
                transaction: transaction.id.clone(),
            });
        }
    }

    let externals = external_effects(workspace);

    // Change an external effect's idempotency.
    for (operation, effect, declared, _) in &externals {
        for idempotency in [
            ExternalIdempotency::Unspecified,
            ExternalIdempotency::Distinguishable,
            ExternalIdempotency::IdenticalPerIdentity,
            ExternalIdempotency::SideEffectFree,
        ] {
            if idempotency != *declared {
                mutations.push(Mutation::SetExternalIdempotency {
                    operation: (*operation).clone(),
                    effect: (*effect).clone(),
                    idempotency,
                });
            }
        }
    }

    // Widen a selector.
    for (operation, transaction) in &transactions {
        for (step, declared) in transaction.steps.iter().enumerate() {
            let narrow = match declared {
                TransactionStep::Read(read) => Some(&read.target),
                TransactionStep::Update(update) => Some(&update.target),
                TransactionStep::Delete(delete) => Some(&delete.target),
                TransactionStep::Lock(lock) => Some(&lock.target),
                _ => None,
            }
            .is_some_and(|target| target.predicate != SelectorPredicate::All);

            if narrow {
                mutations.push(Mutation::WidenSelector {
                    operation: (*operation).clone(),
                    transaction: transaction.id.clone(),
                    step,
                });
            }
        }
    }

    // Remove a requirement.
    for requirement in requirements(workspace) {
        mutations.push(Mutation::RemoveRequirement { requirement });
    }

    // The remaining withdrawals of Part IV §12.2.
    for (operation, effect, _, result_replay) in &externals {
        if *result_replay != ExternalResultReplay::Unspecified {
            mutations.push(Mutation::SetExternalResultReplay {
                operation: (*operation).clone(),
                effect: (*effect).clone(),
                result_replay: ExternalResultReplay::Unspecified,
            });
        }
    }

    for site in repeatable_requests(workspace) {
        mutations.push(Mutation::UnspecifyRetry { site });
    }

    for channel in identified_channels(workspace) {
        mutations.push(Mutation::UnspecifyMessageIdentity { channel });
    }

    mutations
}

// ---------------------------------------------------------------------
// Sites
// ---------------------------------------------------------------------

/// Every inline transaction of every operation, with its operation, in
/// program order.
fn transactions(workspace: &WorkspaceState) -> Vec<(&Id, &Transaction)> {
    let mut found = Vec::new();

    for (operation, draft) in &workspace.operations {
        if let Some(program) = &draft.program {
            for block in blocks(program) {
                for step in &block.steps {
                    if let OperationStep::Transaction(execute) = step {
                        found.push((operation, &execute.transaction));
                    }
                }
            }
        }
    }

    found
}

/// A program's blocks, outermost first, each before the blocks nested
/// in its steps.
fn blocks(program: &OperationBlock) -> Vec<&OperationBlock> {
    let mut found = vec![program];
    let mut index = 0;

    while index < found.len() {
        let block = found[index];

        for step in &block.steps {
            match step {
                OperationStep::Transaction(execute) => found.extend(execute.rejected.as_ref()),

                OperationStep::MatchResult(matched) => {
                    found.push(&matched.ok);
                    found.extend(matched.errors.values());
                }

                OperationStep::Branch(branch) => {
                    found.push(&branch.then);
                    found.extend(branch.otherwise.as_ref());
                }

                OperationStep::ExecuteEffect(_)
                | OperationStep::ExecuteEffectAsync(_)
                | OperationStep::ExecuteEffectIntent(_)
                | OperationStep::ExecuteEffectIntentAsync(_)
                | OperationStep::JoinAll(_)
                | OperationStep::Race(_)
                | OperationStep::Return(_)
                | OperationStep::Complete
                | OperationStep::Abandon => {}
            }
        }

        index += 1;
    }

    found
}

/// Calls `visit` on every block of a program, outermost first.
fn blocks_mut(block: &mut OperationBlock, visit: &mut dyn FnMut(&mut OperationBlock)) {
    visit(block);

    for step in &mut block.steps {
        match step {
            OperationStep::Transaction(execute) => {
                if let Some(rejected) = &mut execute.rejected {
                    blocks_mut(rejected, visit);
                }
            }

            OperationStep::MatchResult(matched) => {
                blocks_mut(&mut matched.ok, visit);

                for arm in matched.errors.values_mut() {
                    blocks_mut(arm, visit);
                }
            }

            OperationStep::Branch(branch) => {
                blocks_mut(&mut branch.then, visit);

                if let Some(otherwise) = &mut branch.otherwise {
                    blocks_mut(otherwise, visit);
                }
            }

            OperationStep::ExecuteEffect(_)
            | OperationStep::ExecuteEffectAsync(_)
            | OperationStep::ExecuteEffectIntent(_)
            | OperationStep::ExecuteEffectIntentAsync(_)
            | OperationStep::JoinAll(_)
            | OperationStep::Race(_)
            | OperationStep::Return(_)
            | OperationStep::Complete
            | OperationStep::Abandon => {}
        }
    }
}

/// Applies `edit` to one transaction's execution site, returning
/// whether it changed anything.
fn with_transaction(
    workspace: &mut WorkspaceState,
    operation: &Id,
    transaction: &Id,
    mut edit: impl FnMut(&mut ExecuteTransaction) -> bool,
) -> bool {
    let Some(program) = workspace
        .operations
        .get_mut(operation)
        .and_then(|draft| draft.program.as_mut())
    else {
        return false;
    };

    let mut changed = false;

    blocks_mut(program, &mut |block| {
        for step in &mut block.steps {
            if let OperationStep::Transaction(execute) = step
                && execute.transaction.id == *transaction
            {
                changed |= edit(execute);
            }
        }
    });

    changed
}

/// Applies `edit` to the effect contract declared at one effect site:
/// a program's effect step, or a transaction's intent.
fn with_effect(
    workspace: &mut WorkspaceState,
    operation: &Id,
    effect: &Id,
    mut edit: impl FnMut(&mut Effect) -> bool,
) -> bool {
    let Some(program) = workspace
        .operations
        .get_mut(operation)
        .and_then(|draft| draft.program.as_mut())
    else {
        return false;
    };

    let mut changed = false;

    blocks_mut(program, &mut |block| {
        for step in &mut block.steps {
            match step {
                OperationStep::ExecuteEffect(execute) if execute.effect_id == *effect => {
                    changed |= edit(&mut execute.effect);
                }

                OperationStep::ExecuteEffectAsync(execute) if execute.effect_id == *effect => {
                    changed |= edit(&mut execute.effect);
                }

                OperationStep::Transaction(execute) => {
                    for step in &mut execute.transaction.steps {
                        if let TransactionStep::EstablishEffectIntent(establish) = step
                            && establish.effect_id == *effect
                        {
                            changed |= edit(&mut establish.effect);
                        }
                    }
                }

                _ => {}
            }
        }
    });

    changed
}

/// Every external effect contract at an effect site, with its declared
/// idempotency and result replay.
fn external_effects(
    workspace: &WorkspaceState,
) -> Vec<(&Id, &Id, ExternalIdempotency, ExternalResultReplay)> {
    let mut found = Vec::new();

    for (operation, effect, contract) in effect_sites(workspace) {
        if let Effect::External(external) = contract {
            found.push((
                operation,
                effect,
                external.idempotency,
                external.result_replay,
            ));
        }
    }

    found
}

/// Every effect contract declared at an operation's effect site.
fn effect_sites(workspace: &WorkspaceState) -> Vec<(&Id, &Id, &Effect)> {
    let mut found = Vec::new();

    for (operation, draft) in &workspace.operations {
        let Some(program) = &draft.program else {
            continue;
        };

        for block in blocks(program) {
            for step in &block.steps {
                match step {
                    OperationStep::ExecuteEffect(execute) => {
                        found.push((operation, &execute.effect_id, &execute.effect));
                    }

                    OperationStep::ExecuteEffectAsync(execute) => {
                        found.push((operation, &execute.effect_id, &execute.effect));
                    }

                    OperationStep::Transaction(execute) => {
                        for step in &execute.transaction.steps {
                            if let TransactionStep::EstablishEffectIntent(establish) = step {
                                found.push((operation, &establish.effect_id, &establish.effect));
                            }
                        }
                    }

                    _ => {}
                }
            }
        }
    }

    found
}

/// Every request effect declaring `retry: may_repeat`: at an
/// operation's effect site, or as a transition's side effect.
fn repeatable_requests(workspace: &WorkspaceState) -> Vec<Site> {
    let mut found = Vec::new();

    for (operation, effect, contract) in effect_sites(workspace) {
        if let Effect::Request(request) = contract
            && request.retry == RetrySemantics::MayRepeat
        {
            found.push(Site::EffectSite {
                operation: operation.clone(),
                effect: effect.clone(),
            });
        }
    }

    for (machine, declared) in &workspace.state_machines {
        for (transition, declared) in &declared.transitions {
            for (effect, side_effect) in &declared.side_effects {
                if let TransitionSideEffect::Request(request) = side_effect
                    && request.retry == RetrySemantics::MayRepeat
                {
                    found.push(Site::TransitionEffect {
                        machine: machine.clone(),
                        transition: transition.clone(),
                        effect: effect.clone(),
                    });
                }
            }
        }
    }

    found
}

fn unspecify_retry(workspace: &mut WorkspaceState, site: &Site) -> bool {
    let unspecify = |retry: &mut RetrySemantics| {
        let changed = *retry != RetrySemantics::Unspecified;

        *retry = RetrySemantics::Unspecified;

        changed
    };

    match site {
        Site::EffectSite { operation, effect } => {
            with_effect(workspace, operation, effect, |contract| match contract {
                Effect::Request(request) => unspecify(&mut request.retry),
                Effect::Publication(_) | Effect::External(_) | Effect::OutboxWrite(_) => false,
            })
        }

        Site::TransitionEffect {
            machine,
            transition,
            effect,
        } => workspace
            .state_machines
            .get_mut(machine)
            .and_then(|declared| declared.transitions.get_mut(transition))
            .and_then(|declared| declared.side_effects.get_mut(effect))
            .is_some_and(|side_effect| match side_effect {
                TransitionSideEffect::Request(request) => unspecify(&mut request.retry),
                TransitionSideEffect::Publication(_) => false,
            }),
    }
}

/// Every channel declaring a message identity.
fn identified_channels(workspace: &WorkspaceState) -> Vec<Channel> {
    let topics = workspace
        .topics
        .iter()
        .filter(|(_, topic)| topic.message_identity != MessageIdentity::Unspecified)
        .map(|(topic, _)| Channel::Topic {
            topic: topic.clone(),
        });

    let outboxes = workspace
        .data_models
        .iter()
        .flat_map(|(data_model, declared)| {
            declared
                .outboxes
                .iter()
                .filter(|(_, outbox)| outbox.message_identity != MessageIdentity::Unspecified)
                .map(|(outbox, _)| Channel::Outbox {
                    data_model: data_model.clone(),
                    outbox: outbox.clone(),
                })
        });

    topics.chain(outboxes).collect()
}

/// The selector a widening may replace: a read's, an update's, a
/// delete's, or a lock's. The other selecting steps pin their target's
/// identity, which validation requires of them.
fn widenable(step: &mut TransactionStep) -> Option<&mut ObjectSelector> {
    match step {
        TransactionStep::Read(read) => Some(&mut read.target),
        TransactionStep::Update(update) => Some(&mut update.target),
        TransactionStep::Delete(delete) => Some(&mut delete.target),
        TransactionStep::Lock(lock) => Some(&mut lock.target),

        TransactionStep::CompareAndSet(_)
        | TransactionStep::Insert(_)
        | TransactionStep::Upsert(_)
        | TransactionStep::Transition(_)
        | TransactionStep::AdvanceCursor(_)
        | TransactionStep::Fence(_)
        | TransactionStep::EstablishEffectIntent(_)
        | TransactionStep::EstablishTransactionOutput(_)
        | TransactionStep::WriteOutbox(_) => None,
    }
}

/// Inserts a deleting transaction right after `after`'s execution site.
fn add_deleter(
    workspace: &mut WorkspaceState,
    operation: &Id,
    after: &Id,
    data_model: &Id,
    selector: &ObjectSelector,
) -> bool {
    let Some(program) = workspace
        .operations
        .get_mut(operation)
        .and_then(|draft| draft.program.as_mut())
    else {
        return false;
    };

    let taken: BTreeSet<Id> = blocks(program)
        .into_iter()
        .flat_map(|block| &block.steps)
        .filter_map(|step| match step {
            OperationStep::Transaction(execute) => Some(execute.transaction.id.clone()),
            _ => None,
        })
        .collect();

    let id = (0..)
        .map(|n| match n {
            0 => Id(format!("{after}_deleter")),
            n => Id(format!("{after}_deleter_{n}")),
        })
        .find(|candidate| !taken.contains(candidate))
        .expect("an unused id");

    let deleter = OperationStep::Transaction(ExecuteTransaction {
        transaction: Transaction {
            id,
            data_model: Some(data_model.clone()),
            isolation: TransactionIsolation::Unspecified,
            idempotency: IdempotencyGuarantee::Unspecified,
            requirements: TransactionRequirements::default(),
            steps: vec![TransactionStep::Delete(Delete {
                target: selector.clone(),
            })],
        },
        rejected: None,
    });

    let mut inserted = false;

    blocks_mut(program, &mut |block| {
        if inserted {
            return;
        }

        let position = block.steps.iter().position(|step| {
            matches!(step, OperationStep::Transaction(execute) if execute.transaction.id == *after)
        });

        if let Some(position) = position {
            block.steps.insert(position + 1, deleter.clone());
            inserted = true;
        }
    });

    inserted
}

/// Every declared requirement's identity, operations' first.
fn requirements(workspace: &WorkspaceState) -> Vec<RequirementIdentity> {
    let mut found = Vec::new();

    for (operation, draft) in &workspace.operations {
        let subject = ObligationSubject::Operation {
            operation: operation.clone(),
        };

        let idempotency = basis::requirement_keys(
            &subject,
            ObligationProperty::Idempotency,
            draft
                .requirements
                .idempotency
                .iter()
                .map(|requirement| requirement.key.components.clone()),
        );

        let recoverability = basis::requirement_keys(
            &subject,
            ObligationProperty::Recoverability,
            draft
                .requirements
                .recoverability
                .iter()
                .map(|requirement| requirement.key.components.clone()),
        );

        found.extend(
            idempotency
                .iter()
                .chain(&recoverability)
                .map(|key| key.requirement()),
        );
    }

    for (operation, transaction) in transactions(workspace) {
        for property in [
            ObligationProperty::TransactionSerializability,
            ObligationProperty::TransactionOrdering,
        ] {
            found.extend(
                basis::transaction_keys(operation, transaction, property)
                    .iter()
                    .map(|key| key.requirement()),
            );
        }
    }

    found
}

/// Removes the requirement `identity` names, if the workspace declares
/// it.
fn remove_requirement(workspace: &mut WorkspaceState, identity: &RequirementIdentity) -> bool {
    match &identity.subject {
        ObligationSubject::Operation { operation } => {
            let Some(draft) = workspace.operations.get_mut(operation) else {
                return false;
            };

            let subject = identity.subject.clone();

            match identity.family {
                RequirementFamily::Idempotency => {
                    let keys = basis::requirement_keys(
                        &subject,
                        ObligationProperty::Idempotency,
                        draft
                            .requirements
                            .idempotency
                            .iter()
                            .map(|requirement| requirement.key.components.clone()),
                    );

                    remove_at(&mut draft.requirements.idempotency, &keys, identity)
                }

                RequirementFamily::Recoverability => {
                    let keys = basis::requirement_keys(
                        &subject,
                        ObligationProperty::Recoverability,
                        draft
                            .requirements
                            .recoverability
                            .iter()
                            .map(|requirement| requirement.key.components.clone()),
                    );

                    remove_at(&mut draft.requirements.recoverability, &keys, identity)
                }

                RequirementFamily::TransactionSerializability
                | RequirementFamily::TransactionOrdering => false,
            }
        }

        ObligationSubject::Transaction {
            operation,
            transaction,
        } => with_transaction(workspace, operation, transaction, |execute| {
            let declared = &mut execute.transaction;

            match identity.family {
                RequirementFamily::TransactionSerializability => {
                    let keys = basis::transaction_keys(
                        operation,
                        declared,
                        ObligationProperty::TransactionSerializability,
                    );

                    remove_at(&mut declared.requirements.serializability, &keys, identity)
                }

                RequirementFamily::TransactionOrdering => {
                    let keys = basis::transaction_keys(
                        operation,
                        declared,
                        ObligationProperty::TransactionOrdering,
                    );

                    remove_at(&mut declared.requirements.ordering, &keys, identity)
                }

                RequirementFamily::Idempotency | RequirementFamily::Recoverability => false,
            }
        }),
    }
}

/// Removes the element whose key names `identity`.
fn remove_at<T>(
    list: &mut Vec<T>,
    keys: &[basis::ObligationKey],
    identity: &RequirementIdentity,
) -> bool {
    match keys.iter().position(|key| key.requirement() == *identity) {
        Some(index) if index < list.len() => {
            list.remove(index);

            true
        }

        Some(_) | None => false,
    }
}

/// A unit enum value as its durable spelling.
fn snake<T: serde::Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(text)) => text,
        _ => String::new(),
    }
}
