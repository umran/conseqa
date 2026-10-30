//! Extraction of bases and blocked-on sets from the structured proofs
//! (§10.5, §10.7, and §10.8 of the spec change analysis revision).
//!
//! The basis is read from the verifiers' structured verdicts, never
//! from the rendered report: the prose may be reworded freely, the
//! structures are what the verifiers decided. The model is read only
//! to state a fact in full where a proof names a step or a binding: a
//! lock step's selector, an object's data model, a channel's identity
//! mapping.
//!
//! Every function here matches its structure's variants by name, with
//! no wildcard arm, and destructures every struct with all of its
//! fields named. A new proof route, obstacle, evidence, or gap, or a
//! new field on one, therefore fails to compile until the catalogue
//! says what it contributes, instead of silently producing an empty
//! basis. Fields that contribute nothing are bound to `_` by name,
//! which records that decision where a reader looks for it.

use std::collections::BTreeSet;

use crate::analyzer::verification::{
    self, ArtifactAvailability, ArtifactReplay, CommitOrderEvidence, ConditionalMutationKind,
    ConsumerCollapse, DecisionGap, DecisionReplay, DecisionRule, DependencyEvidence, DependencyGap,
    DependencySide, EffectRetrySafety, EffectSafety, GoverningKeyDefect, GuardCoverage, HeldWrite,
    IdempotencyCheck, IdempotencyObstacle, IdempotencyProof, IdempotencyVerdict, IdentityLineage,
    InstanceGap, InstanceStability, IsolationFact, LineageFact, LineageSource, LockRef,
    ManagedFieldRef, PathResumption, PathRetrySafety, PayloadIdentityGap, ProducerRef,
    RecoverabilityCheck, RecoverabilityObstacle, RecoverabilityProof, RecoverabilityVerdict,
    ReplayGap, Resolution, ResultGap, ResultReplayCheck, ResultReplayObstacle, ResultReplayProof,
    ResultReplayVerdict, ResultStabilityRule, RetryDriver, RetryRoute, RetryableReturn,
    ReturnedResult, StabilityGap, StabilityRule, TransactionOrderingCheck,
    TransactionOrderingObstacle, TransactionOrderingProof, TransactionOrderingVerdict,
    TransactionRef, TransactionResolution, TransactionRetrySafety, TransactionSerializabilityCheck,
    TransactionSerializabilityObstacle, TransactionSerializabilityProof,
    TransactionSerializabilityVerdict, UnstableRoot, VerificationReport, key_input,
    returns_consistently,
};
use crate::spec::{
    DeliverySemantics, Effect, ExternalIdentity, FieldPath, Id, IdempotencyGuarantee, Input,
    LockMode, MessageIdentity, MessageSelector, Model, ObjectSelector, Transaction,
    TransactionIsolation, TransactionStep, TransitionSideEffect, ValueRef,
};

use super::{
    ArtifactRoute, BasisFact, BlockedOn, Channel, Coverage, DeclarationKey, Fact, FactSet,
    Guarantee, Mechanism, ObligationKey, ObligationProperty, RootRule, Site, TransactionId,
    idempotency_keys, recoverability_keys, transaction_keys,
};

/// What one verdict rests on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Grounds {
    /// A proven verdict's basis.
    Basis(Vec<BasisFact>),

    /// An unproven verdict's blocked-on set.
    BlockedOn(BlockedOn),
}

/// Extracts the grounds of the checks of one verification report,
/// against the model it verified.
pub struct Extractor<'a> {
    model: &'a Model,
    verification: &'a VerificationReport,
}

impl<'a> Extractor<'a> {
    pub fn new(model: &'a Model, verification: &'a VerificationReport) -> Self {
        Self {
            model,
            verification,
        }
    }

    pub fn transaction_serializability(&self, check: &TransactionSerializabilityCheck) -> Grounds {
        let TransactionSerializabilityCheck {
            operation,
            transaction,
            location: _,
            requirement,
            key: _,
            verdict,
            // The admissions a proven commit carries are reported beside
            // the proof; nothing about them is relied on.
            artifacts: _,
        } = check;

        let mut collector = self.collector(
            operation,
            self.transaction_requirement(
                operation,
                transaction,
                ObligationProperty::TransactionSerializability,
                *requirement,
            ),
        );

        match verdict {
            TransactionSerializabilityVerdict::Proven { proof, scope: _ } => {
                collector.serializability_proof(proof);
                collector.basis()
            }

            TransactionSerializabilityVerdict::Unproven { obstacles } => {
                for obstacle in obstacles {
                    collector.serializability_obstacle(obstacle);
                }

                collector.blocked_on()
            }
        }
    }

    pub fn transaction_ordering(&self, check: &TransactionOrderingCheck) -> Grounds {
        let TransactionOrderingCheck {
            operation,
            transaction,
            location: _,
            requirement,
            key: _,
            position: _,
            verdict,
            artifacts: _,
        } = check;

        let mut collector = self.collector(
            operation,
            self.transaction_requirement(
                operation,
                transaction,
                ObligationProperty::TransactionOrdering,
                *requirement,
            ),
        );

        match verdict {
            TransactionOrderingVerdict::Proven { proof, scope: _ } => {
                collector.ordering_proof(proof);
                collector.basis()
            }

            TransactionOrderingVerdict::Unproven { obstacles } => {
                for obstacle in obstacles {
                    collector.ordering_obstacle(transaction, obstacle);
                }

                collector.blocked_on()
            }
        }
    }

    pub fn idempotency(&self, check: &IdempotencyCheck) -> Grounds {
        let IdempotencyCheck {
            operation,
            requirement,
            key: _,
            verdict,
            // A coinductive proof rests on the requirements it reaches,
            // which its `relies_on` facts already name.
            coinductive: _,
            lineage,
        } = check;

        let mut collector = self.collector(
            operation,
            self.operation_requirement(operation, ObligationProperty::Idempotency, *requirement),
        );

        match verdict {
            IdempotencyVerdict::Proven { proof, scope: _ } => {
                collector.idempotency_proof(proof);

                // Declared propagations are what the identity-based
                // population rests on; an undeclared one is evidence
                // for a reader, and relied on by nothing.
                for lineage in lineage {
                    collector.lineage(lineage);
                }

                collector.basis()
            }

            IdempotencyVerdict::Unproven { obstacles } => {
                for obstacle in obstacles {
                    collector.idempotency_obstacle(obstacle);
                }

                collector.blocked_on()
            }
        }
    }

    pub fn result_replay(&self, check: &ResultReplayCheck) -> Grounds {
        let ResultReplayCheck {
            operation,
            requirement,
            key: _,
            verdict,
            coinductive: _,
        } = check;

        let mut collector = self.collector(
            operation,
            self.operation_requirement(operation, ObligationProperty::ResultReplay, *requirement),
        );

        match verdict {
            ResultReplayVerdict::Proven { proof, scope: _ } => {
                collector.result_replay_proof(proof);
                collector.basis()
            }

            ResultReplayVerdict::Unproven { obstacles } => {
                for obstacle in obstacles {
                    collector.result_replay_obstacle(obstacle);
                }

                collector.blocked_on()
            }
        }
    }

    pub fn recoverability(&self, check: &RecoverabilityCheck) -> Grounds {
        let RecoverabilityCheck {
            operation,
            requirement,
            key,
            completion: _,
            verdict,
            // Notes sit beside a verdict and do not bear on it.
            notes: _,
        } = check;

        let mut collector = self.collector(
            operation,
            self.operation_requirement(operation, ObligationProperty::Recoverability, *requirement),
        );

        match verdict {
            RecoverabilityVerdict::Proven { proof, scope: _ } => {
                collector.recoverability_proof(key_input(key), proof);
                collector.basis()
            }

            RecoverabilityVerdict::Unproven { obstacles } => {
                for obstacle in obstacles {
                    collector.recoverability_obstacle(obstacle);
                }

                collector.blocked_on()
            }
        }
    }

    fn collector(&self, operation: &'a Id, requirement: Option<DeclarationKey>) -> Collector<'a> {
        Collector {
            model: self.model,
            verification: self.verification,
            operation,
            requirement,
            present: FactSet::default(),
            missing: FactSet::default(),
            anchors: BTreeSet::new(),
        }
    }

    /// The requirement declaration of a transaction obligation.
    fn transaction_requirement(
        &self,
        operation: &Id,
        transaction: &Id,
        property: ObligationProperty,
        index: usize,
    ) -> Option<DeclarationKey> {
        let declared = transaction_declaration(self.model, operation, transaction)?;

        transaction_keys(operation, declared, property)
            .get(index)
            .map(|key| DeclarationKey::Requirement(key.requirement()))
    }

    /// The requirement declaration of an operation obligation.
    fn operation_requirement(
        &self,
        operation: &Id,
        property: ObligationProperty,
        index: usize,
    ) -> Option<DeclarationKey> {
        let declared = self.model.operations.get(operation)?;

        let keys = match property {
            ObligationProperty::Recoverability => recoverability_keys(operation, declared),

            ObligationProperty::Idempotency | ObligationProperty::ResultReplay => {
                idempotency_keys(operation, declared, property)
            }

            ObligationProperty::TransactionSerializability
            | ObligationProperty::TransactionOrdering => Vec::new(),
        };

        keys.get(index)
            .map(|key| DeclarationKey::Requirement(key.requirement()))
    }
}

/// The facts and anchors of one obligation's verdict, as they are
/// found.
struct Collector<'a> {
    model: &'a Model,
    verification: &'a VerificationReport,

