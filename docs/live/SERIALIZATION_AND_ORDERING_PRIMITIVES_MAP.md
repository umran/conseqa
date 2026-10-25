# Serialization and Ordering Primitives: Documentation Source Map

## Purpose

This document is a compact map for a future documentation-generating agent. It identifies the framework's serialization and ordering vocabulary, the implementation that gives each item meaning, and the tests that demonstrate the intended behavior. It is not a substitute for the normative semantics in [`CONSEQA_DSL_SEMANTICS.md`](CONSEQA_DSL_SEMANTICS.md), especially §§17 and 20–22.

## Motivation

Correctness and scalability are often in contention in data-intensive applications. Scalable architecture designs frequently cannot preserve the correctness assumptions of naive abstract computation models: concurrency, retries, distribution, delayed work, and independently scheduled operations break the illusion that state changes occur in one simple sequential order. This necessitates introducing serialization and ordering primitives directly into the state mutation model.

Conseqa attempts to provide a minimal set of such primitives while maximizing the expressibility of correctness constraints across a wide domain of potential applications. The model therefore distinguishes concrete implementation guarantees—such as isolation, strict locks, atomic conditional mutations, cursors, and fences—from the serializability and ordering requirements those guarantees are used to prove.

## Normative entry points

| Subject | Primary source |
|---|---|
| Complete semantic contract | [`CONSEQA_DSL_SEMANTICS.md`](CONSEQA_DSL_SEMANTICS.md) |
| DSL 6 revision rationale and acceptance cases | [`Conseqa_Atomic_Mutation_and_Serialization_Primitives_Revision_Specification_DSL_v6.md`](../../Conseqa_Atomic_Mutation_and_Serialization_Primitives_Revision_Specification_DSL_v6.md) |
| Transaction DSL types | [`src/spec/operation/transaction.rs`](../../src/spec/operation/transaction.rs) |
| Object identity and version declarations | [`src/spec/data_model.rs`](../../src/spec/data_model.rs) |

## Requirements the primitives prove

