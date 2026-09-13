# Conseqa Transaction Consistency and Ordering Revision

**Target baseline:** `977795de4901144c541d77967f5f30b7df9042cb`  
**New DSL contract:** `DSL_VERSION = 4`

## 1. Purpose

This revision replaces Conseqa's operation-level serialization and ordering proof system with transaction-level consistency requirements enforced at the persistent-state boundary.

The revision introduces:

- transaction serializability requirements;
- transaction ordering requirements;
- explicit transaction rejection control flow;
- S/X transactional-lock proof semantics;
- application-managed object versions with optimistic validation;
- ordered state cursors;
- fencing tokens;
- model-wide transaction conflict analysis;
- serializable-isolation closure analysis;
- explicit logical error classes;
- transition-scoped atomic Outbox admission.

The revision removes:

- operation-level serialization requirements;
- operation-level ordering requirements;
- operation-level `InvocationLock`;
- topology-derived correctness proofs;
- `ExecutionPool.execution_handoff`;
- all serialization/ordering proof semantics based on routing, member assignment, or member concurrency.

The governing principle becomes:

> L1 describes runtime topology and transport behavior. Transaction correctness is established at the L0 transactional state boundary.

---

# 2. Semantic properties

Conseqa v4 distinguishes three independent concerns.

## 2.1 Transaction serializability

```text
SerializableBy(K)
```

means:

> For committed executions of the transaction associated with equal logical key `K`, together with every conflicting transaction execution in the relevant conflict closure, the admitted persistent-state history is equivalent to some serial transaction history.

Physical transaction execution may overlap.

Aborted, rejected, or indeterminate attempts do not appear as committed transactions in that history.

---

## 2.2 Transaction ordering

```text
OrderedBy(K, P)
```

means:

> For committed transaction executions sharing key `K`, meaningful precedence established by position `P` is respected by persistent-state application.

For:

```text
P1 < P2
```

a committed application of `P2` may not logically precede the committed application of `P1`.

`OrderedBy(K,P)` requires serializable state application for the relevant domain.

It is stronger than ordinary transaction serializability:

```text
OrderedBy(K,P)
    => serializable state application

SerializableBy(K)
    -/-> OrderedBy(K,P)
```

Ordering says which serial order is admissible.

---

## 2.3 Operation correctness

Operations no longer declare serialization or ordering requirements.

Operation-level concerns remain:

```text
idempotency
recoverability
result replay
causal program structure
```

Conseqa makes no whole-operation claim that:

- same-key invocations cannot overlap;
- an entire distributed operation executes serially;
- effects execute in transaction order;
- worker failover preserves invocation exclusion.

---

# 3. Remove operation-level serialization and ordering

## 3.1 Current v3

```rust
pub struct OperationRequirements {
    pub serialization: Vec<SerializationRequirement>,
    pub ordering: Vec<OrderingRequirement>,
    pub idempotency: Vec<IdempotencyRequirement>,
    pub recoverability: Vec<RecoverabilityRequirement>,
}
```

## 3.2 Revised v4

```rust
pub struct OperationRequirements {
    pub idempotency: Vec<IdempotencyRequirement>,
    pub recoverability: Vec<RecoverabilityRequirement>,
}
```

Remove:

```rust
SerializationRequirement
OrderingRequirement
```

entirely.

Old declarations are invalid v4 DSL.

There is no compatibility alias or migration parser.

---

# 4. Remove `InvocationLock`

Remove:

```rust
Operation {
    ...
    invocation_lock: Option<InvocationLock>,
}
```

and:

```rust
pub struct InvocationLock {
    pub key: ValueRef,
}
```

Also remove:

```text
InvocationLockKeyNotFromInput
InvocationLockKeyNotEvaluable
SerializationProof::InvocationLocked
```

and all validation, reporting, visualization, fixture, and documentation support associated with them.

Rationale:

`InvocationLock` existed to discharge operation-level `SerializedBy(K)`. That obligation no longer exists.

Where application correctness requires exclusion around transactional state, transaction `Lock` is the relevant primitive.

---

# 5. Remove topology correctness semantics

## 5.1 Remove execution handoff

Delete:

```rust
pub struct ExecutionPool {
    pub member_concurrency: MemberConcurrency,
    pub execution_handoff: Option<ExecutionHandoff>,
}
```

and replace it with:

```rust
pub struct ExecutionPool {
    pub member_concurrency: MemberConcurrency,
}
```

Delete:

```rust
pub enum ExecutionHandoff {
    ExclusiveOwnership,
}
```

No equivalent replacement is introduced.

---

## 5.2 `MemberAssignment` becomes placement-only

Retain:

```rust
pub enum MemberAssignment {
    ConsistentHash,
    RoundRobin,
}
```

Normative semantics:

### `ConsistentHash`

> Within a stable membership/assignment view, equal routing domains map to the same execution-pool member.

It asserts no property across:

- member failure;
- process replacement;
- autoscaling;
- ownership transfer;
- partition reassignment;
- rebalance;
- stale worker execution.

### `RoundRobin`

> Runtime invocations are assigned in rotational member order without semantic-key affinity.

Neither variant participates in correctness proofs.

---

## 5.3 Retain `MemberConcurrency`

Retain:

```rust
pub enum MemberConcurrency {
    Unspecified,
    Bounded(NonZeroU32),
    Unbounded,
}
```

Its semantics become purely runtime-topological:

> `member_concurrency` describes simultaneous invocation capacity of one realized member.

It does not establish:

```text
transaction serializability
transaction ordering
stale-worker exclusion
cross-incarnation exclusion
commit ordering
```

---

# 6. Retain transport ordering as a transport fact

Retain:

```text
TopicRuntime.ordering
SubscriptionRuntime.ordering
OutboxRuntime.ordering
BatchOrderingPreservation
```

These remain meaningful L1 facts.

They establish transport/dispatch precedence only.

Explicitly:

```text
transport ordering
    -/-> transaction commit ordering

transport ordering
    -/-> transaction serializability

transport ordering
    -/-> operation serialization
```

They remain useful for:

- architecture description;
- simulation;
- latency/contention analysis;
- Confluence realization;
- identifying expected input precedence.

They are not sufficient correctness evidence.

---

# 7. Add transaction requirements

Revise `Transaction`:

```rust
pub struct Transaction {
    pub id: Id,
    pub data_model: Option<Id>,
    pub isolation: TransactionIsolation,
    pub idempotency: IdempotencyGuarantee,

    #[serde(default)]
    pub requirements: TransactionRequirements,

    pub steps: Vec<TransactionStep>,
}
```