    /// The obligation's operation: the one a governing key, a path, or
    /// a stable root belongs to.
    operation: &'a Id,

    /// The requirement declaration the obligation comes from, which a
    /// key-component root's stability is read from.
    requirement: Option<DeclarationKey>,

    present: FactSet,
    missing: FactSet,
    anchors: BTreeSet<DeclarationKey>,
}

impl<'a> Collector<'a> {
    // -----------------------------------------------------------------
    // Results
    // -----------------------------------------------------------------

    /// A proven verdict's basis: the facts found. Nothing of a proof is
    /// missing, and every declaration a proof read anchors a fact.
    fn basis(self) -> Grounds {
        debug_assert!(
            self.missing.into_facts().is_empty() && self.anchors.is_empty(),
            "a proof contributed a gap"
        );

        Grounds::Basis(self.present.into_facts())
    }

    fn blocked_on(self) -> Grounds {
        Grounds::BlockedOn(BlockedOn {
            present: self.present.into_facts(),
            missing: self.missing.into_facts(),
            anchors: self.anchors,
        })
    }

    fn fact(&mut self, fact: Fact, anchors: impl IntoIterator<Item = DeclarationKey>) {
        self.present.insert(fact, anchors);
    }

    fn missing_fact(&mut self, fact: Fact, anchors: impl IntoIterator<Item = DeclarationKey>) {
        self.missing.insert(fact, anchors);
    }

    fn anchor(&mut self, anchor: DeclarationKey) {
        self.anchors.insert(anchor);
    }

    // -----------------------------------------------------------------
    // Transaction serializability
    // -----------------------------------------------------------------

    fn serializability_proof(&mut self, proof: &TransactionSerializabilityProof) {
        match proof {
            TransactionSerializabilityProof::SerializableIsolationClosure {
                root,
                key,
                closure,
            } => {
                self.closure(root, key, closure);

                for member in closure {
                    self.isolation(member, TransactionIsolation::Serializable);
                }
            }

            TransactionSerializabilityProof::ConflictGraph {
                root,
                key,
                closure,
                dependencies,
            } => {
                self.closure(root, key, closure);

                // Only a dependency inside a cyclic component needs
                // commit-order evidence; one between components closes
                // no cycle, and its evidence is not relied on.
                let components = verification::cyclic_components(closure, dependencies);

                self.fact(
                    Fact::CyclicComponents {
                        root: transaction_id(root),
                        key: key.clone(),
                        components: components
                            .iter()
                            .map(|component| component.iter().map(transaction_id).collect())
                            .collect(),
                    },
                    closure.iter().map(transaction_anchor),
                );

                for dependency in dependencies {
                    let inside = components.iter().any(|component| {
                        component.contains(&dependency.source)
                            && component.contains(&dependency.target)
                    });

                    if inside {
                        self.dependency(dependency);
                    }
                }
            }
        }
    }

    fn closure(&mut self, root: &TransactionRef, key: &ValueRef, closure: &[TransactionRef]) {
        self.fact(
            Fact::ConflictClosure {
                root: transaction_id(root),
                key: key.clone(),
                members: closure.iter().map(transaction_id).collect(),
            },
            closure.iter().map(transaction_anchor),
        );
    }

    fn isolation(&mut self, transaction: &TransactionRef, level: TransactionIsolation) {
        self.fact(
            Fact::Isolation {
                operation: transaction.operation.clone(),
                transaction: transaction.transaction.clone(),
                level,
            },
            [transaction_anchor(transaction)],
        );
    }

    /// One dependency's evidence, or, on an unconstrained dependency,
    /// its gaps.
    fn dependency(&mut self, dependency: &DependencyEvidence) {
        let DependencyEvidence {
            kind: _,
            source,
            source_step: _,
            source_mode: _,
            target,
            target_step: _,
            target_mode: _,
            object,
            selector_overlap: _,
            field_overlap: _,
            evidence,
            // Equal fencing tokens order nothing, so a fence is recorded
            // beside the evidence and constrains no dependency.
            fence: _,
            gaps,
        } = dependency;

        match evidence {
            // Only a write-read dependency's reader, the target, is
            // credited with an intrinsic committed read.
            CommitOrderEvidence::IntrinsicCommittedRead { isolation } => {
                self.isolation(target, *isolation);
            }

            CommitOrderEvidence::AtomicWriteOrder {
                source_isolation,
                target_isolation,
            } => {
                self.isolation(source, *source_isolation);
                self.isolation(target, *target_isolation);
            }

            CommitOrderEvidence::StrictLock {
                reader_lock,
                writer_lock,
            } => {
                self.lock(reader_lock);
                self.lock(writer_lock);
            }

            CommitOrderEvidence::AtomicConditionalMutation {
                guarded_by,
                step: _,
                object,
                mechanism,
                guard,
                compared_fields,
                held_write,
            } => {
                self.guarded_mutation(guarded_by, object, *mechanism, guard, compared_fields);

                match held_write.as_deref() {
                    None => {}

                    Some(HeldWrite::Isolation {
                        transaction,
                        isolation,
                    }) => self.isolation(transaction, *isolation),

                    Some(HeldWrite::GuardedMutation {
                        guarded_by,
                        step: _,
                        object,
                        mechanism,
                        guard,
                        compared_fields,
                    }) => self.guarded_mutation(
                        guarded_by,
                        object,
                        *mechanism,
                        guard,
                        compared_fields,
                    ),
                }
            }

            // A read-only observation's reader is the source of its
            // anti-dependencies.
            CommitOrderEvidence::ReadOnlyObservation { isolation } => {
                self.isolation(source, *isolation);
            }

            CommitOrderEvidence::OrderedCursor {
                object,
                field,
                rule,
            } => {
                let data_model = self.object_data_model(object);

                self.fact(
                    Fact::Cursor {
                        data_model: data_model.clone(),
                        object: object.clone(),
                        field: field.clone(),
                        rule: *rule,
                    },
                    [
                        object_anchor(&data_model, object),
                        transaction_anchor(source),
                        transaction_anchor(target),
                    ],
                );
            }

            CommitOrderEvidence::None => {}
        }

        if !gaps.is_empty() {
            self.anchor(transaction_anchor(source));
            self.anchor(transaction_anchor(target));
            self.anchor(object_anchor(&self.object_data_model(object), object));
        }

        for gap in gaps {
            self.dependency_gap(gap);
        }
    }

    /// A guarded mutation's fact, and the facts its coverage rests on
    /// beyond the mutation itself.
    fn guarded_mutation(
        &mut self,
        guarded_by: &TransactionRef,
        object: &Id,
        mechanism: ConditionalMutationKind,
        guard: &GuardCoverage,
        compared_fields: &BTreeSet<FieldPath>,
    ) {
        let data_model = self.transaction_data_model(guarded_by, object);

        let coverage = match guard {
            GuardCoverage::Atomic => Some(Coverage::Atomic),

            GuardCoverage::HeldProtection => Some(Coverage::HeldProtection),

            GuardCoverage::ObservedState { read, read_step: _ } => {
                Some(Coverage::ObservedState { read: read.clone() })
            }

            GuardCoverage::ObservedVersion {
                read,
                read_step: _,
                version_field,
                no_deleters,
            } => {
                // The token covers an insertion only because no
                // transaction of the model deletes the object.
                if *no_deleters {
                    self.fact(
                        Fact::NoDeleters {
                            data_model: data_model.clone(),
                            object: object.clone(),
                        },
                        [object_anchor(&data_model, object)],
                    );
                }

                Some(Coverage::ObservedVersion {
                    read: read.clone(),
                    version_field: version_field.clone(),
                })
            }

            // The reader's lock is relied on as much as the writer's
            // guard: without it the guard orders nothing.
            GuardCoverage::LockedReader { reader_lock } => {
                self.lock(reader_lock);

                self.lock_step(reader_lock)
                    .map(|(selector, mode)| Coverage::LockedReader {
                        selector: selector.clone(),
                        mode,
                    })
            }
        };

        // A model that fails validation may name a step that is not
        // there; verification stays total on it, and so does this.
        let Some(coverage) = coverage else {
            return;
        };

        self.fact(
            Fact::GuardedMutation {
                operation: guarded_by.operation.clone(),
                transaction: guarded_by.transaction.clone(),
                data_model: data_model.clone(),
                object: object.clone(),
                mechanism: mechanism_of(mechanism),
                coverage,
                compared_fields: compared_fields.clone(),
            },
            [
                transaction_anchor(guarded_by),
                object_anchor(&data_model, object),
            ],
        );
    }

    fn lock(&mut self, lock: &LockRef) {
        let LockRef {
            transaction,
            step: _,
            mode: _,
        } = lock;

        let Some((selector, mode)) = self.lock_step(lock) else {
            return;
        };

        let object = selector.object.clone();
        let data_model = self.transaction_data_model(transaction, &object);

        self.fact(
            Fact::Lock {
                operation: transaction.operation.clone(),
                transaction: transaction.transaction.clone(),
                data_model: data_model.clone(),
                object: object.clone(),
                selector: selector.clone(),
                mode,
            },
            [
                transaction_anchor(transaction),
                object_anchor(&data_model, &object),
            ],
        );
    }