| Requirement | Meaning | DSL type | Prover |
|---|---|---|---|
| `SerializableBy(K)` | Committed executions in each key domain, together with their conflict closure, admit an equivalent serial order. | [`TransactionSerializabilityRequirement`](../../src/spec/operation/transaction.rs#L95) | [`transaction_serializability::prove`](../../src/analyzer/verification/transaction_serializability.rs#L208) |
| `OrderedBy(K, P)` | Committed executions in each key domain take effect in position order; serializability is a prerequisite. | [`TransactionOrderingRequirement`](../../src/spec/operation/transaction.rs#L113) | [`transaction_ordering`](../../src/analyzer/verification/transaction_ordering.rs) |

These declarations are proof obligations, not guarantees and not synchronization operations.

## Author-facing serialization and ordering primitives

| Primitive | Minimal semantic role | DSL/model source | Validation and proof implementation | Primary tests |
|---|---|---|---|---|
| Transaction boundary and step order | Defines the atomic commit/reject/interruption boundary and the program order used for protection coverage. | [`Transaction` and `TransactionStep`](../../src/spec/operation/transaction.rs) | [`validate_transactions`](../../src/analyzer/validation/mod.rs); transaction indexing in [`ConflictIndex`](../../src/analyzer/verification/transaction_conflicts.rs#L368) | [`tests/transaction_requirements.rs`](../../tests/transaction_requirements.rs), [`tests/validation.rs`](../../tests/validation.rs) |
| `isolation: unspecified` | Supplies no isolation evidence. | [`TransactionIsolation`](../../src/spec/operation/transaction.rs#L120) | Dependency-edge evaluation in [`transaction_conflicts.rs`](../../src/analyzer/verification/transaction_conflicts.rs) | [`tests/transaction_requirements.rs`](../../tests/transaction_requirements.rs) |
| `isolation: read_committed` | Excludes dirty reads; does not exclude non-repeatable reads, lost updates, or write skew. | [`TransactionIsolation`](../../src/spec/operation/transaction.rs#L120) | Committed-read and write-order evidence in [`transaction_conflicts.rs`](../../src/analyzer/verification/transaction_conflicts.rs) | [`tests/transaction_requirements.rs`](../../tests/transaction_requirements.rs) |
| `isolation: snapshot` | Makes ordinary reads observe one committed snapshot; does not by itself exclude write skew. | [`TransactionIsolation`](../../src/spec/operation/transaction.rs#L120) | Snapshot read-only evidence in [`ConflictIndex::serializes_at_its_read`](../../src/analyzer/verification/transaction_conflicts.rs#L1218) | `a_read_only_observation_at_one_instant_serializes_at_its_read` in [`tests/transaction_requirements.rs`](../../tests/transaction_requirements.rs) |
| `isolation: serializable` | Provides serial execution semantics only when every transaction in the relevant conflict closure participates. | [`TransactionIsolation`](../../src/spec/operation/transaction.rs#L120) | Serializable-closure route in [`transaction_serializability.rs`](../../src/analyzer/verification/transaction_serializability.rs) | `serializable_isolation_across_the_closure_proves` and `one_weaker_transaction_in_the_closure_defeats_the_isolation_route` in [`tests/transaction_requirements.rs`](../../tests/transaction_requirements.rs) |
| Shared strict lock | Protects a read after acquisition and conflicts with an exclusive lock on the covered target. | [`Lock`](../../src/spec/operation/transaction.rs#L523) and `LockMode::Shared` | Lock coverage and acquisition timing in [`transaction_conflicts.rs`](../../src/analyzer/verification/transaction_conflicts.rs) | Shared-lock and late-lock cases in [`tests/transaction_requirements.rs`](../../tests/transaction_requirements.rs) |
| Exclusive strict lock | Protects reads and writes after acquisition and conflicts with shared and exclusive locks on the covered target. | [`Lock`](../../src/spec/operation/transaction.rs#L523) and `LockMode::Exclusive` | Lock coverage and strict-lock evidence in [`transaction_conflicts.rs`](../../src/analyzer/verification/transaction_conflicts.rs) | Exclusive-lock and write-skew cases in [`tests/transaction_requirements.rs`](../../tests/transaction_requirements.rs) |
| Lock acquisition order | Declares the order in which one lock step acquires multiple selected instances; intended for deadlock reasoning, not business-event ordering. | `LockOrder` and `OrderingTerm` in [`transaction.rs`](../../src/spec/operation/transaction.rs) | Lock validation in [`validation/mod.rs`](../../src/analyzer/validation/mod.rs) | Lock-order validation cases in [`tests/validation.rs`](../../tests/validation.rs) |
| Object identity | Defines one logical persistent instance and the arbitration domain used by selectors, locks, compare-and-set, upsert, cursors, and fences. | [`DataObject`](../../src/spec/data_model.rs#L58) | Identity/selector validation in [`validation/mod.rs`](../../src/analyzer/validation/mod.rs); selector overlap in [`transaction_conflicts.rs`](../../src/analyzer/verification/transaction_conflicts.rs) | Identity and selector cases in [`tests/validation.rs`](../../tests/validation.rs) and [`tests/transaction_requirements.rs`](../../tests/transaction_requirements.rs) |
| Intrinsic object version | A managed integer token established on insertion and advanced by every committed mutation of a live instance; useful only when a guarded mutation compares it. | [`ObjectVersion`](../../src/spec/data_model.rs#L109) | Version-field validation in [`validation/mod.rs`](../../src/analyzer/validation/mod.rs); publication and observed-version evidence in [`transaction_conflicts.rs`](../../src/analyzer/verification/transaction_conflicts.rs) | Version publication, observed-version, direct-assignment, and delete/reinsert cases in [`tests/transaction_requirements.rs`](../../tests/transaction_requirements.rs) |
| `compare_and_set` | Atomically compares fields and mutates one fully identified existing instance; mismatch or absence rejects the transaction. | [`CompareAndSet`](../../src/spec/operation/transaction.rs#L430) and `CompareCondition` | [`validate_guarded_mutations`](../../src/analyzer/validation/mod.rs#L1285); observation matching in [`observation`](../../src/analyzer/verification/transaction_conflicts.rs#L1714); guard evidence in [`ConflictIndex::observation_guard`](../../src/analyzer/verification/transaction_conflicts.rs#L1269) | CAS structure, rejection, observed-field, and observed-version cases in [`tests/transaction_requirements.rs`](../../tests/transaction_requirements.rs) |
| Observed-field guard | A guarded mutation compares a field with the same field from an earlier read of the same identified instance; coverage is limited to the compared conflict regions and that read. | `CompareCondition` in [`transaction.rs`](../../src/spec/operation/transaction.rs) | [`observation`](../../src/analyzer/verification/transaction_conflicts.rs#L1714) and [`ConflictIndex::observation_guard`](../../src/analyzer/verification/transaction_conflicts.rs#L1269) | `an_observed_field_compare_and_set_constrains_the_anti_dependency` in [`tests/transaction_requirements.rs`](../../tests/transaction_requirements.rs) |
| Observed-version guard | A guarded mutation compares the object's version with an earlier observed version; it covers observations of the same live instance up to the guard, subject to delete/reinsert token repetition. | `ObjectVersion` plus `CompareCondition` | Observed-version branch in [`ConflictIndex::observation_guard`](../../src/analyzer/verification/transaction_conflicts.rs#L1269) | Observed-version and repeating-version cases in [`tests/transaction_requirements.rs`](../../tests/transaction_requirements.rs) |
| State-machine `transition` | Atomically tests the declared `from` state, changes it to `to`, and may conjoin additional comparisons; failure rejects. | [`StateTransition`](../../src/spec/operation/transaction.rs#L898) and state-machine declarations in [`src/spec`](../../src/spec) | Transition indexing in [`transaction_conflicts.rs`](../../src/analyzer/verification/transaction_conflicts.rs); state-machine validation in [`validation/mod.rs`](../../src/analyzer/validation/mod.rs) | Transition guard and observed-version cases in [`tests/transaction_requirements.rs`](../../tests/transaction_requirements.rs) |
| `upsert` identity arbitration | Atomically inserts an absent identified instance or updates an existing one. It protects its own arbitration and subsequent covered accesses, but no earlier read. | [`Upsert`](../../src/spec/operation/transaction.rs#L491) | [`validate_guarded_mutations`](../../src/analyzer/validation/mod.rs#L1285); upsert access and conditional-mutation indexing in [`transaction_conflicts.rs`](../../src/analyzer/verification/transaction_conflicts.rs) | Upsert identity, conflicts, and no-earlier-read coverage in [`tests/transaction_requirements.rs`](../../tests/transaction_requirements.rs) |
| `advance_cursor` with `successor` | Accepts only `incoming = stored + 1`; rejects stale, duplicate, and gapped positions. | [`AdvanceCursor`](../../src/spec/operation/transaction.rs#L969) and `CursorAdvanceRule::Successor` | Managed-field validation in [`validate_managed_fields`](../../src/analyzer/validation/mod.rs#L1392); cursor evidence in [`transaction_conflicts.rs`](../../src/analyzer/verification/transaction_conflicts.rs); ordering proof in [`transaction_ordering.rs`](../../src/analyzer/verification/transaction_ordering.rs) | Successor ordering cases in [`tests/transaction_requirements.rs`](../../tests/transaction_requirements.rs) |
| `advance_cursor` with `monotonic_after` | Accepts any incoming position strictly greater than the stored position; gaps are allowed. | [`AdvanceCursor`](../../src/spec/operation/transaction.rs#L969) and `CursorAdvanceRule::MonotonicAfter` | Same cursor validation and proof paths as `successor` | `a_monotonic_cursor_proves_ordering_too` in [`tests/transaction_requirements.rs`](../../tests/transaction_requirements.rs) |
| Cursor comparisons | Conjoins observed-state or application comparisons with the cursor rule in the same atomic mutation; ordering and observation guarding remain distinct proof roles. | `AdvanceCursor.compare` in [`transaction.rs`](../../src/spec/operation/transaction.rs#L969) | Common comparison validation and conditional-mutation indexing | Guard-composition cases in [`tests/transaction_requirements.rs`](../../tests/transaction_requirements.rs) |
| `fence` | Rejects authority tokens older than the persisted token and advances it for newer tokens; equal tokens remain valid. It supports ordering after serializability is established, but is not serializability evidence by itself. | [`Fence`](../../src/spec/operation/transaction.rs#L1025) | Managed-field validation in [`validate_managed_fields`](../../src/analyzer/validation/mod.rs#L1392); fence ordering in [`transaction_ordering.rs`](../../src/analyzer/verification/transaction_ordering.rs); the no-held-protection rule in [`ConditionalMutationKind`](../../src/analyzer/verification/transaction_conflicts.rs#L250) | Fence ordering and no-commit-order-evidence cases in [`tests/transaction_requirements.rs`](../../tests/transaction_requirements.rs) |

## Derived proof evidence

These are analyzer conclusions, not additional DSL primitives.

| Evidence | Derivation source |
|---|---|
| `intrinsic_committed_read` | A read under declared isolation observes only committed writes. |
| `atomic_write_order` | Conflicting writes by transactions with declared isolation are installed in commit order. |
| `strict_lock` | Covering reader and writer locks were acquired before their accesses and are held to transaction end. |
| `atomic_conditional_mutation` | A compare-and-set, transition, cursor advance, or upsert supplies atomic or held-protection evidence; observed comparisons may additionally cover an earlier read. |
| `read_only_observation` | A write-free transaction observes at one instant: one identified read under declared isolation, or any ordinary reads under snapshot isolation. |
| `ordered_cursor` | Two advances of the same managed cursor under the same rule are ordered by accepted positions. |

The evidence data model is [`CommitOrderEvidence`](../../src/analyzer/verification/transaction_conflicts.rs#L2038), with conditional-mutation coverage described by [`GuardCoverage`](../../src/analyzer/verification/transaction_conflicts.rs#L2099). Human-readable proof explanations are generated by [`transaction_serializability::evidence_sentence`](../../src/analyzer/verification/transaction_serializability.rs#L549).

## Baseline state mutations that are not serialization guards

| Step | Relevant limitation |
|---|---|
| `read` | Observes state; it does not protect the state unless isolation, a prior lock, a later observed-state guard, or the read-only rule supplies evidence. |
| `update` | Unconditional mutation; never rejects because an earlier observation became stale. |
| `insert` | Creates an identity; it does not protect an earlier read or range query. |
| `delete` | Removes selected instances; it leaves no surviving version token. |

Their DSL definitions are in [`src/spec/operation/transaction.rs`](../../src/spec/operation/transaction.rs), and their conflict footprints are indexed in [`src/analyzer/verification/transaction_conflicts.rs`](../../src/analyzer/verification/transaction_conflicts.rs).

## Facts deliberately excluded from state serialization proofs

Do not document any of the following as state-serialization primitives:

- transport or queue ordering;
- routing and partition affinity;
- member assignment;
- worker or pool concurrency bounds;
- operation or service boundaries;
- transaction commit deduplication or idempotency keys;
- effect idempotency;
- a version declaration without a comparison;
- an unconditional `update`;
- an `upsert` as protection for a preceding read;
- a `fence` as serializability evidence between equal-token executions.

Runtime topology and state serialization are intentionally separated by the normative contract. The analyzer's conflict graph and accepted evidence are implemented in [`src/analyzer/verification/transaction_conflicts.rs`](../../src/analyzer/verification/transaction_conflicts.rs).

## Diagnostics, reports, and visualization

| Surface | Source |
|---|---|
| Validation error definitions and messages | [`src/analyzer/validation/error.rs`](../../src/analyzer/validation/error.rs) |
| Diagnostic codes | [`src/analyzer/diagnostic.rs`](../../src/analyzer/diagnostic.rs) |
| Verification report types | [`src/analyzer/report.rs`](../../src/analyzer/report.rs) |
| Serializability explanations | [`src/analyzer/verification/transaction_serializability.rs`](../../src/analyzer/verification/transaction_serializability.rs) |
| Ordering explanations | [`src/analyzer/verification/transaction_ordering.rs`](../../src/analyzer/verification/transaction_ordering.rs) |
| Published serialization-graph report | [`TRANSACTION_SERIALIZATION_GRAPH.md`](TRANSACTION_SERIALIZATION_GRAPH.md) |
| Visualization proof adapter | [`src/viz/transaction_proofs.rs`](../../src/viz/transaction_proofs.rs) |
| Visualization TypeScript model | [`viz/src/types/transactionProofs.ts`](../../viz/src/types/transactionProofs.ts) |

## Recommended documentation-generation path

1. Treat [`CONSEQA_DSL_SEMANTICS.md`](CONSEQA_DSL_SEMANTICS.md) as normative.
2. Use the Rust DSL types to enumerate the exact authored vocabulary and defaults.
3. Use validation to describe structural restrictions and diagnostic names.
4. Use `transaction_conflicts.rs` to describe conflict footprints, coverage, and derived evidence.
5. Use the serializability and ordering provers to describe what each requirement accepts.
6. Use `tests/transaction_requirements.rs` for worked examples and boundary cases.
7. Use the report and visualization surfaces for the terminology exposed to users.