Add:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransactionRequirements {
    #[serde(default)]
    pub serializability: Vec<TransactionSerializabilityRequirement>,

    #[serde(default)]
    pub ordering: Vec<TransactionOrderingRequirement>,
}
```

---

# 8. Transaction serializability requirement

```rust
pub struct TransactionSerializabilityRequirement {
    pub key: ValueRef,
}
```

Canonical rendering:

```text
SerializableBy(K)
```

The key identifies the logical conflict domain whose equal-valued executions are subject to the obligation.

The key MUST be available when the transaction begins.

It may derive from values already available at that operation-program point, including:

```text
input
prior transaction output
synchronous result already bound on the reaching path
```

It may not derive from a `transaction_read` performed inside the same transaction.

---

# 9. Transaction ordering requirement

```rust
pub struct TransactionOrderingRequirement {
    pub key: ValueRef,
    pub position: ValueRef,
}
```

Canonical rendering:

```text
OrderedBy(K, P)
```

`key` identifies the independent ordering domain.

`position` identifies meaningful application precedence within that domain.

Both MUST be available at transaction entry.

V4 ordering positions MUST resolve to a non-optional ordered scalar.

Initially supported:

```text
int
decimal
timestamp
```

`float` is excluded because NaN and implementation-specific comparison behavior make it unsuitable as a semantic total order.

`uuid`, `bool`, structured schemas, and lists are not order positions.

---

# 10. Transaction execution becomes explicitly rejectable

## 10.1 Current

```rust
OperationStep::Transaction(Transaction)
```

## 10.2 Revised

```rust
OperationStep::Transaction(ExecuteTransaction)
```

with:

```rust
pub struct ExecuteTransaction {
    pub transaction: Transaction,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rejected: Option<OperationBlock>,
}
```

---

# 11. Three transaction outcome classes

A transaction attempt has three semantically distinct classes:

```text
Committed
Rejected
Interrupted / Indeterminate
```

## Committed

The transaction commits its mutations and transaction artifacts atomically.

Normal operation control continues after the transaction step.

## Rejected

A modeled logical commit guard conclusively fails.

Examples:

```text
state-transition `from` guard mismatch
version validation mismatch
cursor constraint failure
stale fencing token
```

No transaction mutation or artifact commits.

The `rejected` operation block executes.

## Interrupted / indeterminate

Examples:

```text
database connection loss
deadlock victim abort
serialization failure
process crash
unknown commit outcome
infrastructure timeout
```

These do not enter `rejected`.

They are execution/recoverability phenomena.

They are not application `Err` results.

---

# 12. Rejected branch requirement

A transaction containing any potentially rejecting step MUST provide:

```text
rejected: ...
```

A transaction with no logically rejecting steps MUST NOT provide it.

Initially rejecting steps are:

```text
Transition
ValidateVersion
AdvanceCursor
Fence
```

Diagnostics:

```text
MissingTransactionRejectedArm
UnexpectedTransactionRejectedArm
```

A generic rejection arm is sufficient in v4.

The particular rejection cause is not yet exposed as a first-class operation value.

Typed rejection causes may be introduced later if required.

---

# 13. Rejection branch control semantics

If a transaction rejects:

```text
transaction body stops immediately
all staged mutations roll back
transaction outputs are not established
effect intents are not established
ordinary OutboxWriteEffects are not admitted
transition-scoped OutboxWriteEffects are not admitted
```

Control enters:

```text
ExecuteTransaction.rejected
```

If that nested block terminates, the operation terminates.

If it falls through, control rejoins after the transaction step.

Transaction artifacts from the rejected transaction are unavailable on this path.

Existing definite-availability analysis must account for this branch.

---

# 14. State transitions become explicitly fallible

For:

```text
Transition:
    from = {A, B}
    to   = C
```

the transition means:

```text
if current state ∈ {A,B}:
    transition may continue normally

otherwise:
    reject containing transaction
```

A rejected transition:

- does not mutate the state machine;
- establishes no transition artifacts;
- admits none of its transition-scoped Outbox writes;
- causes the containing transaction to reject.

The state-machine transition itself does not implicitly return an operation `Err`.

The surrounding operation chooses the boundary result in its rejection branch.

---

# 15. Transition-scoped atomic Outbox admission

State-machine transitions may atomically admit Outbox messages as part of the transition's containing transaction.

Extend the transition declaration:

```rust
pub struct Transition {
    pub from: BTreeSet<Id>,
    pub to: Id,
    pub side_effects: BTreeMap<Id, TransitionSideEffect>,

    #[serde(default)]
    pub effects: BTreeMap<Id, TransitionEffect>,
}
```

Add:

```rust
pub enum TransitionEffect {
    OutboxWrite(OutboxWriteEffect),
}
```

and, on the applying `transition` transaction step, the derivation of each admitted message:

```rust
pub struct StateTransition {
    pub machine: Id,
    pub transition: Id,
    pub subject: ObjectSelector,
    pub effect_intents: BTreeMap<Id, TransitionEffectIntent>,

    #[serde(default)]
    pub effects: BTreeMap<Id, TransitionEffectApplication>,
}

pub struct TransitionEffectApplication {
    pub values: Derivation,
}
```

V4 deliberately keeps this surface narrow.

No direct effect-execution kind is valid as a transition effect.

## 15.1 Amendment: keyed effects and application-site derivations

The first draft of this section declared `effects: Vec<TransitionEffect>` on the transition and nothing on the applying step. The implementation keys the declaration by effect id and pairs it with a derivation map on the applying step. This amendment records the shape as shipped, and the reasons.

An admission is a message instance, and the model must know where the instance's values come from. That derivation cannot live on the state machine, which is shared by every operation that applies it and knows nothing about any one transaction's inputs or reads. It belongs at the applying step, evaluated in the enclosing transaction context — exactly where `effect_intents` already places the derivations of transition side effects. A step can refer to a declared effect only by name; pairing derivations positionally against a list would silently re-pair every applying site whenever the machine's list is reordered.

The effect id is also the admission site's stable identity, as an `effect_id` is for every other effect occurrence: lineage, diagnostics, proof evidence, visualization, and — decisively — the target of an `idempotency_key_propagation`, which names its effect as `effect:<id>`. A nameless admission could declare no propagation, so outbox lineage could not be traced through a transition-scoped write.

Finally, `Transition.side_effects` and `StateTransition.effect_intents` already form this exact pair of maps under an exact-match rule. Two adjacent collections of effects on one struct with different shapes would be one more thing to learn for no semantic gain.

Consequences:

- the effect id lives in the global identifier namespace, so a collision with any other id is `DuplicateId`, as for a side-effect id;
- `StateTransition.effects.keys()` must equal `Transition.effects.keys()`; a missing or unexpected derivation is `InvalidTransitionOutboxDerivation { transaction, transition, missing, unexpected }`, the outbox counterpart of `TransitionEffectIntentsMismatch`;
- a transition without outbox effects omits both maps;
- the admissions of one transition commit together, so the map's id order carries no meaning. A future transition effect kind for which declaration order mattered would need an ordering fact of its own.

Surface form:

```yaml
state_machines:
  machine.order_lifecycle:
    transitions:
      transition.order.mark_paid:
        from: [state.order.pending]
        to: state.order.paid
        side_effects: {}
        effects:
          effect.order.paid_admitted:
            kind: outbox_write
            outbox: outbox.order_events
            schema: schema.OrderPaid
            idempotency_key_propagation: []