    fn serializability_obstacle(&mut self, obstacle: &TransactionSerializabilityObstacle) {
        match obstacle {
            TransactionSerializabilityObstacle::TransactionNotIndexed {
                operation,
                transaction,
            } => self.anchor(DeclarationKey::Transaction {
                operation: operation.clone(),
                transaction: transaction.clone(),
            }),

            // Present, and insufficient for the closure route.
            TransactionSerializabilityObstacle::SerializableClosureContainsWeakerIsolation {
                transactions,
            } => {
                for IsolationFact {
                    transaction,
                    isolation,
                } in transactions
                {
                    self.isolation(transaction, *isolation);
                }
            }

            TransactionSerializabilityObstacle::TransactionSerializabilityUnconstrainedCycle {
                cycle,
            } => {
                for member in cycle {
                    self.anchor(transaction_anchor(member));
                }
            }

            TransactionSerializabilityObstacle::TransactionSerializabilityUnprotectedReadWriteDependency {
                dependency,
            }
            | TransactionSerializabilityObstacle::TransactionSerializabilityUnconstrainedDependency {
                dependency,
            } => self.dependency(dependency),
        }
    }

    fn dependency_gap(&mut self, gap: &DependencyGap) {
        match gap {
            // The lock route's premise, stated exactly: a lock over the
            // access's own selector, shared for a reader and exclusive
            // for a writer.
            DependencyGap::LockCoverageMissing {
                transaction,
                side,
                object,
                step,
            } => {
                let data_model = self.transaction_data_model(transaction, object);

                self.anchor(transaction_anchor(transaction));
                self.anchor(object_anchor(&data_model, object));

                let Some(selector) = self.access_selector(transaction, *step) else {
                    return;
                };

                self.missing_fact(
                    Fact::Lock {
                        operation: transaction.operation.clone(),
                        transaction: transaction.transaction.clone(),
                        data_model: data_model.clone(),
                        object: object.clone(),
                        selector: selector.clone(),
                        mode: match side {
                            DependencySide::Reader => LockMode::Shared,
                            DependencySide::Writer => LockMode::Exclusive,
                        },
                    },
                    [
                        transaction_anchor(transaction),
                        object_anchor(&data_model, object),
                    ],
                );
            }

            // Present, and insufficient: acquired after the access.
            DependencyGap::LockAcquiredAfterProtectedAccess {
                transaction,
                side: _,
                lock_step,
                access_step: _,
            } => {
                self.anchor(transaction_anchor(transaction));

                if let Some(TransactionStep::Lock(lock)) =
                    self.step(&transaction.operation, &transaction.transaction, *lock_step)
                {
                    self.lock(&LockRef {
                        transaction: transaction.clone(),
                        step: *lock_step,
                        mode: lock.mode,
                    });
                }
            }

            DependencyGap::ObservedStateGuardMissing {
                transaction,
                object,
                step: _,
            }
            | DependencyGap::ObservedStateGuardDoesNotCoverConflict {
                transaction,
                object,
                step: _,
                guard_step: _,
                fields: _,
            }
            | DependencyGap::ObservedStateNotIdentified {
                transaction,
                object,
                step: _,
            } => {
                let data_model = self.transaction_data_model(transaction, object);

                self.anchor(transaction_anchor(transaction));
                self.anchor(object_anchor(&data_model, object));
            }

            // The version route's premise, stated exactly: no deleter,
            // falsified by the one named.
            DependencyGap::ObservedVersionMayRepeat {
                transaction,
                object,
                step: _,
                deleted_by,
            } => {
                let data_model = self.object_data_model(object);

                self.anchor(transaction_anchor(transaction));

                self.missing_fact(
                    Fact::NoDeleters {
                        data_model: data_model.clone(),
                        object: object.clone(),
                    },
                    [
                        object_anchor(&data_model, object),
                        transaction_anchor(deleted_by),
                    ],
                );
            }

            // Present, and insufficient: nothing says it reads
            // committed data or installs writes in commit order.
            DependencyGap::IsolationUnspecified { transaction } => {
                self.isolation(transaction, TransactionIsolation::Unspecified);
            }

            DependencyGap::TransactionConflictUnknownSelectorOverlap { object }
            | DependencyGap::TransactionConflictUnknownFieldOverlap { object } => {
                let data_model = self.object_data_model(object);

                self.anchor(object_anchor(&data_model, object));
            }
        }
    }

    // -----------------------------------------------------------------
    // Transaction ordering
    // -----------------------------------------------------------------

    fn ordering_proof(&mut self, proof: &TransactionOrderingProof) {
        match proof {
            TransactionOrderingProof::Cursor {
                key: _,
                position: _,
                cursor,
                rule,
                step: _,
                serializability,
            } => {
                let ManagedFieldRef { object, field } = cursor;
                let data_model = self.object_data_model(object);

                let mut anchors = vec![object_anchor(&data_model, object)];
                anchors.extend(self.managed_field_writers(object, field));

                self.fact(
                    Fact::Cursor {
                        data_model,
                        object: object.clone(),
                        field: field.clone(),
                        rule: *rule,
                    },
                    anchors,
                );

                self.serializability_proof(serializability);
            }

            TransactionOrderingProof::Fence {
                key: _,
                position: _,
                fence,
                step: _,
                serializability,
            } => {
                let ManagedFieldRef { object, field } = fence;
                let data_model = self.object_data_model(object);

                let mut anchors = vec![object_anchor(&data_model, object)];
                anchors.extend(self.managed_field_writers(object, field));

                self.fact(
                    Fact::Fence {
                        data_model,
                        object: object.clone(),
                        field: field.clone(),
                    },
                    anchors,
                );

                self.serializability_proof(serializability);
            }
        }
    }

    fn ordering_obstacle(&mut self, transaction: &Id, obstacle: &TransactionOrderingObstacle) {
        let requiring = DeclarationKey::Transaction {
            operation: self.operation.clone(),
            transaction: transaction.clone(),
        };

        match obstacle {
            TransactionOrderingObstacle::TransactionNotIndexed {
                operation,
                transaction,
            } => self.anchor(DeclarationKey::Transaction {
                operation: operation.clone(),
                transaction: transaction.clone(),
            }),

            TransactionOrderingObstacle::OrderingMissingSerializability { obstacles } => {
                for obstacle in obstacles {
                    self.serializability_obstacle(obstacle);
                }
            }

            TransactionOrderingObstacle::OrderingMissingCursorOrFence => self.anchor(requiring),

            // Present, and insufficient: a cursor or fence whose
            // position or domain is not the requirement's.
            TransactionOrderingObstacle::OrderingPositionMismatch { step, incoming: _ }
            | TransactionOrderingObstacle::OrderingKeyDomainMismatch { step, object: _ } => {
                self.anchor(requiring);
                self.managed_step(transaction, *step);
            }

            TransactionOrderingObstacle::OrderingUncontrolledManagedFieldWriter {
                transaction,
                step: _,
                field,
            } => {
                let ManagedFieldRef { object, field: _ } = field;
                let data_model = self.object_data_model(object);

                self.anchor(requiring);
                self.anchor(transaction_anchor(transaction));
                self.anchor(object_anchor(&data_model, object));
            }
        }
    }

    /// The cursor or fence fact of the obligation's transaction's step.
    fn managed_step(&mut self, transaction: &Id, step: usize) {
        let operation = self.operation;

        let fact = match self.step(operation, transaction, step) {
            Some(TransactionStep::AdvanceCursor(advance)) => {
                let object = &advance.target.object;
                let data_model = self.object_data_model(object);

                Some((
                    Fact::Cursor {
                        data_model: data_model.clone(),
                        object: object.clone(),
                        field: advance.field.clone(),
                        rule: advance.rule,
                    },
                    object_anchor(&data_model, object),
                ))
            }

            Some(TransactionStep::Fence(fence)) => {
                let object = &fence.target.object;
                let data_model = self.object_data_model(object);

                Some((
                    Fact::Fence {
                        data_model: data_model.clone(),
                        object: object.clone(),
                        field: fence.field.clone(),
                    },
                    object_anchor(&data_model, object),
                ))
            }

            _ => None,
        };

        if let Some((fact, object)) = fact {
            self.fact(
                fact,
                [
                    object,
                    DeclarationKey::Transaction {
                        operation: operation.clone(),
                        transaction: transaction.clone(),
                    },
                ],
            );
        }
    }

    // -----------------------------------------------------------------
    // Idempotency
    // -----------------------------------------------------------------

    fn idempotency_proof(&mut self, proof: &IdempotencyProof) {
        match proof {
            IdempotencyProof::NoAdmittedInvocations { input } => self.admits_no_messages(input),

            IdempotencyProof::NoAdmittedPaths { input } => self.fact(
                Fact::NoAdmittedPaths {
                    operation: self.operation.clone(),
                    input: input.clone(),
                },
                [self.program_anchor(), self.input_anchor(input)],
            ),

            IdempotencyProof::SingleDelivery { input, topic } => {
                self.delivery(self.operation, input, DeliverySemantics::AtMostOnce);

                let channel = Channel::Topic {
                    topic: topic.clone(),
                };

                for schema in self.admitted_schemas(self.operation, input) {
                    self.message_identity(
                        &channel,
                        &schema,
                        [subscription_anchor(self.operation, input)],
                    );
                }
            }

            IdempotencyProof::RetrySafePaths { paths } => {
                for path in paths {
                    let PathRetrySafety {
                        path: _,
                        decisions,
                        transactions,
                        effects,
                    } = path;

                    for decision in decisions {
                        self.decision(decision);
                    }

                    for TransactionRetrySafety { transaction, route } in transactions {
                        self.retry_route(transaction, route);
                    }

                    for EffectRetrySafety { effect, safety } in effects {
                        self.effect_safety(effect, safety);
                    }
                }
            }
        }
    }

    fn retry_route(&mut self, transaction: &Id, route: &RetryRoute) {
        match route {
            RetryRoute::KeyedCommit { key } => self.keyed_commit(transaction, key),

            RetryRoute::NaturalReplay { body } => self.natural_replay(transaction, body),
        }
    }

    fn keyed_commit(&mut self, transaction: &Id, key: &[verification::StableRoot]) {
        self.fact(
            Fact::DeduplicatedBy {
                operation: self.operation.clone(),
                transaction: transaction.clone(),
                key: key.iter().map(|root| root.root.clone()).collect(),
            },
            [self.transaction_key(transaction)],
        );

        for root in key {
            self.stable_root(root);
        }
    }

    fn natural_replay(&mut self, transaction: &Id, body: &[verification::StableRoot]) {
        self.fact(
            Fact::NaturallyReplayable {
                operation: self.operation.clone(),
                transaction: transaction.clone(),
            },
            [self.transaction_key(transaction)],
        );

        for root in body {
            self.stable_root(root);
        }
    }

    fn effect_safety(&mut self, effect: &Id, safety: &EffectSafety) {
        let site = self.effect_anchor(self.operation, effect);

        match safety {
            EffectSafety::ExternallyIdempotent { identity_key } => {
                self.external_guarantee(effect, Guarantee::Idempotent);

                for root in identity_key {
                    self.stable_root(root);
                }
            }

            EffectSafety::ExternallySideEffectFree => {
                self.external_guarantee(effect, Guarantee::SideEffectFree);
            }

            EffectSafety::SameLogicalMessage {
                topic,
                schema,
                instance,
                consumers,
            } => {
                let channel = Channel::Topic {
                    topic: topic.clone(),
                };

                self.message_identity(&channel, schema, [site]);
                self.instance(instance);

                for consumer in consumers {
                    self.consumer(consumer);
                }
            }

            EffectSafety::DeduplicatedByTarget {
                operation,
                input,
                instance,
            } => {
                for key in self.idempotency_obligations(operation, input, true) {
                    self.relies_on(key, [site.clone()]);
                }

                self.instance(instance);
            }

            EffectSafety::TransactionDeduplicated { transaction, key } => {
                self.keyed_commit(transaction, key);
            }

            EffectSafety::SameLogicalOutboxMessage {
                outbox,
                schema,
                instance,
                consumers,
            } => {
                if let Some(channel) = self.outbox_channel(outbox) {
                    self.message_identity(&channel, schema, [site]);
                }

                self.instance(instance);

                for consumer in consumers {
                    self.consumer(consumer);
                }
            }
        }
    }

    fn consumer(&mut self, consumer: &ConsumerCollapse) {
        match consumer {
            ConsumerCollapse::ProvenRequirement { operation, input } => {
                for key in self.idempotency_obligations(operation, input, true) {
                    self.relies_on(key, []);
                }
            }

            ConsumerCollapse::SingleDelivery { operation, input } => {
                self.delivery(operation, input, DeliverySemantics::AtMostOnce);
            }
        }
    }

    fn instance(&mut self, instance: &InstanceStability) {
        match instance {
            InstanceStability::ReplayDeterministic { roots } => {
                for root in roots {
                    self.stable_root(root);
                }
            }

            InstanceStability::EstablishedIntent { intent, replay } => {
                self.artifact(intent, replay);
            }
        }
    }

    fn lineage(&mut self, lineage: &IdentityLineage) {
        let IdentityLineage {
            source,
            schema,
            producer,
            fact,
        } = lineage;

        match fact {
            LineageFact::Propagated {
                source: _,
                requirement: _,
            } => {
                let channel = match source {
                    LineageSource::Topic { topic } => Some(Channel::Topic {
                        topic: topic.clone(),
                    }),

                    LineageSource::Outbox { outbox } => self.outbox_channel(outbox),
                };

                let (site, anchor) = match producer {
                    ProducerRef::Operation { operation, effect } => (
                        Site::EffectSite {
                            operation: operation.clone(),
                            effect: effect.clone(),
                        },
                        self.effect_anchor(operation, effect),
                    ),

                    ProducerRef::Transition {
                        machine,
                        transition,
                        effect,
                    } => (
                        Site::TransitionEffect {
                            machine: machine.clone(),
                            transition: transition.clone(),
                            effect: effect.clone(),
                        },
                        DeclarationKey::Transition {
                            machine: machine.clone(),
                            transition: transition.clone(),
                        },
                    ),
                };

                // The propagation counts only where its targets cover
                // the channel's identity fields, so its truth is read
                // from the channel's message identity too.
                if let Some(channel) = channel {
                    let declared = match &channel {
                        Channel::Topic { topic } => DeclarationKey::Topic(topic.clone()),

                        Channel::Outbox { data_model, outbox } => DeclarationKey::Outbox {
                            data_model: data_model.clone(),
                            outbox: outbox.clone(),
                        },
                    };

                    self.fact(
                        Fact::Propagation {
                            producer: site,
                            schema: schema.clone(),
                            channel,
                        },
                        [anchor, declared],
                    );
                }
            }

            LineageFact::Undeclared => {}
        }
    }

    fn idempotency_obstacle(&mut self, obstacle: &IdempotencyObstacle) {
        match obstacle {
            IdempotencyObstacle::GoverningKeyInadmissible { defect } => {
                self.governing_key(defect);
            }

            IdempotencyObstacle::PathDecisionUnstable {
                path: _,
                decision: _,
                gap,
            } => {
                self.anchor(self.program_anchor());
                self.decision_gap(gap);
            }

            IdempotencyObstacle::TransactionNotRetrySafe {
                path: _,
                transaction,
                recovery,
                reconstruction,
            } => self.unresolvable(transaction, recovery, reconstruction),

            IdempotencyObstacle::ExternalApplicationsDistinguishable { path: _, effect }
            | IdempotencyObstacle::ExternalIdempotencyUnknown { path: _, effect }
            | IdempotencyObstacle::EffectInstanceUnspecified { path: _, effect } => {
                self.anchor(self.effect_anchor(self.operation, effect));
            }

            IdempotencyObstacle::ExternalIdentityKeyUnstable {
                path: _,
                effect,
                roots,
            }
            | IdempotencyObstacle::EffectInstanceRootUnstable {
                path: _,
                effect,
                roots,
            } => {
                self.anchor(self.effect_anchor(self.operation, effect));

                for root in roots {
                    self.unstable_root(root);
                }
            }

            IdempotencyObstacle::PublicationNotIdentified {
                path: _,
                effect,
                topic,
                schema,
            } => {
                self.anchor(self.effect_anchor(self.operation, effect));
                self.anchor(DeclarationKey::Topic(topic.clone()));
                self.anchor(DeclarationKey::Schema(schema.clone()));
            }

            IdempotencyObstacle::PublicationConsumerNotKeyed {
                path: _,
                effect,
                topic,
                schema: _,
                operation,
                input,
            } => {
                self.anchor(self.effect_anchor(self.operation, effect));
                self.anchor(DeclarationKey::Topic(topic.clone()));
                self.keyed_requirement_missing(operation, input, ObligationProperty::Idempotency);
            }

            // Present, and insufficient: the consumer's requirement is
            // declared and not proven.
            IdempotencyObstacle::PublicationConsumerRequirementUnproven {
                path: _,
                effect,
                topic,
                schema: _,
                operation,
                input,
            } => {
                self.anchor(self.effect_anchor(self.operation, effect));
                self.anchor(DeclarationKey::Topic(topic.clone()));

                for key in self.idempotency_obligations(operation, input, false) {
                    self.relies_on(key, []);
                }
            }

            IdempotencyObstacle::OutboxWriteNotIdentified {
                path: _,
                effect,
                transaction,
                outbox,
                schema,
            } => {
                self.anchor(self.effect_anchor(self.operation, effect));
                self.anchor(self.transaction_key(transaction));
                self.anchor(DeclarationKey::Schema(schema.clone()));

                if let Some(anchor) = self.outbox_anchor(outbox) {
                    self.anchor(anchor);
                }
            }

            IdempotencyObstacle::OutboxConsumerNotKeyed {
                path: _,
                effect,
                outbox,
                schema: _,
                operation,
                input,
            } => {
                self.anchor(self.effect_anchor(self.operation, effect));
                self.keyed_requirement_missing(operation, input, ObligationProperty::Idempotency);

                if let Some(anchor) = self.outbox_anchor(outbox) {
                    self.anchor(anchor);
                }
            }

            IdempotencyObstacle::OutboxConsumerRequirementUnproven {
                path: _,
                effect,
                outbox,
                schema: _,
                operation,
                input,
            } => {
                self.anchor(self.effect_anchor(self.operation, effect));

                if let Some(anchor) = self.outbox_anchor(outbox) {
                    self.anchor(anchor);
                }

                for key in self.idempotency_obligations(operation, input, false) {
                    self.relies_on(key, []);
                }
            }

            IdempotencyObstacle::IntentNotEstablished { path: _, intent: _ } => {
                self.anchor(self.program_anchor());
            }

            IdempotencyObstacle::IntentNotReplayAvailable {
                path: _,
                intent: _,
                transaction,
                recovery,
                reconstruction,
            } => {
                self.anchor(self.transaction_key(transaction));
                self.replay_gaps(transaction, recovery);
                self.replay_gaps(transaction, reconstruction);
            }

            IdempotencyObstacle::RequestSchemaMismatch {
                path: _,
                effect,
                expected,
                actual,
            } => {
                self.anchor(self.effect_anchor(self.operation, effect));
                self.anchor(DeclarationKey::Schema(expected.clone()));
                self.anchor(DeclarationKey::Schema(actual.clone()));
            }

            IdempotencyObstacle::RequestTargetHasNoKeyedRequirement {
                path: _,
                effect,
                operation,
                input,
            } => {
                self.anchor(self.effect_anchor(self.operation, effect));
                self.keyed_requirement_missing(operation, input, ObligationProperty::Idempotency);
            }

            // Present, and insufficient: the target's requirement is
            // declared and not proven.
            IdempotencyObstacle::RequestTargetRequirementUnproven {
                path: _,
                effect,
                operation,
                input,
            } => {
                self.anchor(self.effect_anchor(self.operation, effect));

                for key in self.idempotency_obligations(operation, input, false) {
                    self.relies_on(key, []);
                }
            }
        }
    }