```

```yaml
# the applying site, a step of tx.apply_payment
- kind: transition
  machine: machine.order_lifecycle
  transition: transition.order.mark_paid
  subject:
    object: object.order
    predicate:
      kind: eq
      field: order_id
      value: { source: input:input.apply_payment.captured, path: order_id }
  effect_intents: {}
  effects:
    effect.order.paid_admitted:
      values:
        kind: deterministic
        from:
          - { source: transaction_read:read.apply_payment.order, path: order_id }
          - { source: input:input.apply_payment.captured, path: event_id }
```

The atomicity contract of §16, the exclusions of §17, the provenance distinction of §18, and the proof treatment of §19 are unchanged by the shape.

---

# 16. Transition-effect atomicity

For:

```text
Transaction T
    Transition Pending -> Accepted
        OutboxWrite PaymentRequested
        OutboxWrite AuditAccepted
```

the logical commit unit is:

```text
state transition
+
PaymentRequested admission
+
AuditAccepted admission
+
all other containing-transaction mutations/artifacts
```

These all:

```text
commit together

or

roll back together
```

Conseqa therefore establishes:

```text
successful transition commit
    =>
all declared transition Outbox writes admitted

transition rejection
    =>
none admitted

transaction rollback/interruption
    =>
none logically committed
```

This is the same atomicity contract as a sibling transaction-level `OutboxWriteEffect`; the transition merely scopes the admission to successful application of that particular state transition.

---

# 17. Transition effects are not post-transition execution

`TransitionEffect::OutboxWrite` means:

```text
atomically persist/admit an Outbox message
```

It does not mean:

```text
execute the Outbox consumer
publish externally
call another operation synchronously
perform remote I/O
```

Therefore transition effects MUST NOT include:

```text
ExternalEffect
PublicationEffect
RequestEffect
ExecuteEffectIntent
ExecuteEffectIntentAsync
ExecuteEffectAsync
```

These cannot be made part of the transaction's atomic state-transition commit.

---

# 18. Relationship to ordinary `OutboxWriteEffect`

Both forms remain valid:

```text
Transaction:
    Transition ...
    OutboxWrite ...
```

and:

```text
Transaction:
    Transition:
        effects:
            OutboxWrite ...
```

Their semantic distinction is provenance.

A sibling transaction Outbox write is caused by the transaction program generally.

A transition-scoped Outbox write is admitted iff that particular transition is successfully applied.

For analysis and transaction atomicity, both are transaction-bound durable admissions.

---

# 19. Transition-scoped Outbox admission in proofs

A transition-scoped Outbox write does not create a database conflict edge for transaction serializability analysis.

The transition itself contributes:

```text
Read(state-machine state)
Write(state-machine state)
```

The Outbox admission is an atomic commit artifact associated with the transaction.

Therefore if:

```text
T1 < T2
```

is established at the transaction level and:

```text
T1 transition admits M1
T2 transition admits M2
```

then their logical admission events follow the transaction commit history.

Conseqa does not infer:

```text
Consume(M1) < Consume(M2)
```

unless separately established by the downstream Outbox runtime/application.

---

# 20. Transaction locking primitives

Retain:

```rust
pub enum LockMode {
    Shared,
    Exclusive,
}
```

Do **not** add an Update/U lock in v4.

---

# 21. Shared-lock semantics

For:

```text
Lock(S, target)
```

the transaction acquires a shared lock on the selected logical object domain at that program point.

Compatibility:

```text
S / S = compatible
S / X = conflict
```

The lock is held until transaction termination.

A shared lock can protect subsequent reads for serializability analysis.

It does not protect any observation made before acquisition.

---

# 22. Exclusive-lock semantics

For:

```text
Lock(X, target)
```

compatibility is:

```text
X / S = conflict
X / X = conflict
```

The lock is held until transaction termination.

It can protect subsequent reads and writes over the selected domain.

---

# 23. No Update lock

Do not add:

```text
Update
U
SharedThenUpgrade
```

in v4.

An update lock is primarily useful for implementation-level lock-upgrade and deadlock behavior.

It adds no required correctness power beyond acquiring `Exclusive` before a read-modify-write critical section.

Model:

```text
Lock(X, Account(K))
Read(Account(K))
Write(Account(K))
```

rather than:

```text
Lock(U)
Read
Upgrade(X)
Write
```

If lock-upgrade/deadlock modeling later requires U locks, add them deliberately.

---

# 24. Add application object versions

Extend `DataObject`:

```rust
pub struct DataObject {
    pub schema: Id,
    pub identity: Vec<FieldPath>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<ObjectVersion>,
}

pub struct ObjectVersion {
    pub field: FieldPath,
}
```

The version field MUST be:

```text
non-optional
int
not part of object identity
```

The field is managed by the version protocol.

Ordinary `Write` steps may not directly write it.

---

# 25. Version semantics

A versioned object provides one monotonically increasing application concurrency token.

A successful mutation of a live versioned object MUST advance its version.

Conseqa enforces:

```text
Insert
    -> creates initial version

Write / Transition
    -> must be accompanied by BumpVersion

Delete
    -> removes the versioned instance
```

A transition-scoped Outbox admission by itself does not count as a mutation of the transitioned `DataObject` beyond the transition state change already requiring its version bump where applicable.

Any model path that mutates a live versioned object without satisfying the protocol is invalid.

---

# 26. Add `ValidateVersion`

```rust
pub struct ValidateVersion {
    pub target: ObjectSelector,
    pub expected: ValueRef,
}
```

Add:

```rust
TransactionStep::ValidateVersion(ValidateVersion)
```

`expected` MUST derive from a preceding `transaction_read` of the same target's declared version field.

Semantics:

> The transaction may commit only if the object's current version at commit arbitration still equals `expected`.

Mismatch causes logical transaction rejection.

The check is a commit guard, not merely a non-atomic comparison performed at the step's wall-clock instant.

---

# 27. Add `BumpVersion`

```rust
pub struct BumpVersion {
    pub target: ObjectSelector,
}
```

Add:

```rust
TransactionStep::BumpVersion(BumpVersion)
```

On successful commit:

```text
version := version + 1
```

atomically with the surrounding transaction.

The version field is not directly assigned through a `Derivation`.

For one selected object instance, one transaction may bump it at most once.

---

# 28. OCC proof pattern

Example:

```text
Read GlobalLimit including version -> G17
Read Account(K) including version   -> A42