    // -----------------------------------------------------------------
    // Result replay
    // -----------------------------------------------------------------

    fn result_replay_proof(&mut self, proof: &ResultReplayProof) {
        match proof {
            ResultReplayProof::NoAdmittedInvocations { input } => self.admits_no_messages(input),

            ResultReplayProof::NoReturnedResult { input } => self.fact(
                Fact::ReturnsNoResult {
                    operation: self.operation.clone(),
                    input: input.clone(),
                },
                [self.program_anchor()],
            ),

            ResultReplayProof::ClassFixedResult { returns, retryable } => {
                for returned in returns {
                    let ReturnedResult {
                        path: _,
                        arm: _,
                        decisions,
                        derivation,
                    } = returned;

                    for decision in decisions {
                        self.decision(decision);
                    }

                    for root in derivation {
                        self.stable_root(root);
                    }
                }

                // A retryable return is exempt by the contract itself;
                // nothing about it is relied on.
                for RetryableReturn { path: _, error: _ } in retryable {}
            }
        }
    }

    fn result_replay_obstacle(&mut self, obstacle: &ResultReplayObstacle) {
        match obstacle {
            ResultReplayObstacle::GoverningKeyInadmissible { defect } => {
                self.governing_key(defect);
            }

            ResultReplayObstacle::PathDecisionUnstable {
                path: _,
                decision: _,
                gap,
            } => {
                self.anchor(self.program_anchor());
                self.decision_gap(gap);
            }

            ResultReplayObstacle::ResultDerivationUnspecified { path: _, request } => {
                self.anchor(self.program_anchor());
                self.anchor(self.input_anchor(request));
            }

            ResultReplayObstacle::ResultDerivationRootUnstable {
                path: _,
                request,
                roots,
            } => {
                self.anchor(self.program_anchor());
                self.anchor(self.input_anchor(request));

                for root in roots {
                    self.unstable_root(root);
                }
            }
        }
    }

    // -----------------------------------------------------------------
    // Recoverability
    // -----------------------------------------------------------------

    fn recoverability_proof(&mut self, input: Option<&Id>, proof: &RecoverabilityProof) {
        match proof {
            RecoverabilityProof::NoAdmittedInvocations { input } => self.admits_no_messages(input),

            RecoverabilityProof::Resumable { paths } => self.resumption(input, paths),

            RecoverabilityProof::Guaranteed { driver, paths } => {
                self.retry_driver(driver);
                self.resumption(input, paths);
            }
        }
    }

    fn resumption(&mut self, input: Option<&Id>, paths: &[PathResumption]) {
        for path in paths {
            let PathResumption {
                path: _,
                transactions,
                artifacts,
                // An abandoning path is judged as an interruption at its
                // end; its re-drive is the driver's fact.
                abandons: _,
            } = path;

            for TransactionResolution {
                transaction,
                resolution,
            } in transactions
            {
                match resolution {
                    Resolution::KeyedCommit { key } => self.keyed_commit(transaction, key),

                    Resolution::NaturalReplay { body } => self.natural_replay(transaction, body),

                    Resolution::TerminalStep => {
                        if let Some(input) = input {
                            self.fact(
                                Fact::Terminal {
                                    operation: self.operation.clone(),
                                    input: input.clone(),
                                },
                                [self.program_anchor()],
                            );
                        }
                    }
                }
            }

            for ArtifactAvailability { artifact, replay } in artifacts {
                self.artifact(artifact, replay);
            }
        }
    }

    fn retry_driver(&mut self, driver: &RetryDriver) {
        match driver {
            RetryDriver::AtLeastOnceDelivery { input, topic: _ } => {
                self.delivery(self.operation, input, DeliverySemantics::AtLeastOnce);
            }

            RetryDriver::IntrinsicOutboxRedrive { input, outbox } => {
                let Some((data_model, _)) = self.model.outbox(outbox) else {
                    return;
                };

                self.fact(
                    Fact::OutboxRedrive {
                        operation: self.operation.clone(),
                        input: input.clone(),
                        data_model: data_model.clone(),
                        outbox: outbox.clone(),
                    },
                    [
                        self.input_anchor(input),
                        DeclarationKey::Outbox {
                            data_model: data_model.clone(),
                            outbox: outbox.clone(),
                        },
                    ],
                );
            }

            RetryDriver::InboundRepeatableRequest { operation, effect } => self.fact(
                Fact::RetryMayRepeat {
                    site: Site::EffectSite {
                        operation: operation.clone(),
                        effect: effect.clone(),
                    },
                },
                [self.effect_anchor(operation, effect)],
            ),

            RetryDriver::InboundRepeatableTransitionEffect {
                machine,
                transition,
                effect,
            } => self.fact(
                Fact::RetryMayRepeat {
                    site: Site::TransitionEffect {
                        machine: machine.clone(),
                        transition: transition.clone(),
                        effect: effect.clone(),
                    },
                },
                [DeclarationKey::Transition {
                    machine: machine.clone(),
                    transition: transition.clone(),
                }],
            ),
        }
    }

    fn recoverability_obstacle(&mut self, obstacle: &RecoverabilityObstacle) {
        match obstacle {
            RecoverabilityObstacle::GoverningKeyInadmissible { defect } => {
                self.governing_key(defect);
            }

            RecoverabilityObstacle::NoAdmittedPath { input } => {
                self.anchor(self.program_anchor());
                self.anchor(self.input_anchor(input));
            }

            RecoverabilityObstacle::PathNotTerminated { path: _ }
            | RecoverabilityObstacle::EveryPathAbandons { input: _ }
            | RecoverabilityObstacle::ArtifactNotEstablished {
                path: _,
                artifact: _,
                consumer: _,
            } => self.anchor(self.program_anchor()),

            RecoverabilityObstacle::TransactionNotResolvable {
                path: _,
                transaction,
                recovery,
                reconstruction,
            } => self.unresolvable(transaction, recovery, reconstruction),

            RecoverabilityObstacle::ArtifactNotReplayAvailable {
                path: _,
                artifact: _,
                transaction,
                recovery,
                reconstruction,
            } => {
                self.anchor(self.transaction_key(transaction));
                self.replay_gaps(transaction, recovery);
                self.replay_gaps(transaction, reconstruction);
            }

            // A subscription's declared delivery is present and
            // insufficient, and at-least-once is the route's premise.
            // A request input has no delivery: what it lacks is a
            // modeled caller that repeats.
            RecoverabilityObstacle::NoModeledRetryDriver { input, delivery } => match delivery {
                Some(delivery) => {
                    self.delivery(self.operation, input, *delivery);

                    self.missing_fact(
                        Fact::Delivery {
                            operation: self.operation.clone(),
                            input: input.clone(),
                            semantics: DeliverySemantics::AtLeastOnce,
                        },
                        [subscription_anchor(self.operation, input)],
                    );
                }

                None => self.anchor(self.input_anchor(input)),
            },
        }
    }

    // -----------------------------------------------------------------
    // Shared replay structures
    // -----------------------------------------------------------------

    fn admits_no_messages(&mut self, input: &Id) {
        let mut anchors = vec![self.input_anchor(input)];

        anchors.extend(self.channel_anchor(self.operation, input));

        self.fact(
            Fact::AdmitsNoMessages {
                operation: self.operation.clone(),
                input: input.clone(),
            },
            anchors,
        );
    }

    fn delivery(&mut self, operation: &Id, input: &Id, semantics: DeliverySemantics) {
        self.fact(
            Fact::Delivery {
                operation: operation.clone(),
                input: input.clone(),
                semantics,
            },
            [subscription_anchor(operation, input)],
        );
    }