ValidateVersion(GlobalLimit, G17)
ValidateVersion(Account(K), A42)

Write Account(K)
BumpVersion(Account(K))
```

If a conflicting transaction commits a `GlobalLimit` mutation:

```text
G17 -> G18
```

before this transaction commits, `ValidateVersion(G17)` rejects the stale transaction.

Thus a stale read cannot silently participate in a successful commit.

This is usable serialization evidence.

---

# 29. Add ordered cursor primitive

Add:

```rust
pub struct AdvanceCursor {
    pub target: ObjectSelector,
    pub field: FieldPath,
    pub incoming: ValueRef,
    pub rule: CursorAdvanceRule,
}

pub enum CursorAdvanceRule {
    Successor,
    MonotonicAfter,
}
```

Add:

```rust
TransactionStep::AdvanceCursor(AdvanceCursor)
```

The cursor field MUST be a supported ordered scalar.

The primitive is the only ordinary update mechanism for a field that participates as a cursor in a serializability/ordering proof.

Insert initialization remains valid.

---

# 30. `Successor` cursor

For integer cursor:

```text
stored = S
incoming = P
```

the transaction may commit only when:

```text
P = S + 1
```

Then:

```text
cursor := P
```

commits atomically with the transaction.

Examples:

```text
stored 17, incoming 18
    -> admissible

stored 17, incoming 17
    -> reject stale/duplicate

stored 17, incoming 19
    -> reject gap
```

Both stale and gap conditions enter the generic transaction rejection arm in v4.

---

# 31. `MonotonicAfter` cursor

The transaction may commit only when:

```text
incoming > stored
```

and atomically performs:

```text
cursor := incoming
```

This permits gaps.

It is appropriate for:

```text
high-water marks
snapshot versions
monotonic log positions
superseding updates
```

It is not sufficient where every predecessor must be applied.

---

# 32. Add fencing primitive

```rust
pub struct Fence {
    pub target: ObjectSelector,
    pub field: FieldPath,
    pub token: ValueRef,
}
```

Add:

```rust
TransactionStep::Fence(Fence)
```

The field MUST be an ordered scalar.

Semantics for current fence `F` and incoming token `T`:

```text
T < F
    -> reject transaction

T = F
    -> authority remains valid

T > F
    -> atomically advance fence to T
```

Fence advancement commits atomically with the containing transaction.

---

# 33. Fence meaning

A fence asserts:

> State protected by the transaction cannot be mutated by an older authority generation after a newer generation has been accepted.

It does not claim the stale worker has terminated.

Example:

```text
worker A token 17
worker B token 18

B commits fence 18

A later attempts transaction with token 17
    -> rejected
```

This is precisely why fencing is preferred over reasoning from topology failover.

---

# 34. Managed monotonic fields

For proof soundness, build a model-wide index of fields used by:

```text
ObjectVersion
AdvanceCursor
Fence
```

A field may have only one managed semantic role.

Reject:

```text
same field used as both cursor and fence
ordinary Write mutating a version field
ordinary Write mutating a cursor field
ordinary Write mutating a fence field
```

Insert initialization is permitted.

Diagnostics:

```text
ManagedFieldRoleConflict
DirectWriteToManagedField
InvalidManagedFieldType
```

---

# 35. Transaction serializability analysis scope

Serializability is a system-wide property over potentially conflicting transaction executions.

For:

```text
SerializableBy(K)
```

the analyzer first computes the relevant transaction **conflict closure**.

It must not examine only the declaring transaction.

---

# 36. Transaction identity for analysis

Introduce internal:

```rust
pub struct TransactionRef {
    pub operation: Id,
    pub transaction: Id,
    pub location: StepLocation,
}
```

One inline transaction declaration corresponds to one transaction template.

Concurrent executions of the same template are included in analysis.

Self-conflict edges are therefore possible.

---

# 37. Access index

Derive an access index for every transaction:

```rust
pub struct TransactionAccess {
    pub transaction: TransactionRef,
    pub step: TransactionStepLocation,
    pub object: Id,
    pub selector: SelectorDomain,
    pub fields: AccessFields,
    pub mode: AccessMode,
}
```

Initially:

```rust
pub enum AccessMode {
    Read,
    Write,
    Insert,
    Delete,
    TransitionRead,
    TransitionWrite,
    VersionValidate,
    VersionBump,
    CursorReadWrite,
    FenceReadWrite,
}
```

Lock declarations are indexed separately.

Transition-scoped and ordinary Outbox writes are indexed as transaction commit artifacts, not `DataObject` access conflicts.

---

# 38. Access footprints

Derive:

```text
Read
    -> declared FieldSelection

Write
    -> declared fields

Transition
    -> read + write of state-machine state field

BumpVersion
    -> write of version field

ValidateVersion
    -> read/validation of version field

AdvanceCursor
    -> read + conditional write of cursor field

Fence
    -> read + conditional write of fence field

Delete
    -> conflicts with all fields of selected instance

Insert
    -> creates complete logical instance
```

Outbox writes and EffectIntent establishment are not ordinary `DataObject` conflict accesses.

---

# 39. Selector-domain analysis

Two accesses conflict only where their selected logical object domains may overlap.

Analyzer logic:

1. object IDs differ  
   → disjoint.

2. complete object identity values are provably different  
   → disjoint.

3. equality predicates prove incompatible literals  
   → disjoint.

4. canonical fragment/value equivalence proves equality  
   → overlapping domain.

5. otherwise  
   → potentially overlapping.

Unknown overlap is never treated as disjoint.

---

# 40. Field overlap

For potentially overlapping object instances:

```text
Read(A), Write(B)
```

conflict only if field sets overlap.

`FieldSelection::All` overlaps every field.

Delete overlaps all state.

Insert conflicts with another access when the same complete logical identity may be created/accessed.

---

# 41. Conflict closure

Construct an undirected potential-conflict graph:

```text
transaction template
    -- potential persistent conflict -->
transaction template
```

An edge exists where two executions may touch overlapping state and at least one access is a mutation.

For requirement transaction `T`, its conflict closure is the connected component containing `T`.

This is deliberately transitive.

Example:

```text
T conflicts U
U conflicts V
T does not directly conflict V
```

`V` still belongs to `T`'s closure because a serialization cycle can pass through `U`.

---

# 42. Serializable-isolation closure proof

For root transaction `T`:

```text
ConflictClosure(T) = { T, U, V, ... }
```

If every transaction template in the closure declares:

```text
isolation: serializable
```

then:

```text
SerializableBy(K)
```

is proven, provided the requirement key identifies the analyzed transaction domain.

One serializable transaction mixed with weaker conflicting transactions is insufficient.

---

# 43. Serialization graph proof

Where the closure is not uniformly serializable, construct a potential serialization-dependency graph.

Dependency kinds:

```rust
pub enum DependencyKind {
    WriteRead,
    ReadWriteAntiDependency,
    WriteWrite,
}
```

Each potential dependency records:

```text
source transaction
target transaction
object
selector overlap evidence
field overlap evidence
access locations
commit-order evidence
```

---

# 44. Why anti-dependencies matter

For:

```text
T:
    R(GlobalLimit)
    W(Account)

U:
    R(Account)
    W(GlobalLimit)
```

snapshot-stable reads can still yield:

```text
T --rw--> U
U --rw--> T
```

This is write skew.

Therefore:

```text
repeatable read
snapshot read stability
```

do not by themselves prove serializability.

---

# 45. Commit-order evidence

For each potential dependency, determine whether the model guarantees:

```text
if both transactions commit and this dependency occurs,
commit(source) < commit(target)
```

Represent internally:

```rust
pub enum CommitOrderEvidence {
    IntrinsicCommittedRead,
    AtomicWriteOrder,

    StrictLock {
        ...
    },

    VersionValidation {
        ...
    },

    OrderedCursor {
        ...
    },

    None,
}
```

Fence ordering may be recorded separately because equal fencing tokens do not generally serialize same-generation transactions.

---

# 46. Natural commit-order edges

Under `ReadCommitted`, `Snapshot`, or `Serializable`:

A transaction that actually reads another transaction's committed write establishes:

```text
writer commits before reader observes
```

so that dependency is commit-order aligned.

Atomic conflicting writes likewise admit a commit ordering.

`TransactionIsolation::Unspecified` provides no such fact unless another primitive establishes it.

---

# 47. Strict-lock evidence

A read/write dependency is commit-order protected by lock discipline only where both conflicting transaction templates honor compatible locking over the same logical domain.

Required coverage:

### Read side

Before protected read:

```text
Shared OR Exclusive lock
```

### Write side

Before conflicting mutation:

```text
Exclusive lock
```

Selectors must overlap the same protected domain.

Locks are held until transaction termination.

Therefore if the reader obtains its compatible lock first:

```text
reader commits before writer can acquire X
```

and the dependency becomes commit-order aligned.

---

# 48. Lock acquisition timing

A lock does not protect earlier observations.

This cannot prove protection:

```text
Read X
Lock X
Write Y
```

This can:

```text
Lock X
Read X
Write Y
```

The verifier must compare transaction program locations.

---

# 49. Version-validation evidence

For:

```text
T reads X.version = V
T later ValidateVersion(X,V)

U mutates X
U BumpVersion(X)
```

if U commits before T:

```text
X.version != V
```

and T rejects.

Therefore if both commit with the read/write anti-dependency:

```text
commit(T) < commit(U)
```

The dangerous anti-dependency becomes commit-order constrained.

This is the OCC route that closes write-skew cycles involving validated observations.

---

# 50. Cursor evidence

Two successful transactions advancing the same cursor domain through:

```text
Successor
```

or:

```text
MonotonicAfter
```

are commit-ordered by accepted cursor position.

An older position cannot commit after a newer accepted position.

A duplicate/stale position rejects.

Cursor evidence may therefore constrain dependency edges between transactions governed by the same cursor.

---

# 51. Fencing evidence

A fence proves:

```text
lower generation cannot commit after higher generation is accepted
```

It does not by itself serialize multiple transactions carrying the same fencing token.

Therefore fencing may eliminate stale-generation dependency histories but is not an independent general serializability proof.

It composes with:

```text
locks
version validation
cursor ordering
serializable isolation
```

---

# 52. Static cycle algorithm

For each requirement closure:

1. construct all potential directed dependency edges;
2. classify each edge as commit-order constrained or unconstrained;
3. compute strongly connected components over the potential graph;
4. ignore acyclic singleton components;
5. for each cyclic SCC:
   - if every edge participating in the SCC is commit-order constrained, the apparent cycle cannot occur in a committed history because it would imply a cycle in strict commit order;
   - if the SCC contains any unconstrained dependency edge, Conseqa cannot exclude a non-serializable committed history.

Therefore:

```text
no cyclic SCC containing an unconstrained edge
    =>
SerializableBy(K) proven
```

Otherwise:

```text
Unknown / Unproven
```

with the unconstrained cycle reported.

Conseqa does not assume a missing synchronization fact.

---

# 53. Serializability proof evidence

Add:

```rust
pub enum TransactionSerializabilityProof {
    SerializableIsolationClosure {
        root: TransactionRef,
        key: ValueRef,
        closure: Vec<TransactionRef>,
    },

    ConflictGraph {
        root: TransactionRef,
        key: ValueRef,
        closure: Vec<TransactionRef>,
        dependencies: Vec<DependencyEvidence>,
    },
}
```

`DependencyEvidence` records the reason each dangerous edge was made commit-order safe.

---

# 54. Transaction ordering proof

An ordering requirement:

```text
OrderedBy(K,P)
```

first requires a transaction-serializability proof for the relevant key/domain.

The designer does not need to redundantly declare `SerializableBy(K)`.

The ordering verifier may invoke the serializability prover as a prerequisite.

---

# 55. Cursor ordering route

The canonical ordering proof is:

```text
Requirement.position
    canonically equals
AdvanceCursor.incoming
```

and:

```text
Requirement.key
    canonically identifies
the cursor target domain
```

The verifier then requires:

- compatible cursor use across all transaction templates that mutate the cursor;
- no ordinary writes to the cursor field;
- transaction serializability for the affected conflict closure.

Then:

```text
Successor
```

or:

```text
MonotonicAfter
```

can prove:

```text
OrderedBy(K,P)
```

`Successor` additionally proves gap-free accepted progression, though v4 need not expose that as a separate requirement.

---

# 56. Fence ordering route

A fencing token may prove monotonic transaction ordering where:

```text
Requirement.position == Fence.token
```

and:

```text
Requirement.key
```

identifies the same fence domain.

The verifier requires:

- all relevant ordered transaction templates to honor the same fence field/domain;
- no uncontrolled writes to that fence field;
- transaction serializability independently established.

The fence then proves:

```text
P1 < P2
    =>