    /// A channel's identity for one schema, when it declares one.
    fn message_identity(
        &mut self,
        channel: &Channel,
        schema: &Id,
        anchors: impl IntoIterator<Item = DeclarationKey>,
    ) {
        let (identity, channel_anchor) = match channel {
            Channel::Topic { topic } => (
                self.model
                    .topics
                    .get(topic)
                    .map(|declared| &declared.message_identity),
                DeclarationKey::Topic(topic.clone()),
            ),

            Channel::Outbox { data_model, outbox } => (
                self.model
                    .data_models
                    .get(data_model)
                    .and_then(|declared| declared.outboxes.get(outbox))
                    .map(|declared| &declared.message_identity),
                DeclarationKey::Outbox {
                    data_model: data_model.clone(),
                    outbox: outbox.clone(),
                },
            ),
        };

        let fields = match identity {
            Some(MessageIdentity::Keyed(key)) => key.mapping.get(schema).cloned(),
            Some(MessageIdentity::Unspecified) | None => None,
        };

        let Some(fields) = fields else {
            return;
        };

        let mut anchors: Vec<DeclarationKey> = anchors.into_iter().collect();
        anchors.push(channel_anchor);

        self.fact(
            Fact::MessageIdentity {
                channel: channel.clone(),
                schema: schema.clone(),
                fields,
            },
            anchors,
        );
    }

    fn external_guarantee(&mut self, effect: &Id, guarantee: Guarantee) {
        let Some(identity) = self.external_identity(effect) else {
            return;
        };

        self.fact(
            Fact::ExternalGuarantee {
                operation: self.operation.clone(),
                effect: effect.clone(),
                guarantee,
                identity,
            },
            [self.effect_anchor(self.operation, effect)],
        );
    }

    /// A consumer or request target declares no requirement of
    /// `property` keyed from its input: the query fact it fails, read
    /// from the input its requirements would be keyed from.
    fn keyed_requirement_missing(
        &mut self,
        operation: &Id,
        input: &Id,
        property: ObligationProperty,
    ) {
        self.missing_fact(
            Fact::KeyedRequirement {
                operation: operation.clone(),
                input: input.clone(),
                property,
            },
            [input_key(operation, input)],
        );
    }

    fn relies_on(&mut self, key: ObligationKey, anchors: impl IntoIterator<Item = DeclarationKey>) {
        let mut anchors: Vec<DeclarationKey> = anchors.into_iter().collect();
        anchors.push(DeclarationKey::Requirement(key.requirement()));

        self.fact(Fact::ReliesOn { obligation: key }, anchors);
    }

    /// An artifact's route to a retry, and the facts the route rests
    /// on; an unavailable artifact's gaps.
    fn artifact(&mut self, artifact: &Id, replay: &ArtifactReplay) {
        match replay {
            ArtifactReplay::Recovered { transaction, key } => {
                self.artifact_route(transaction, artifact, ArtifactRoute::Recovered);
                self.keyed_commit(transaction, key);
            }

            ArtifactReplay::Reconstructed {
                transaction,
                derivation,
                body,
            } => {
                self.artifact_route(transaction, artifact, ArtifactRoute::Reconstructed);
                self.natural_replay(transaction, body);

                for root in derivation {
                    self.stable_root(root);
                }
            }

            ArtifactReplay::Unavailable {
                transaction,
                recovery,
                reconstruction,
            } => {
                self.anchor(self.transaction_key(transaction));
                self.replay_gaps(transaction, recovery);
                self.replay_gaps(transaction, reconstruction);
            }
        }
    }

    fn artifact_route(&mut self, transaction: &Id, artifact: &Id, route: ArtifactRoute) {
        self.fact(
            Fact::ArtifactRoute {
                operation: self.operation.clone(),
                transaction: transaction.clone(),
                artifact: artifact.clone(),
                route,
            },
            [self.transaction_key(transaction)],
        );
    }

    /// A root's stability, and the declared fact its rule rests on.
    fn stable_root(&mut self, root: &verification::StableRoot) {
        let verification::StableRoot { root, rule } = root;

        match rule {
            StabilityRule::KeyComponent => {
                let anchors: Vec<DeclarationKey> = self.requirement.iter().cloned().collect();

                self.root_fact(RootRule::KeyComponent, root, anchors);
            }

            // Pinned by a declared identity: a request input's, or a
            // channel's message identity for every admitted schema.
            StabilityRule::IdentifiedPayload => {
                let mut anchors: Vec<DeclarationKey> = self.requirement.iter().cloned().collect();

                if let crate::spec::ValueSource::Input(input) = &root.source {
                    anchors.push(self.input_anchor(input));
                    anchors.extend(self.channel_anchor(self.operation, input));

                    if let Some(channel) = self.input_channel(self.operation, input) {
                        for schema in self.admitted_schemas(self.operation, input) {
                            self.message_identity(&channel, &schema, []);
                        }
                    }
                }

                self.root_fact(RootRule::IdentifiedPayload, root, anchors);
            }

            StabilityRule::RecoveredArtifact { transaction } => {
                self.root_fact(
                    RootRule::RecoveredArtifact,
                    root,
                    [self.transaction_key(transaction)],
                );

                if let Some(key) = self.declared_commit_key(transaction) {
                    self.fact(
                        Fact::DeduplicatedBy {
                            operation: self.operation.clone(),
                            transaction: transaction.clone(),
                            key,
                        },
                        [self.transaction_key(transaction)],
                    );
                }
            }

            StabilityRule::ReconstructedArtifact { transaction } => {
                self.root_fact(
                    RootRule::ReconstructedArtifact,
                    root,
                    [self.transaction_key(transaction)],
                );

                self.fact(
                    Fact::NaturallyReplayable {
                        operation: self.operation.clone(),
                        transaction: transaction.clone(),
                    },
                    [self.transaction_key(transaction)],
                );
            }

            // Stable because the target proves its result replay: the
            // proof rests on that obligation (§10.6).
            StabilityRule::ReplayConsistentResult { result: _, effect } => {
                let site = self.effect_anchor(self.operation, effect);

                if let Some(key) = self.target_result_obligation(effect) {
                    self.relies_on(key, [site]);
                }
            }

            StabilityRule::ReplayStableExternalResult {
                result: _,
                effect,
                arm: _,
            } => {
                let site = self.effect_anchor(self.operation, effect);

                self.root_fact(RootRule::ReplayStableExternalResult, root, [site]);
                self.external_guarantee(effect, Guarantee::ReplayStable);
            }
        }
    }

    fn root_fact(
        &mut self,
        rule: RootRule,
        root: &ValueRef,
        anchors: impl IntoIterator<Item = DeclarationKey>,
    ) {
        self.fact(
            Fact::StableRoot {
                operation: self.operation.clone(),
                rule,
                root: root.clone(),
            },
            anchors,
        );
    }

    fn decision(&mut self, decision: &DecisionReplay) {
        let DecisionReplay {
            // Which arm a path takes is positional; the rule is what
            // was relied on.
            decision: _,
            rule,
        } = decision;

        match rule {
            DecisionRule::StableResult {
                result: _,
                effect,
                rule,
            } => self.result_rule(effect, rule),

            DecisionRule::StableCondition { roots } => {
                for root in roots {
                    self.stable_root(root);
                }
            }

            // Derived from program structure, never an implementation
            // assumption: it lapses by itself when the continuation
            // gains work.
            DecisionRule::IdempotencyInertContinuation => {}

            DecisionRule::ResolvedCommit { transaction, key } => {
                self.keyed_commit(transaction, key);
            }

            DecisionRule::OutcomeDivergenceAddsNoWork { transaction: _ } => {}
        }
    }

    fn result_rule(&mut self, effect: &Id, rule: &ResultStabilityRule) {
        let site = self.effect_anchor(self.operation, effect);

        match rule {
            ResultStabilityRule::ReplayConsistentTarget {
                operation,
                input: _,
                requirement,
                instance,
            } => {
                if let Some(key) = self.result_obligation(operation, *requirement) {
                    self.relies_on(key, [site]);
                }

                self.instance(instance);
            }

            ResultStabilityRule::ExternalTerminalResult {
                arm: _,
                identity_key,
            } => {
                self.external_guarantee(effect, Guarantee::ReplayStable);

                for root in identity_key {
                    self.stable_root(root);
                }
            }
        }
    }

    // -----------------------------------------------------------------
    // Shared gaps
    // -----------------------------------------------------------------

    fn governing_key(&mut self, defect: &GoverningKeyDefect) {
        if let Some(requirement) = self.requirement.clone() {
            self.anchor(requirement);
        }

        match defect {
            GoverningKeyDefect::Empty | GoverningKeyDefect::ComponentNotFromInput { source: _ } => {
            }

            GoverningKeyDefect::ComponentsFromMultipleInputs { first, second } => {
                self.anchor(self.input_anchor(first));
                self.anchor(self.input_anchor(second));
            }

            GoverningKeyDefect::InputNotDeclared { input } => {
                self.anchor(self.input_anchor(input));
            }
        }
    }

    /// A transaction no retry route resolves: natural replay is the
    /// premise route A lacks, and both routes' gaps are read.
    fn unresolvable(
        &mut self,
        transaction: &Id,
        recovery: &[ReplayGap],
        reconstruction: &[ReplayGap],
    ) {
        let anchor = self.transaction_key(transaction);

        self.anchor(anchor.clone());

        self.missing_fact(
            Fact::NaturallyReplayable {
                operation: self.operation.clone(),
                transaction: transaction.clone(),
            },
            [anchor],
        );

        self.replay_gaps(transaction, recovery);
        self.replay_gaps(transaction, reconstruction);
    }