P1 cannot commit after P2 has been accepted
```

Equal fencing tokens establish no relative order.

---

# 57. L1 is not an ordering proof route

Do not discharge `TransactionOrderingRequirement` from:

```text
TopicRuntime.ordering
SubscriptionRuntime.ordering
OutboxRuntime.ordering
BatchOrderingPreservation
```

alone or in combination with pool topology.

Those facts may describe how work ordinarily arrives.

The transaction requirement demands state-level enforcement that remains correct under:

```text
redelivery
timeout
worker replacement
stale workers
retries
reordering after failure
```

Cursor/fence validation provides that enforcement.

---

# 58. Transaction fallibility and ordering

Cursor, version, fence, and invalid-transition failures are logical transaction rejection.

Therefore an out-of-order or stale transaction does not corrupt state and subsequently require repair:

```text
bad state commits
then repair
```

is not the v4 model.

Instead:

```text
detect invalid/stale state before successful commit
reject transaction
retry/defer/no-op according to operation control
```

V4 is a conflict-prevention/validation model, not a post-corruption convergence model.

---

# 59. Error-result refinement

Replace:

```rust
pub struct ResultType {
    pub ok: Id,
    pub err: ErrorResultType,
}
```

with:

```rust
pub struct ResultType {
    pub ok: Id,

    #[serde(default)]
    pub errors: BTreeMap<Id, ErrorResultType>,
}
```

The map key is the logical error-class ID.

Example:

```yaml
result:
  ok: schema.Payment

  errors:
    already_processed:
      schema: schema.AlreadyProcessed
      disposition: terminal

    conflict:
      schema: schema.ConcurrentConflict
      disposition: retryable
```

---

# 60. Return error class

Replace:

```rust
ResultOutcome::Err {
    values: Derivation,
}
```

with:

```rust
ResultOutcome::Err {
    error: Id,
    values: Derivation,
}
```

The named error class MUST exist in the targeted request result contract.

Its derivation MUST match that class's schema.

---

# 61. Match result error classes

Replace:

```rust
pub struct MatchResult {
    pub result: Id,
    pub ok: OperationBlock,
    pub err: OperationBlock,
}
```

with:

```rust
pub struct MatchResult {
    pub result: Id,
    pub ok: OperationBlock,
    pub errors: BTreeMap<Id, OperationBlock>,
}
```

Error arms are explicit and exhaustive over the result contract's error classes.

Each error class retains its own:

```text
terminal
retryable
unspecified
```

disposition.

Execution interruption is never synthesized as an `Err`.

---

# 62. Analyzer pass architecture

Implement the new proof system as separate passes.

## Pass 1 — structural validation

Validate:

```text
transaction requirements
transaction-entry availability of keys/positions
ordered scalar types
transition effect kinds
transition Outbox references/schemas/derivations
version declarations
managed monotonic fields
lock targets/modes
rejected-arm presence
result error classes
```

## Pass 2 — transaction control-flow validation

Extend operation traversal through:

```text
ExecuteTransaction.rejected
```

Update:

```text
step locations
transaction enumeration
artifact availability
terminal reachability
definite result availability
```

Transition rejection must remove all transition-scoped effect artifacts/admissions from the rejected path.

## Pass 3 — transaction access indexing

Generate:

```text
TransactionRef
TransactionAccess
LockAccess
VersionProtocolIndex
ManagedMonotonicFieldIndex
TransactionCommitArtifactIndex
```

`TransactionCommitArtifactIndex` includes:

```text
ordinary OutboxWriteEffect
transition-scoped OutboxWriteEffect
EffectIntent establishment
transaction outputs
```

These do not become ordinary persistent conflict accesses.

## Pass 4 — selector/field overlap

Canonicalize access domains and identify potential conflicts.

## Pass 5 — conflict closure

Build model-wide transaction conflict components.

## Pass 6 — serializable-isolation closure

Attempt the simple proof route.

## Pass 7 — serialization graph

Build dependencies, derive commit-order evidence, compute SCCs, and attempt the mixed-mechanism proof.

## Pass 8 — transaction ordering

Consume:

```text
serializability proof
+
cursor/fence protocol
+
key/position equivalence
```

and discharge ordering requirements.

## Pass 9 — existing idempotency/recoverability

Run independently.

Update result reasoning for logical error classes.

---

# 63. Analyzer modules

Remove:

```text
src/analyzer/verification/serialization.rs
src/analyzer/verification/ordering.rs
```

in their current operation-level form.

Replace with:

```text
verification/
    transaction_serializability.rs
    transaction_ordering.rs
    transaction_conflicts.rs
```

Shared conflict machinery should not be duplicated.

---

# 64. Remove old proof structures

Delete old report/proof structures representing:

```text
InvocationLocked
RequestRouted serialization
SubscriptionRouted serialization
OutboxRouted serialization
topology ordering proofs
execution_handoff evidence
member-concurrency correctness evidence
```

No v4 report should contain a proof statement of:

```text
consistent_hash + bounded(1) => correctness
```

---

# 65. New report structures

Add approximately:

```rust
pub struct TransactionRequirementVerdict {
    pub operation: Id,
    pub transaction: Id,
    pub requirement: TransactionRequirementView,
    pub verdict: Verdict,
    pub proof: Option<TransactionProof>,
}

pub enum TransactionProof {
    Serializability(TransactionSerializabilityProof),
    Ordering(TransactionOrderingProof),
}
```

Ordering proof:

```rust
pub enum TransactionOrderingProof {
    Cursor {
        key: ValueRef,
        position: ValueRef,
        cursor: ManagedFieldRef,
        rule: CursorAdvanceRule,
        serializability: Box<TransactionSerializabilityProof>,
    },

    Fence {
        key: ValueRef,
        position: ValueRef,
        fence: ManagedFieldRef,
        serializability: Box<TransactionSerializabilityProof>,
    },
}
```

Commit-artifact evidence may additionally report transactionally admitted Outbox messages, including whether they originated directly from the transaction or from a transition.

---

# 66. Proof rendering example — serializable isolation

```text
Requirement:
    tx.accept_order SerializableBy(order_id)

Verdict:
    Proven

Evidence:
    relevant conflict closure:
        tx.accept_order
        tx.cancel_order
        tx.adjust_inventory

    every transaction in the closure declares:
        isolation = serializable
```

---

# 67. Proof rendering example — mixed application protocol

```text
Requirement:
    tx.allocate SerializableBy(account_id)

Verdict:
    Proven

Evidence:
    GlobalLimit read is guarded by:
        observed version = read.global_limit.version
        ValidateVersion(GlobalLimit, observed version)

    every committed GlobalLimit mutation:
        BumpVersion(GlobalLimit)

    Account(account_id) mutation is protected by:
        Exclusive lock acquired before Account observation

    remaining dependency edges are commit-order constrained.

    no cyclic conflict component contains an
    unconstrained dependency.
```

---

# 68. Proof rendering example — ordered transition with atomic Outbox admission

```text
Requirement:
    tx.apply_order_event OrderedBy(order_id, event_sequence)

Verdict:
    Proven

Evidence:
    transaction state history is serializable

    ordering key:
        order_id

    logical position:
        event_sequence

    persisted cursor:
        Order.last_applied_sequence

    cursor rule:
        successor

    transition:
        Pending -> Accepted

    transition atomically admits:
        outbox.PaymentRequested

    therefore:
        transaction state application respects event_sequence

        PaymentRequested admission is part of the same
        ordered transaction commit

    no claim is made about later PaymentRequested consumption order.
```

---

# 69. Required diagnostics

Add at minimum:

```text
TransactionRequirementKeyUnavailable
TransactionOrderingPositionUnavailable
TransactionOrderingPositionNotOrderedScalar

MissingTransactionRejectedArm
UnexpectedTransactionRejectedArm

InvalidTransitionEffectKind
UnknownTransitionOutbox
InvalidTransitionOutboxSchema
InvalidTransitionOutboxDerivation

InvalidObjectVersionField
DirectWriteToVersionField
MissingVersionBump
DuplicateVersionBump
VersionValidationWithoutObservedVersion

ManagedFieldRoleConflict
DirectWriteToManagedField
InvalidManagedFieldType

LockCoverageMissing
LockAcquiredAfterProtectedAccess

TransactionConflictUnknownSelectorOverlap
TransactionConflictUnknownFieldOverlap

SerializableClosureContainsWeakerIsolation

TransactionSerializabilityUnconstrainedCycle
TransactionSerializabilityUnprotectedReadWriteDependency

OrderingMissingSerializability
OrderingMissingCursorOrFence
OrderingKeyDomainMismatch
OrderingPositionMismatch
OrderingUncontrolledManagedFieldWriter

UnknownResultErrorClass
MissingResultErrorArm
UnexpectedResultErrorArm
```

Diagnostics for conflict cycles should identify the concrete transaction chain.

---

# 70. Conservative analysis rules

Conseqa MUST refuse proof when required facts are unknown.

Examples:

```text
selector overlap unknown
    -> potentially overlapping

field provenance unknown
    -> potentially conflicting

isolation unspecified
    -> no isolation guarantee

version mutation coverage incomplete
    -> no OCC credit

ordinary write touches cursor
    -> no cursor ordering proof

fence use incomplete
    -> no fencing credit
```

`unspecified` remains epistemic.

It never means false and never means safe.

---

# 71. Deadlock remains separate

S/X locking can prove serializable committed histories while still admitting deadlock.

Conseqa must not conflate:

```text
serializability
```

with:

```text
deadlock freedom
```

A deadlock victim abort is an interrupted attempt, not logical rejection.

Existing/future lock-order analysis remains independent.

`LockOrder` stays in the DSL.

---

# 72. Interaction with idempotency

Transaction serializability does not prove idempotency.

Example:

```text
counter += 1
```

may be perfectly serializable and non-idempotent.

Similarly:

```text
OrderedBy(K,P)
```

does not imply duplicate suppression.

Keep:

```text
transaction idempotency
operation idempotency
result replay
```

as independent properties.

---

# 73. Interaction with Outbox

The revised exclusive-consumer Outbox semantics remain unchanged.

An ordinary `OutboxWriteEffect` remains atomically admitted with transaction commit.

A transition-scoped `OutboxWriteEffect` is additionally conditioned on successful application of its enclosing transition.

Both forms participate in transaction atomicity at the **admission** level.

Thus:

```text
Transition Pending -> Accepted
    OutboxWrite PaymentRequested
```

establishes:

```text
Accepted committed
    =>
PaymentRequested admitted in the same commit
```

and:

```text
transition rejected
    =>
PaymentRequested not admitted
```

Downstream consumer:

```text
retries
ordering
partitioning
worker concurrency
serialization
```

remain separate analyses.

---

# 74. Interaction with EffectIntent

EffectIntent establishment remains a transaction artifact.

Transaction serializability applies to the atomic establishment event.

Later:

```text
ExecuteEffectIntent
```

is outside transaction serializability.

No attempt is made to lift transaction order into effect-execution order.

Transition effects do not admit `ExecuteEffectIntent`.

This deliberately eliminates whole-operation serialization composition.

---

# 75. L1 after the refactor

L1 retains:

```text
TopicRuntime grouping/ordering
Subscription delivery/grouping/ordering/dispatch
Outbox partitioning/ordering/dispatch
Request routing
MemberAssignment
ExecutionPool.member_concurrency
StorageLayout
batching semantics
```

L1 removes:

```text
ExecutionPool.execution_handoff
ExecutionHandoff
```

Its governing rule becomes:

> L1 describes placement, transport, grouping, precedence, and runtime capacity. It does not provide transaction consistency guarantees.

---

# 76. Files directly affected

At minimum:

```text
src/spec/model.rs
    DSL_VERSION 3 -> 4

src/spec/operation/mod.rs
    remove invocation_lock
    remove operation serialization/ordering requirements
    retain idempotency/recoverability

src/spec/operation/program.rs
    Transaction(Transaction)
        ->
    Transaction(ExecuteTransaction)

src/spec/operation/transaction.rs
    add TransactionRequirements
    add transaction requirement types
    add ValidateVersion
    add BumpVersion
    add AdvanceCursor
    add Fence

src/spec/operation/state_machine.rs
    make transition rejection explicit
    add Transition.effects
    add TransitionEffect::OutboxWrite

src/spec/operation/result.rs
    multiple logical error classes

src/spec/data_model.rs
    add ObjectVersion

src/spec/runtime.rs
    remove execution_handoff
    strip correctness semantics from MemberAssignment/member_concurrency
    tighten ordering non-implications