    fn replay_gaps(&mut self, transaction: &Id, gaps: &[ReplayGap]) {
        for gap in gaps {
            match gap {
                ReplayGap::NoKeyedCommit
                | ReplayGap::ContainsTransition
                | ReplayGap::ContainsInsert
                | ReplayGap::ContainsDelete
                | ReplayGap::PublishesVersion
                | ReplayGap::ContainsCompareAndSet
                | ReplayGap::ContainsFenceComparison
                | ReplayGap::ContainsUpsert
                | ReplayGap::ContainsCursorAdvance
                | ReplayGap::MutationDerivationUnspecified
                | ReplayGap::ArtifactDerivationUnspecified => {
                    self.anchor(self.transaction_key(transaction));
                }

                // Present, and insufficient: declared over a root that
                // is not stable.
                ReplayGap::CommitKeyRootUnstable { root: _, gap } => {
                    if let Some(key) = self.declared_commit_key(transaction) {
                        self.fact(
                            Fact::DeduplicatedBy {
                                operation: self.operation.clone(),
                                transaction: transaction.clone(),
                                key,
                            },
                            [self.transaction_key(transaction)],
                        );
                    }

                    self.stability_gap(gap);
                }

                ReplayGap::MutationTargetRootUnstable { root: _, gap }
                | ReplayGap::MutationDerivationRootUnstable { root: _, gap }
                | ReplayGap::ArtifactDerivationRootUnstable { root: _, gap } => {
                    self.anchor(self.transaction_key(transaction));
                    self.stability_gap(gap);
                }
            }
        }
    }

    fn unstable_root(&mut self, root: &UnstableRoot) {
        let UnstableRoot { root: _, gap } = root;

        self.stability_gap(gap);
    }

    fn stability_gap(&mut self, gap: &StabilityGap) {
        match gap {
            StabilityGap::UnidentifiedPayloadField { input, identity } => {
                self.anchor(self.input_anchor(input));

                if let Some(anchor) = self.channel_anchor(self.operation, input) {
                    self.anchor(anchor);
                }

                match identity {
                    PayloadIdentityGap::NotDeclared => {}

                    PayloadIdentityGap::SchemaNotMapped { schema } => {
                        self.anchor(DeclarationKey::Schema(schema.clone()));
                    }

                    PayloadIdentityGap::NotPinnedByKey { schema, field: _ } => {
                        if let Some(requirement) = self.requirement.clone() {
                            self.anchor(requirement);
                        }

                        if let Some(schema) = schema {
                            self.anchor(DeclarationKey::Schema(schema.clone()));
                        }
                    }
                }
            }

            StabilityGap::NotTriggeringInput { input } => {
                self.anchor(self.input_anchor(input));

                if let Some(requirement) = self.requirement.clone() {
                    self.anchor(requirement);
                }
            }

            StabilityGap::MutableSubjectState { machine } => {
                self.anchor(DeclarationKey::StateMachine(machine.clone()));
            }

            StabilityGap::EffectPayloadRoot { effect } => {
                self.anchor(self.effect_anchor(self.operation, effect));
            }

            StabilityGap::TransactionReadRoot { read: _ }
            | StabilityGap::ArtifactNotInContext { artifact: _ }
            | StabilityGap::ResultNotInContext { result: _ } => {
                self.anchor(self.program_anchor());
            }

            StabilityGap::ArtifactUnavailable {
                artifact: _,
                transaction,
                recovery,
                reconstruction,
            } => {
                self.anchor(self.transaction_key(transaction));
                self.replay_gaps(transaction, recovery);
                self.replay_gaps(transaction, reconstruction);
            }

            StabilityGap::ResultUnstable {
                result: _,
                effect,
                gap,
            } => {
                self.anchor(self.effect_anchor(self.operation, effect));
                self.result_gap(gap);
            }
        }
    }

    fn decision_gap(&mut self, gap: &DecisionGap) {
        match gap {
            DecisionGap::ConditionUnspecified | DecisionGap::ResultNotInContext { result: _ } => {
                self.anchor(self.program_anchor());
            }

            DecisionGap::ConditionRootsUnstable { roots } => {
                for root in roots {
                    self.unstable_root(root);
                }
            }

            DecisionGap::ResultUnstable {
                result: _,
                effect,
                gap,
            } => {
                self.anchor(self.effect_anchor(self.operation, effect));
                self.result_gap(gap);
            }

            DecisionGap::TransactionOutcomeUnstable {
                transaction,
                outcome: _,
                recovery,
            } => {
                self.anchor(self.transaction_key(transaction));
                self.replay_gaps(transaction, recovery);
            }
        }
    }

    fn result_gap(&mut self, gap: &ResultGap) {
        match gap {
            ResultGap::InstanceNotClassFixed { gap } => self.instance_gap(gap),

            ResultGap::RequestSchemaMismatch { expected, actual } => {
                self.anchor(DeclarationKey::Schema(expected.clone()));
                self.anchor(DeclarationKey::Schema(actual.clone()));
            }

            ResultGap::TargetResultNotDeclared { operation, input } => {
                self.keyed_requirement_missing(operation, input, ObligationProperty::ResultReplay);
            }

            ResultGap::TargetErrorRetryable {
                operation,
                input,
                error: _,
            } => self.anchor(input_key(operation, input)),

            // Present, and insufficient: declared and not proven.
            ResultGap::TargetResultUnproven { operation, input } => {
                self.anchor(input_key(operation, input));

                if let Some(declared) = self.model.operations.get(operation)
                    && let Some(index) = returns_consistently(declared, input)
                    && let Some(key) = self.result_obligation(operation, index)
                {
                    self.relies_on(key, []);
                }
            }

            ResultGap::ExternalIdentityKeyUnstable { roots } => {
                for root in roots {
                    self.unstable_root(root);
                }
            }

            ResultGap::ExternalResultNotReplayStable { declared: _ }
            | ResultGap::ExternalErrorRetryable
            | ResultGap::ExternalErrorDispositionUnspecified
            | ResultGap::NoResultContract => {}

            ResultGap::RaceWinnerNondeterministic { candidates: _ } => {
                self.anchor(self.program_anchor());
            }
        }
    }

    fn instance_gap(&mut self, gap: &InstanceGap) {
        match gap {
            InstanceGap::DerivationUnspecified => {}

            InstanceGap::RootsUnstable { roots } => {
                for root in roots {
                    self.unstable_root(root);
                }
            }

            InstanceGap::IntentNotEstablished { intent: _ } => {
                self.anchor(self.program_anchor());
            }

            InstanceGap::IntentNotReplayAvailable {
                intent: _,
                transaction,
                recovery,
                reconstruction,
            } => {
                self.anchor(self.transaction_key(transaction));
                self.replay_gaps(transaction, recovery);
                self.replay_gaps(transaction, reconstruction);
            }
        }
    }

    // -----------------------------------------------------------------
    // Model lookups
    // -----------------------------------------------------------------

    fn program_anchor(&self) -> DeclarationKey {
        DeclarationKey::Program(self.operation.clone())
    }

    fn input_anchor(&self, input: &Id) -> DeclarationKey {
        input_key(self.operation, input)
    }

    fn transaction_key(&self, transaction: &Id) -> DeclarationKey {
        DeclarationKey::Transaction {
            operation: self.operation.clone(),
            transaction: transaction.clone(),
        }
    }