src/analyzer/validation/*
    new structural validation
    transition-effect validation
    remove invocation-lock/execution-handoff validation

src/analyzer/verification/*
    remove old operation serialization/ordering proof system
    add transaction conflict + serializability + ordering proof system

src/analyzer/report.rs
    replace operation serialization/ordering evidence
    report transaction commit artifacts where useful

src/viz/*
    remove old requirement/proof views
    expose transaction requirements and proof evidence
    render transition-scoped Outbox admissions

tests/*
    rewrite v3 serialization/ordering fixtures
    add transition-outbox atomicity fixtures
    add transaction consistency matrix
```

---

# 77. DSL and report versioning

Set:

```rust
pub const DSL_VERSION: DslVersion = DslVersion(4);
```

Because v4 makes normative and structural changes.

The report storage/prover format MUST also increment.

Assuming no intervening change:

```text
FORMAT 6 -> FORMAT 7
```

No attempt is made to parse DSL v3 as DSL v4.

A v3 model receives an explicit contract-version mismatch.

---

# 78. Remove superseded documentation semantics

Delete or rewrite all semantic text claiming:

```text
SerializedBy operation requirement
OrderedBy operation requirement
InvocationLock proof route
consistent-hash serialization proof
member-concurrency serialization proof
exclusive execution handoff proof
transport + one-worker operation ordering proof
```

Add explicit documentation for:

```text
transaction requirements
transaction rejection
object versions
cursor/fence protocols
transition-scoped Outbox admission
```

Any existing serialization-semantics document based on operation-level topology proofs is superseded by this revision.

---

# 79. No legacy compatibility

This revision deliberately does not implement:

```text
serde aliases
legacy requirement translation
automatic migration
deprecated syntax acceptance
verdict-preservation adapters
execution_handoff compatibility interpretation
invocation_lock compatibility interpretation
```

Old models must be rewritten.

This is preferable to carrying contradictory correctness philosophies in one analyzer.

---

# 80. Minimum implementation sequence

Implement in this order:

```text
1. Remove operation serialization/ordering proof surface.
2. Remove InvocationLock.
3. Remove ExecutionHandoff and topology correctness paths.
4. Add transaction requirements.
5. Add ExecuteTransaction + rejected control.
6. Make Transition rejectable.
7. Add TransitionEffect::OutboxWrite and atomicity validation.
8. Add multiple result error classes.
9. Add ObjectVersion + ValidateVersion/BumpVersion.
10. Add AdvanceCursor.
11. Add Fence.
12. Build transaction access index.
13. Build transaction commit-artifact index.
14. Build selector/field overlap analysis.
15. Build conflict closure.
16. Implement serializable-isolation closure proof.
17. Implement lock coverage / commit-order evidence.
18. Implement version-validation evidence.
19. Implement serialization SCC proof.
20. Implement cursor ordering proof.
21. Implement fence ordering proof.
22. Replace report/viz evidence.
23. Rewrite semantic documentation and fixtures.
24. Bump DSL/report versions.
```

The transaction requirement surface should not land without at least the serializable-closure proof route.

Application-state primitives should not receive proof semantics until model-wide conflict analysis exists.

Transition-scoped Outbox admission may land earlier because its atomicity semantics are independently useful and straightforward.

---

# 81. Minimum test matrix

## Removals

```text
operation serialization field rejected
operation ordering field rejected
invocation_lock rejected
execution_handoff rejected
```

## Transition rejection

```text
allowed from-state -> commit
invalid from-state -> rejected branch
no transaction artifacts survive rejection
no transition Outbox admission survives rejection
infrastructure abort does not select rejected
```

## Transition Outbox effects

```text
successful transition -> transition Outbox message admitted
rejected transition -> no message admitted
later transaction-step rollback -> no message admitted
multiple transition Outbox writes commit atomically
invalid transition effect kind rejected
unknown Outbox rejected
invalid schema/derivation rejected
```

## Locking

```text
S/S compatible
S/X conflict
X/X conflict

lock before read -> usable
read before lock -> not protected
```

## Versioning

```text
mutating versioned object without bump -> invalid
direct ordinary write to version field -> invalid

stale version -> reject
unchanged version -> validate

validated GlobalLimit breaks write-skew cycle
missing validation leaves cycle unproven
```

## Serializable closure

```text
all conflict-closure txs serializable -> proven
one weaker conflicting tx -> closure route fails
indirect conflicting tx included transitively
```

## Graph route

```text
acyclic graph -> proven
cycle of unprotected rw dependencies -> unproven
cycle fully commit-order constrained -> proven
unknown selector overlap -> conservative/unproven
```

## Cursor

```text
successor 17 -> 18 -> commit
successor 17 -> 19 -> reject
successor 17 -> 17 -> reject

monotonic 17 -> 19 -> commit
monotonic 19 -> 18 -> reject

ordinary writer of cursor field -> proof refused
```

## Fence

```text
current 18, token 17 -> reject
current 18, token 18 -> accepted
current 18, token 19 -> advance

fence alone does not serialize equal-token transactions
```

## Ordering

```text
serializable tx + matching successor cursor -> proven
matching cursor without serializability -> unproven
position mismatch -> unproven

ordered transition + transition Outbox admission:
    transaction ordering applies to admission commit
    downstream consumption order is not inferred
```

## Orthogonality

```text
serializability does not prove idempotency
ordering does not prove idempotency
transport ordering does not prove transaction ordering
member_concurrency does not prove transaction serializability
consistent hashing does not prove transaction serializability
transaction ordering of Outbox admission does not prove consumer ordering
```

---

# 82. Final semantic architecture

```text
                    L1 RUNTIME
                    ==========

transport grouping
transport precedence
routing
member assignment
member concurrency
partitioning
batching
storage layout

        |
        | descriptive / realization facts
        | no exclusion-based correctness proof
        v


                L0 TRANSACTION STATE
                ====================

          Transaction requirements
            /                \
           /                  \
          v                    v
 SerializableBy(K)       OrderedBy(K,P)
          |                    |
    +-----+------+        +----+-----+
    |     |      |        |          |
    v     v      v        v          v
 serial  S/X   versions  cursor     fence
 isolation locks validation

          \                  /
           \                /
            v              v
        model-wide conflict analysis
                    |
                    v
             proof / unknown


         ATOMIC TRANSACTION COMMIT ARTIFACTS
         ===================================

persistent state mutations
state-machine transitions
transaction outputs
EffectIntent establishment
ordinary OutboxWriteEffect
transition-scoped OutboxWriteEffect

All are admitted/established iff the
containing transaction commits.


                OPERATION PROGRAM
                =================

idempotency
recoverability
explicit transaction commit/rejection control
effects
async effects
request results

No operation-level serialization or ordering obligation.
```

The central v4 rule is:

> **Distributed execution is allowed to overlap, retry, fail over, and reorder. Conseqa proves correctness where persistent application state makes invalid histories unable to commit, while transactionally admitted downstream work—including Outbox messages attached directly to state transitions—shares the same atomic commit boundary.**