    /// The step of an inline transaction.
    fn step(&self, operation: &Id, transaction: &Id, step: usize) -> Option<&'a TransactionStep> {
        transaction_declaration(self.model, operation, transaction)?
            .steps
            .get(step)
    }

    /// A lock step's selector and mode.
    fn lock_step(&self, lock: &LockRef) -> Option<(&'a ObjectSelector, LockMode)> {
        match self.step(
            &lock.transaction.operation,
            &lock.transaction.transaction,
            lock.step,
        )? {
            TransactionStep::Lock(declared) => Some((&declared.target, declared.mode)),
            _ => None,
        }
    }

    /// The selector of the step an access was indexed from. An insert
    /// selects nothing by a selector, so a lock over it is not stated.
    fn access_selector(
        &self,
        transaction: &TransactionRef,
        step: usize,
    ) -> Option<&'a ObjectSelector> {
        match self.step(&transaction.operation, &transaction.transaction, step)? {
            TransactionStep::Read(read) => Some(&read.target),
            TransactionStep::Update(update) => Some(&update.target),
            TransactionStep::CompareAndSet(guarded) => Some(&guarded.target),
            TransactionStep::Upsert(upsert) => Some(&upsert.target),
            TransactionStep::Delete(delete) => Some(&delete.target),
            TransactionStep::Lock(lock) => Some(&lock.target),
            TransactionStep::Transition(transition) => Some(&transition.subject),
            TransactionStep::AdvanceCursor(advance) => Some(&advance.target),
            TransactionStep::Fence(fence) => Some(&fence.target),

            TransactionStep::Insert(_)
            | TransactionStep::EstablishEffectIntent(_)
            | TransactionStep::EstablishTransactionOutput(_)
            | TransactionStep::WriteOutbox(_) => None,
        }
    }

    /// The data model a transaction's access to `object` is against:
    /// the transaction's own, or the object's where it declares none.
    fn transaction_data_model(&self, transaction: &TransactionRef, object: &Id) -> Id {
        transaction_declaration(self.model, &transaction.operation, &transaction.transaction)
            .and_then(|declared| declared.data_model.clone())
            .unwrap_or_else(|| self.object_data_model(object))
    }

    /// The data model declaring an object, resolved through the global
    /// id namespace. An unresolvable object, possible only in a model
    /// that fails validation, is stated against an empty id.
    fn object_data_model(&self, object: &Id) -> Id {
        self.model
            .data_models
            .iter()
            .find(|(_, declared)| declared.objects.contains_key(object))
            .map(|(id, _)| id.clone())
            .unwrap_or_else(|| Id(String::new()))
    }

    /// Every transaction of the model advancing or fencing a managed
    /// field: the transactions an ordering route's cursor or fence fact
    /// is read from.
    fn managed_field_writers(&self, object: &Id, field: &FieldPath) -> Vec<DeclarationKey> {
        let mut writers = Vec::new();

        for (operation_id, operation) in &self.model.operations {
            for (_, transaction) in operation.program.transactions() {
                let writes = transaction.steps.iter().any(|step| match step {
                    TransactionStep::AdvanceCursor(advance) => {
                        &advance.target.object == object && &advance.field == field
                    }
                    TransactionStep::Fence(fence) => {
                        &fence.target.object == object && &fence.field == field
                    }
                    _ => false,
                });

                if writes {
                    writers.push(DeclarationKey::Transaction {
                        operation: operation_id.clone(),
                        transaction: transaction.id.clone(),
                    });
                }
            }
        }

        writers
    }

    fn input(&self, operation: &Id, input: &Id) -> Option<&'a Input> {
        self.model.operations.get(operation)?.inputs.get(input)
    }

    /// The channel a message-driven input consumes.
    fn input_channel(&self, operation: &Id, input: &Id) -> Option<Channel> {
        match self.input(operation, input)? {
            Input::Request(_) => None,

            Input::Subscription(subscription) => Some(Channel::Topic {
                topic: subscription.topic.clone(),
            }),

            Input::Outbox(consumer) => self.outbox_channel(&consumer.outbox),
        }
    }

    /// The declaration of the channel an input consumes, if it is
    /// message-driven.
    fn channel_anchor(&self, operation: &Id, input: &Id) -> Option<DeclarationKey> {
        Some(match self.input_channel(operation, input)? {
            Channel::Topic { topic } => DeclarationKey::Topic(topic),
            Channel::Outbox { data_model, outbox } => DeclarationKey::Outbox { data_model, outbox },
        })
    }

    /// The schemas a message-driven input admits.
    fn admitted_schemas(&self, operation: &Id, input: &Id) -> Vec<Id> {
        match self.input(operation, input) {
            Some(Input::Subscription(subscription)) => match &subscription.messages {
                MessageSelector::Only(messages) => messages.iter().cloned().collect(),

                MessageSelector::All => self
                    .model
                    .topics
                    .get(&subscription.topic)
                    .map(|topic| topic.messages.iter().cloned().collect())
                    .unwrap_or_default(),
            },

            Some(Input::Outbox(consumer)) => self
                .model
                .outbox(&consumer.outbox)
                .map(|(_, outbox)| outbox.messages.iter().cloned().collect())
                .unwrap_or_default(),

            Some(Input::Request(_)) | None => Vec::new(),
        }
    }

    fn outbox_channel(&self, outbox: &Id) -> Option<Channel> {
        let (data_model, _) = self.model.outbox(outbox)?;

        Some(Channel::Outbox {
            data_model: data_model.clone(),
            outbox: outbox.clone(),
        })
    }

    fn outbox_anchor(&self, outbox: &Id) -> Option<DeclarationKey> {
        let (data_model, _) = self.model.outbox(outbox)?;

        Some(DeclarationKey::Outbox {
            data_model: data_model.clone(),
            outbox: outbox.clone(),
        })
    }

    /// The declaration owning an effect: the operation's effect site,
    /// or the transition whose side effect or outbox admission it is.
    fn effect_anchor(&self, operation: &Id, effect: &Id) -> DeclarationKey {
        let owned = self
            .model
            .operations
            .get(operation)
            .is_some_and(|declared| {
                declared
                    .program
                    .effect_declarations()
                    .iter()
                    .any(|(id, _)| *id == effect)
                    || declared
                        .program
                        .outbox_write_declarations()
                        .iter()
                        .any(|(_, write)| &write.effect_id == effect)
            });

        if !owned {
            for (machine_id, machine) in &self.model.state_machines {
                for (transition_id, transition) in &machine.transitions {
                    if transition.side_effects.contains_key(effect)
                        || transition.effects.contains_key(effect)
                    {
                        return DeclarationKey::Transition {
                            machine: machine_id.clone(),
                            transition: transition_id.clone(),
                        };
                    }
                }
            }
        }

        DeclarationKey::EffectSite {
            operation: operation.clone(),
            effect: effect.clone(),
        }
    }

    /// The declared identity of an external effect of the obligation's
    /// operation.
    fn external_identity(&self, effect: &Id) -> Option<ExternalIdentity> {
        self.model
            .operations
            .get(self.operation)?
            .program
            .effect_declarations()
            .into_iter()
            .find_map(|(id, declared)| match declared {
                Effect::External(external) if id == effect => Some(external.identity.clone()),
                _ => None,
            })
    }

    /// The key a transaction declares its commits deduplicated by.
    fn declared_commit_key(&self, transaction: &Id) -> Option<Vec<ValueRef>> {
        match &transaction_declaration(self.model, self.operation, transaction)?.idempotency {
            IdempotencyGuarantee::DeduplicatedBy { key } => Some(key.components.clone()),
            IdempotencyGuarantee::Unspecified | IdempotencyGuarantee::NotDeduplicated => None,
        }
    }

    /// The idempotency obligations of `operation` keyed from `input`,
    /// as the verifier aggregates a target or consumer requirement by
    /// operation and input; the proven ones only, when asked.
    fn idempotency_obligations(
        &self,
        operation: &Id,
        input: &Id,
        proven: bool,
    ) -> Vec<ObligationKey> {
        let Some(declared) = self.model.operations.get(operation) else {
            return Vec::new();
        };

        let keys = idempotency_keys(operation, declared, ObligationProperty::Idempotency);

        self.verification
            .idempotency
            .iter()
            .filter(|check| &check.operation == operation && key_input(&check.key) == Some(input))
            .filter(|check| {
                !proven
                    || match &check.verdict {
                        IdempotencyVerdict::Proven { .. } => true,
                        IdempotencyVerdict::Unproven { .. } => false,
                    }
            })
            .filter_map(|check| keys.get(check.requirement).cloned())
            .collect()
    }

    /// The result replay obligation of an operation's idempotency
    /// requirement.
    fn result_obligation(&self, operation: &Id, index: usize) -> Option<ObligationKey> {
        let declared = self.model.operations.get(operation)?;

        idempotency_keys(operation, declared, ObligationProperty::ResultReplay)
            .get(index)
            .cloned()
    }

    /// The result replay obligation of the target a request effect of
    /// the obligation's operation sends into.
    fn target_result_obligation(&self, effect: &Id) -> Option<ObligationKey> {
        let declared = self.model.operations.get(self.operation)?;

        let target = declared
            .program
            .effect_declarations()
            .into_iter()
            .find_map(|(id, declared)| match declared {
                Effect::Request(request) if id == effect => Some(&request.target),
                _ => None,
            })
            .or_else(|| {
                self.model.state_machines.values().find_map(|machine| {
                    machine.transitions.values().find_map(|transition| {
                        match transition.side_effects.get(effect)? {
                            TransitionSideEffect::Request(request) => Some(&request.target),
                            TransitionSideEffect::Publication(_) => None,
                        }
                    })
                })
            })?;

        let target_operation = self.model.operations.get(&target.operation)?;
        let index = returns_consistently(target_operation, &target.input)?;

        self.result_obligation(&target.operation, index)
    }
}

/// The inline transaction an operation declares under an id.
fn transaction_declaration<'m>(
    model: &'m Model,
    operation: &Id,
    transaction: &Id,
) -> Option<&'m Transaction> {
    model
        .operations
        .get(operation)?
        .program
        .transaction(transaction)
}

fn transaction_id(reference: &TransactionRef) -> TransactionId {
    TransactionId {
        operation: reference.operation.clone(),
        transaction: reference.transaction.clone(),
    }
}

fn transaction_anchor(reference: &TransactionRef) -> DeclarationKey {
    DeclarationKey::Transaction {
        operation: reference.operation.clone(),
        transaction: reference.transaction.clone(),
    }
}

fn object_anchor(data_model: &Id, object: &Id) -> DeclarationKey {
    DeclarationKey::DataObject {
        data_model: data_model.clone(),
        object: object.clone(),
    }
}

fn input_key(operation: &Id, input: &Id) -> DeclarationKey {
    DeclarationKey::Input {
        operation: operation.clone(),
        input: input.clone(),
    }
}

fn subscription_anchor(operation: &Id, input: &Id) -> DeclarationKey {
    DeclarationKey::SubscriptionRuntime {
        operation: operation.clone(),
        input: input.clone(),
    }
}

/// The DSL step that performs a guarded mutation.
fn mechanism_of(mechanism: ConditionalMutationKind) -> Mechanism {
    match mechanism {
        ConditionalMutationKind::CompareAndSet => Mechanism::CompareAndSet,
        ConditionalMutationKind::Transition => Mechanism::Transition,
        ConditionalMutationKind::AdvanceCursor => Mechanism::AdvanceCursor,
        ConditionalMutationKind::Fence => Mechanism::Fence,
        ConditionalMutationKind::UpsertIdentityArbitration => Mechanism::Upsert,
    }
}
