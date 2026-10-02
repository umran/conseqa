# Conseqa Atomic Mutation and Serialization Primitives Revision Specification

## 1. Status

**Status:** Proposed  
**Target DSL version:** 6  
**Baseline:** Conseqa `master`, commit `14e692d102e7db468efcfd764fc4f96f6bc7be2a`  
**Primary affected subsystem:** L0 transaction semantics and transaction serializability verification

This revision replaces Conseqa's explicit version-validation protocol with database-shaped atomic mutation primitives.

The following transaction steps are removed from the DSL:

- `validate_version`
- `bump_version`

The ordinary mutation primitive `write` is renamed to `update`.

The following primitive is introduced:

- `compare_and_set`

The following primitive is introduced for atomic create-or-update semantics:

- `upsert`

Existing domain-specific guarded mutations remain:

- `transition`
- `advance_cursor`
- `fence`

These primitives are unified internally by an analyzer concept of **atomic conditional mutation**, rather than by exposing analyzer proof mechanisms as transaction steps.

---

# 2. Motivation

Conseqa currently represents optimistic concurrency using two explicit transaction steps:

```yaml
- kind: validate_version
  target: ...
  expected: ...

- kind: bump_version
  target: ...
```

Their current semantics are stronger than their position in the transaction program suggests.

`validate_version` is written as an ordinary transaction step but does not semantically perform its comparison at that point. Instead, it declares a comparison that must remain true through commit arbitration.

`bump_version` separately publishes a concurrent modification.

Together:

```text
read(version = N)
validate_version(N)
mutation
bump_version
commit
```

represent an optimistic compare-and-mutate protocol.

This presents several problems.

First, the primitive does not correspond naturally to an ordinary database operation. A database implementation cannot in general perform:

```text
check version
...
commit later
```

and guarantee that no writer changes the version between the check and commit without introducing another mechanism such as locking, a conditional mutation, or serializable isolation.

Second, the DSL decomposes one concurrency operation into two artificial declarations and then reconstructs their relationship inside the analyzer.

The current implementation therefore requires:

- `ValidateVersion`
- `BumpVersion`
- `VersionValidation`
- `AccessMode::VersionValidate`
- `AccessMode::VersionBump`
- validation of matching reads
- validation of required bumps
- validation of duplicate bumps
- analyzer matching between reader validation and writer bump
- `VersionValidationMissing`
- `VersionBumpMissing`
- version-specific remedies
- version-specific visualization and explanation logic

The model should instead expose operations that correspond to concurrency mechanisms an implementation can actually perform.

The central rule of this revision is therefore:

> **Conseqa models atomic storage operations, while the serialization checker derives proof evidence from those operations. It does not expose pieces of the proof algorithm as transaction primitives.**

---

# 3. Goals

This revision has the following goals.

## 3.1 Database-shaped semantics

Each concurrency primitive should have a natural implementation in mainstream transactional stores.

For example:

```sql
UPDATE orders
SET status = ?, version = version + 1
WHERE order_id = ?
  AND version = ?;
```

is naturally represented as `compare_and_set`.

The comparison and mutation constitute one atomic operation.

There is no deferred validation phase.

## 3.2 Preserve Conseqa's static serialization analysis

Conseqa must continue to prove or refuse transaction-level:

```text
SerializableBy(K)
```

using whole-model conflict analysis.

The new primitive set changes the evidence available to that analysis; it does not weaken the requirement.

## 3.3 Generalize beyond version columns

Optimistic concurrency should not inherently require an integer version column.

A transaction may protect an observed value directly:

```text
read balance = 100

compare_and_set:
    require balance = 100
    update balance
```

or indirectly using a version token:

```text
read:
    balance = 100
    version = 7

compare_and_set:
    require version = 7
    update balance
```

The checker should understand both.

## 3.4 Make object versioning intrinsic

A version field remains useful, but version publication should be an object-level invariant rather than an explicit transaction step.

Users should never need to write:

```yaml
- kind: bump_version
```

## 3.5 Preserve specialized domain primitives

`transition`, `advance_cursor`, and `fence` encode semantics substantially richer than an ordinary update and remain useful.

They should not be replaced by generic SQL-shaped syntax merely for uniformity.

Instead, they should participate in a common internal guarded-mutation model.

## 3.6 Make impossible guarantees visible

If a transaction reads object A, derives a decision from A, but only mutates object B, Conseqa should not pretend that a cheap deferred version check on A exists.

The architecture must supply a real mechanism such as:

- locking A,
- serializable isolation,
- or an actual conditional mutation of A.

---

# 4. Non-goals

This revision does not introduce:

- a general SQL expression language;
- range predicates beyond the existing selector model;
- predicate locking;
- SSI simulation;
- arbitrary uniqueness constraints beyond `DataObject.identity`;
- database-vendor-specific syntax;
- compare-and-delete as a separate primitive;
- arbitrary triggers or stored procedures;
- a generic transactional assertion evaluated magically at commit.

In particular, this revision deliberately does **not** introduce an `assert`, `validate`, or `check_at_commit` primitive.

Such a primitive would recreate the abstraction being removed.

---

# 5. DSL version

This revision is normative and backward-incompatible.

Conseqa SHALL declare:

```rust
pub const DSL_VERSION: DslVersion = DslVersion(6);
```

Because DSL 6 changes the meaning and vocabulary of transaction programs, DSL 4 and DSL 5 documents do not have identical semantics under DSL 6.

Conseqa therefore SHALL NOT silently parse them as DSL 6.

The initial implementation SHALL use:

```rust
pub const DSL_READS: [DslVersion; 1] = [DslVersion(6)];
```

This follows the existing parser rule that incompatible specifications are re-authored rather than silently migrated.

A separate migration tool may be introduced later, but migration is not part of parsing.

---

# 6. Revised transaction vocabulary

The principal transaction state operations become:

```text
read
update
compare_and_set
insert
upsert
delete
lock
transition
advance_cursor
fence
```

Artifact and effect operations remain unchanged.

The corresponding `TransactionStep` shape becomes conceptually:

```rust
pub enum TransactionStep {
    Read(Read),

    Update(Update),
    CompareAndSet(CompareAndSet),
    Insert(Insert),
    Upsert(Upsert),
    Delete(Delete),

    Lock(Lock),

    Transition(StateTransition),
    AdvanceCursor(AdvanceCursor),
    Fence(Fence),

    EstablishEffectIntent(EstablishEffectIntent),
    EstablishTransactionOutput(EstablishTransactionOutput),
    WriteOutbox(WriteOutboxEffect),
}
```

`Write`, `ValidateVersion`, and `BumpVersion` are removed.

---

# 7. `update`

`update` replaces the current `write` primitive.

Its semantics remain those of an unconditional mutation of the selected persistent objects.

```rust
pub struct Update {
    pub target: ObjectSelector,
    pub fields: BTreeSet<FieldPath>,
    pub values: Derivation,
}
```

Example:

```yaml
- kind: update
  target:
    object: object.account
    predicate:
      kind: eq
      field: account_id
      value:
        source: input:input.adjust.request
        path: account_id

  fields:
    - balance

  values:
    kind: deterministic
    from:
      - source: input:input.adjust.request
        path: amount
```

`update` is not an optimistic-concurrency guard.

It does not reject merely because some previously observed value has changed.

If stale-read protection is required, the transaction must use:

- `compare_and_set`;
- a suitable guarded domain primitive;
- locking;
- or serializable isolation.

---

# 8. `compare_and_set`

## 8.1 Definition

`compare_and_set` is an atomic conditional update of one identified persistent object instance.

Conceptually:

```rust
pub struct CompareAndSet {
    pub target: ObjectSelector,

    pub compare: Vec<CompareCondition>,

    pub fields: BTreeSet<FieldPath>,

    pub values: Derivation,
}

pub struct CompareCondition {
    pub field: FieldPath,
    pub expected: SelectorValue,
}
```

`SelectorValue` remains capable of holding either a literal or `ValueRef`.

Example:

```yaml
- kind: compare_and_set
  target:
    object: object.account
    predicate:
      kind: eq
      field: account_id
      value:
        source: input:input.adjust.request
        path: account_id

  compare:
    - field: version
      expected:
        source: transaction_read:read.adjust.account
        path: version

  fields:
    - balance

  values:
    kind: deterministic
    from:
      - source: transaction_read:read.adjust.account
        path: balance
      - source: input:input.adjust.request
        path: amount
```

## 8.2 Atomicity

The comparison and mutation MUST constitute one atomic storage operation.

Conceptually:

```text
if current fields satisfy all compare conditions:
    perform mutation
else:
    reject transaction
```

There MUST NOT be an observable interval between successful comparison and acquisition of the mutation's concurrency protection.

A relational implementation may use:

```sql
UPDATE account
SET balance = ?,
    version = version + 1
WHERE account_id = ?
  AND version = ?;
```

and determine success from the affected-row count.

## 8.3 Rejection

`compare_and_set` is a commit guard.

The transaction rejects if:

- the identified instance does not exist; or
- any comparison evaluates false.

Therefore:

```rust
TransactionStep::CompareAndSet(_)
```

contributes `true` to `Transaction::rejects()`.

Its execution site requires a `rejected` arm under the existing transaction rejection rules.

## 8.4 Identity requirement

A `compare_and_set` target MUST identify exactly one logical object instance.

The target predicate MUST pin every field of:

```rust
DataObject.identity
```

using equality against a literal or value reference.

Selectors such as:

```yaml
predicate:
  kind: all
```

or predicates covering only a subset of a composite identity are invalid.

This deliberately excludes range and set CAS operations from the first version of the primitive.

## 8.5 Comparison conditions

`compare` MUST contain at least one condition.

Fields MUST exist in the object's canonical schema.

A field SHOULD appear at most once in `compare`.

Comparison is equality-based in DSL 6.

General inequalities belong to domain-specific primitives such as `advance_cursor` and `fence`.

---

# 9. Observed-state comparison

A comparison may serve two distinct purposes.

For example:

```yaml
compare:
  - field: status
    expected: pending
```

is a legitimate behavioral guard.

However, it does not prove that a previous read remains valid.

For serializability evidence, the checker distinguishes **observed-state comparisons**.

A comparison is an observed-state comparison when its `expected` value is exactly:

```text
transaction_read:<read binding>.<same field>
```

from an earlier `read` of the same identified instance.

Example:

```yaml
- kind: read
  bind: read.order
  target: ...
  fields:
    kind: only
    fields:
      - status

- kind: compare_and_set
  target: ...
  compare:
    - field: status
      expected:
        source: transaction_read:read.order
        path: status
```

The checker may use this condition as evidence that the value observed by `read.order.status` could not have changed unnoticed before the successful mutation.

A comparison against:

- an input;
- a literal;
- another object's read;
- a different instance;
- or a later read

does not constitute observed-state serialization evidence.

It may still be valid application behavior.

---

# 10. Object versions

## 10.1 Revised meaning

`DataObject.version` remains:

```rust
pub struct ObjectVersion {
    pub field: FieldPath,
}
```

The field remains:

- non-optional;
- integer-valued;
- outside object identity;
- managed by the persistence protocol.

Its meaning changes.

A version is now an **application concurrency token**, not one half of an explicit `ValidateVersion` / `BumpVersion` protocol.

## 10.2 Mutation invariant

For every committed transaction that mutates a live versioned instance, the committed version after the transaction MUST differ monotonically from the version preceding that transaction's mutation.

For an integer version:

```text
V_after > V_before
```

is sufficient.

DSL 6 does not require:

```text
V_after = V_before + 1
```

The precise increment is not semantically observable.

This intentionally permits implementations that:

- increment once per transaction;
- increment during one selected update;
- increment through an ORM;
- increment through a database trigger;
- or otherwise maintain the required monotonic token.

## 10.3 Insert

Insertion of a versioned object establishes its initial version.

The exact initial integer is not part of the semantic contract.

## 10.4 Delete

Deletion removes the versioned instance.

No version advancement is required because no live post-state remains.

A concurrent CAS against the deleted instance fails because the target no longer exists.

## 10.5 Managed field

Ordinary user mutations MUST NOT explicitly assign the version field.

Thus the existing principle behind:

```text
DirectWriteToVersionField
```

remains.

The following MUST NOT name the version field as an ordinary mutated field:

- `update.fields`;
- `compare_and_set.fields`;
- `upsert.update.fields`.

Domain-specific primitives also do not directly assign the version.

---

# 11. Version-token CAS

An observed version may guard arbitrary previously observed fields of the same object instance.

Example:

```yaml
- kind: read
  bind: read.account
  target: ...
  fields:
    kind: only
    fields:
      - balance
      - credit_limit
      - version

- kind: compare_and_set
  target: ...
  compare:
    - field: version
      expected:
        source: transaction_read:read.account
        path: version

  fields:
    - balance

  values: ...
```

The serialization checker may interpret this as:

> If another committed transaction mutates this instance after `read.account`, that mutation necessarily publishes a different version or deletes the instance. Therefore this CAS cannot successfully commit against the stale observed version.

Unlike the old protocol, no writer-specific `bump_version` evidence is required.

Version publication is an invariant of the object.

This eliminates the existing reader/writer pairing:

```text
ValidateVersion(reader)
+
BumpVersion(writer)
```

and replaces it with:

```text
observed version CAS(reader)
+
intrinsic version publication by every conflicting mutation
```

---

# 12. Guarded specialized mutations

Some existing primitives are already atomic conditional mutations.

These remain first-class because their domain semantics are useful.

They are extended to allow ordinary observed-state comparisons where necessary.

Conceptually, the following gain:

```rust
#[serde(default)]
pub compare: Vec<CompareCondition>;
```

where appropriate:

- `StateTransition`
- `AdvanceCursor`
- `Fence`

Their intrinsic conditions remain unchanged.

## 12.1 Transition

A transition already means:

```text
current state ∈ allowed-from-states
AND
perform transition atomically
```

An additional version comparison may produce:

```text
current state ∈ allowed-from-states
AND
version = observed_version
AND
perform transition
```

Example:

```yaml
- kind: transition
  machine: machine.order_lifecycle
  transition: transition.order.cancel

  subject:
    object: object.order
    predicate: ...

  compare:
    - field: version
      expected:
        source: transaction_read:read.cancel_order.order
        path: version

  effect_intents: {}
```

A database implementation can lower the combined guard to a single conditional state update.

## 12.2 Advance cursor

`advance_cursor` retains its intrinsic cursor condition.

For example:

```text
stored position accepts incoming position under successor rule
```

An optional compare guard is conjoined with that condition.

## 12.3 Fence

`fence` retains its token comparison semantics.

An optional compare guard is conjoined with the fencing condition.

---

# 13. Why guarded specialized mutations matter

The old version protocol could protect transactions such as:

```text
read order
validate order.version
advance cursor
transition order
bump order.version
```

Simply replacing the entire sequence with a standalone CAS would lose the specialized transition and cursor semantics.

The correct v6 representation is therefore:

```text
read order

advance_cursor
    compare observed version

transition order

commit
```

or, depending on program order and intent:

```text
read order

transition order
    compare observed version

advance_cursor

commit
```

Once an actual conditional mutation successfully obtains the database's mutation protection, the transaction continues while holding the corresponding transactional write protection through commit.

If the guard fails, the whole transaction rejects and preceding writes in that transaction roll back.

---

# 14. `upsert`

## 14.1 Definition

`upsert` represents an atomic insert-or-update operation on one logical identity.

Conceptually:

```rust
pub struct Upsert {
    pub target: ObjectSelector,

    pub insert_values: Derivation,

    pub update_fields: BTreeSet<FieldPath>,

    pub update_values: Derivation,
}
```

Example:

```yaml
- kind: upsert
  target:
    object: object.account_balance
    predicate:
      kind: and
      predicates:
        - kind: eq
          field: tenant_id
          value:
            source: input:input.post.request
            path: tenant_id

        - kind: eq
          field: account_id
          value:
            source: input:input.post.request
            path: account_id

  insert_values:
    kind: deterministic
    from:
      - source: input:input.post.request
        path: tenant_id
      - source: input:input.post.request
        path: account_id
      - source: input:input.post.request
        path: amount

  update_fields:
    - balance

  update_values:
    kind: deterministic
    from:
      - source: input:input.post.request
        path: amount
```

## 14.2 Identity

The initial DSL 6 implementation supports conflict arbitration only on:

```rust
DataObject.identity
```

The target MUST pin the complete identity.

Explicit alternate unique indexes are outside the scope of this revision.

## 14.3 Semantics

For one identified logical instance:

```text
if absent:
    insert
else:
    update
```

The choice and mutation are atomic with respect to competing operations on the same identity.

## 14.4 Versioning

For a versioned object:

- the insert branch establishes an initial version;
- the update branch publishes a new version automatically.

`update_fields` may not contain the version field.

## 14.5 Serialization limits

`upsert` does not magically protect arbitrary preceding reads.

For example:

```text
read Inventory
derive value
upsert Invoice
```

does not protect the Inventory observation.

Upsert only supplies concurrency evidence associated with its own identity arbitration and mutation.

---

# 15. Internal analyzer model

The public DSL should not force every primitive into identical syntax.

The analyzer, however, should normalize relevant operations into a common internal representation.

A proposed internal structure is:

```rust
pub struct ConditionalMutation {
    pub transaction: TransactionRef,
    pub step: usize,

    pub target: ObjectSelector,

    pub mechanism: ConditionalMutationKind,

    pub comparisons: Vec<ComparisonFact>,

    pub writes: AccessFields,

    pub publishes_version: Option<FieldPath>,
}

pub enum ConditionalMutationKind {
    CompareAndSet,
    Transition,
    AdvanceCursor,
    Fence,
    UpsertIdentityArbitration,
}

pub struct ComparisonFact {
    pub field: FieldPath,
    pub expected: SelectorValue,

    pub observed: Option<ObservedValue>,
}
```

This is analyzer IR, not DSL vocabulary.

A mechanism may additionally retain mechanism-specific data.

For example:

```text
AdvanceCursor:
    cursor field
    incoming position
    cursor rule

Fence:
    fence field
    token

Transition:
    state field
    allowed source states
```

---

# 16. Access indexing

The current conflict index maintains separate:

```text
validations
bumps
```

collections.

These are removed.

`TransactionTemplate` becomes conceptually:

```rust
pub struct TransactionTemplate<'a> {
    pub reference: TransactionRef,
    pub transaction: &'a Transaction,

    pub accesses: Vec<TransactionAccess>,
    pub locks: Vec<LockAccess>,

    pub conditional_mutations: Vec<ConditionalMutation>,

    pub artifacts: Vec<CommitArtifact>,
}
```

---

# 17. Access modes

The following access modes disappear:

```rust
VersionValidate
VersionBump
```

Ordinary names should also follow the new DSL terminology:

```rust
Read
Update
Insert
Delete
TransitionRead
TransitionWrite
CursorReadWrite
FenceReadWrite
...
```

`compare_and_set` should produce at least:

- a read footprint for compared fields;
- a write footprint for mutated fields.

Its conditional-mutation metadata associates those accesses with one atomic statement.

An implementation may introduce explicit internal modes such as:

```rust
CompareRead
CompareWrite
UpsertReadWrite
```

if doing so simplifies diagnostics and visualization, but the proof semantics do not require those exact names.

---

# 18. Synthetic version publication

Version publication remains relevant to static conflict analysis even though there is no explicit version step.

When indexing a mutation of a versioned object, the analyzer SHALL treat the operation as publishing a write to the object's version token.

For example:

```text
update fields: [balance]
```

against:

```text
version.field = version
```

has an effective write footprint including:

```text
balance
version
```

This does not mean the DSL operation explicitly assigns the field.

It expresses the object invariant that a successful committed mutation publishes a new token.

Similarly:

- transition writes state + version;
- cursor advancement writes cursor + version;
- fence writes fence + version;
- CAS writes its declared fields + version;
- upsert update publishes version.

Delete already conflicts with the whole instance and needs no synthetic version write.

---

# 19. Serialization checker

The high-level proof architecture remains:

1. build persistent access index;
2. determine potentially overlapping accesses;
3. build transaction conflict closure;
4. construct potential dependency graph;
5. classify dependency edges;
6. determine whether cyclic dependencies are commit-order constrained.

The serializable-isolation closure route remains unchanged.

The conflict-graph route changes its optimistic evidence.

---

# 20. Write-read dependencies

Existing committed-read evidence remains valid.

A write-read dependency may remain constrained by the target transaction's declared isolation according to existing rules.

Atomic conditional mutation need not replace the existing intrinsic committed-read rule.

Version-specific fallback logic based on:

```text
target validates
AND
source bumps
```

is removed.

Where a conditional mutation itself establishes stronger ordering, the analyzer may report that mechanism as evidence, but the ordinary committed-read rule remains preferable when sufficient.

---

# 21. Write-write dependencies

Existing atomic write ordering under declared transactional isolation remains.

For conditional mutations, a successful observed-state CAS may additionally constrain a write-write race:

```text
T1 observes version 4
T2 mutates instance -> version changes
T1 CAS(version = 4)
```

Both T1 and T2 cannot successfully commit as though T2's change had not occurred.

The analyzer may use:

```rust
CommitOrderEvidence::AtomicConditionalMutation { ... }
```

when ordinary isolation evidence is unavailable or when the conditional mechanism is the more precise proof.

---

# 22. Read-write anti-dependencies

Read-write anti-dependencies are the principal reason for the new primitive.

A transaction:

```text
T1 reads X
T2 writes X
```

must not silently allow T1 to commit a decision derived from stale X where serializability requires ordering.

The existing proof routes remain:

- strict locking;
- serializable isolation closure.

The version-validation route is replaced by **observed-state conditional mutation**.

---

# 23. Direct observed-field CAS proof

Consider:

```text
T1:
    read account.balance = 100

    compare_and_set account:
        require balance = 100
        write balance = ...

T2:
    update account.balance
```

If T2 commits before T1's CAS, T1's comparison cannot succeed against the stale observation.

If T1's CAS succeeds first, its mutation obtains the relevant transactional write ordering before T2 can commit a conflicting mutation.

Therefore a successful T1 commit constrains the anti-dependency.

The checker may credit this route when:

1. the earlier read and guarded mutation select the same identified instance;
2. the comparison occurs in the same transaction after the read;
3. the comparison's expected value names exactly the preceding read binding and field;
4. the guarded field covers the conflicting field represented by the anti-dependency;
5. guard failure rejects the whole transaction.

---

# 24. Version-token CAS proof

A version token allows one comparison to cover all observed application fields of an instance.

Suppose T1 reads:

```text
balance
credit_limit
version
```

and later atomically requires:

```text
version = previously_observed_version
```

Any conflicting committed mutation of that live instance:

- publishes a new version; or
- deletes the instance.

Therefore T1 cannot successfully commit its guarded mutation using a stale version.

The checker may credit the version route when:

1. the object declares `DataObject.version`;
2. the earlier read selected the same identified instance;
3. that read included the version field;
4. the guarded mutation compares the declared version field against exactly that earlier read;
5. the guarded mutation is part of the same transaction;
6. the guard rejects the transaction on mismatch.

No writer-side `BumpVersion` lookup is necessary.

This is a major simplification of the existing dependency analysis.

---

# 25. Range and phantom reads

Conditional mutation of one identity does not prove safety for arbitrary set or range reads.

For example:

```text
read all open reservations
...
CAS reservation R
```

does not prove that another transaction could not insert a new matching reservation.

The first v6 CAS route therefore applies only to identified-instance observations.

Range or predicate serializability requires another proof route, such as:

- serializable isolation;
- an appropriate lockable aggregate/domain object;
- or another explicit architecture mechanism introduced in a future revision.

The checker remains conservative.

---

# 26. Commit-order evidence

The existing:

```rust
CommitOrderEvidence::VersionValidation
```

is removed.

A replacement is introduced conceptually as:

```rust
CommitOrderEvidence::AtomicConditionalMutation {
    guarded_by: TransactionRef,
    step: usize,
    object: Id,
    mechanism: ConditionalMutationKind,
    compared_fields: BTreeSet<FieldPath>,
}
```

Where the version-token route is used, the evidence additionally records:

```rust
version_field: Option<FieldPath>
```

or an equivalent explicit evidence variant may be used:

```rust
ObservedStateCompareAndSet
ObservedVersionCompareAndSet
```

Either representation is acceptable provided diagnostics distinguish the reason the guard covers the dependency.

---

# 27. Dependency gaps

The following gaps are removed:

```text
VersionValidationMissing
VersionBumpMissing
```

They are replaced by diagnostics expressing the actual missing concurrency mechanism.

Recommended forms include:

```rust
ObservedStateGuardMissing {
    transaction: TransactionRef,
    object: Id,
}

ObservedStateGuardDoesNotCoverConflict {
    transaction: TransactionRef,
    object: Id,
    fields: ...
}
```

The diagnostic should explain remedies in database-shaped terms:

```text
The transaction reads this instance and later commits a decision that
can conflict with another writer, but it does not lock the observation,
run in a serializable closure, or condition a mutation on the observed
state.
```

For a versioned object:

```text
The transaction may compare the object's declared version against the
version observed by the earlier read using compare_and_set or a guarded
mutation.
```

---

# 28. Ordering checker

`OrderedBy(K, P)` remains conceptually unchanged.

The accepted ordering mechanisms remain:

- `AdvanceCursor`
- `Fence`, where applicable under its existing ordering rules

A generic CAS against a version does **not** prove business-position ordering.

Version numbers establish interference detection, not ordering according to an external position `P`.

Therefore the ordering prover SHALL NOT infer:

```text
OrderedBy(K, P)
```

merely from `compare_and_set`.

Ordering still presupposes transaction serializability according to the existing model.

---

# 29. Validation changes

## 29.1 Removed validation rules

The following become obsolete and should be removed:

```text
MissingVersionBump
DuplicateVersionBump
VersionValidationWithoutObservedVersion
VersionValidationWithoutIdentifiedInstance
VersionProtocolOnUnversionedObject
```

`ValidateVersion` and `BumpVersion` no longer exist, so these states cannot be represented.

## 29.2 Retained version validation

The following remains conceptually valid:

```text
InvalidObjectVersionField
DirectWriteToVersionField
ManagedFieldRoleConflict
```

The wording of `DirectWriteToVersionField` must be revised so it no longer refers to `bump_version`.

Recommended explanation:

```text
The declared version is a managed concurrency token. Application
mutations do not assign it directly; insertion establishes an initial
token and successful mutations of the live instance publish a newer
token automatically.
```

## 29.3 New CAS validations

Introduce validation for:

### `CompareAndSetWithoutIdentifiedInstance`

The CAS target does not pin every identity field.

### `CompareAndSetWithoutComparison`

The `compare` set is empty.

### `DuplicateCompareField`

A compare list contains the same field more than once.

Existing schema-path validation handles unknown comparison fields.

Existing value-reference validation handles invalid or unavailable expected values.

A comparison need not reference a prior transaction read merely to be valid. That restriction applies only when the analyzer attempts to credit it as observed-state serialization evidence.

## 29.4 Upsert validation

Introduce:

### `UpsertWithoutIdentifiedInstance`

The target does not pin full object identity.

### `UpsertMutatesIdentity`

The update branch attempts to mutate a field overlapping object identity.

Direct mutation of the version field continues to use the managed-field error.

---

# 30. Transaction rejection

`Transaction::rejects()` becomes exhaustive over the revised enum.

At minimum:

```text
CompareAndSet -> true
Transition    -> true
AdvanceCursor -> true
Fence         -> true

Read          -> false
Update        -> false
Insert        -> false
Upsert        -> false
Delete        -> false
Lock          -> false
...
```

The existing requirement that rejectable transactions carry an execution-site `rejected` arm remains.

Upsert is not logically rejection-producing merely because it chooses between insertion and update.

Infrastructure/database errors remain outside this logical rejection model.

---

# 31. Confluence sketch compiler

The sketch compiler currently generates `ValidateVersion` automatically for versioned single-record reads and later inserts `BumpVersion` for mutation.

That machinery is removed.

## 31.1 Single-record mutation

Where the sketch:

1. reads one identified versioned object; and
2. later mutates that same object;

the compiler should prefer an observed-version conditional mutation.

Examples:

```text
Write
```

becomes:

```text
CompareAndSet
```

using the observed version.

For:

```text
Transition
AdvanceCursor
Fence
```

the compiler attaches an observed-version comparison to the earliest suitable guarded mutation.

## 31.2 Mutation finalization

The current logic that searches mutated records and appends:

```rust
TransactionStep::BumpVersion(...)
```

is deleted.

No explicit replacement step is inserted.

Version publication follows from the object's version declaration.

## 31.3 Read-only observations

The compiler SHALL NOT synthesize an imaginary version assertion for a versioned object that is only read.

If stability of that read is necessary for a proof, the available mechanisms are real mechanisms:

- a lock before the read;
- stronger isolation;
- or a transaction design containing an actual guarded mutation.

This is intentional.

## 31.4 Read field pruning

A version field is retained in a generated `Read` only when some subsequent generated comparison references it or it is otherwise used.

There is no global rule that every versioned object read must observe its version.

---

# 32. Remedy engine

The current version remedy logic in:

```text
src/harness/executors/remedies.rs
```

constructs `ValidateVersion` and `BumpVersion`.

That route is removed.

A new optimistic remedy may be offered only where a genuine guarded mutation can be constructed.

For a transaction that:

- has a preceding identified read of a versioned instance; and
- later updates that same instance;

the remedy may:

1. ensure the read includes the version field;
2. convert `Update` to `CompareAndSet`; or
3. attach an observed-version comparison to an existing `Transition`, `AdvanceCursor`, or `Fence`.

If no suitable mutation exists, the remedy engine MUST NOT invent an assertion.

It should instead consider the existing real mechanisms:

1. strict lock;
2. serializable closure.

This removes a significant source of architecturally unrealistic automatic repairs.

---

# 33. Serialization diagnostics

User-facing explanations should refer to concrete concurrency mechanisms.

Old terminology such as:

```text
version validation
version bump
```

should disappear from new DSL 6 diagnostics.

Examples:

```text
`tx.adjust_balance` reads `object.account.balance`, while
`tx.post_credit` may concurrently update the same instance. The reader
does not hold a protecting lock and no later atomic mutation compares
the observed state or version before commit.
```

Successful evidence might say:

```text
`tx.adjust_balance` conditions its update on the version observed by
`read.adjust_balance.account`. Every committed mutation of the
versioned instance publishes a newer token, so a stale observation
cannot participate in a successful commit.
```

---

# 34. Visualization

`src/viz/transaction_proofs.rs` and the TypeScript visualization model should replace:

```text
version validation
```

evidence with labels such as:

```text
conditional mutation
compare-and-set
observed version guard
observed field guard
```

Version bumps are no longer separate graph operations.

For a CAS, the visualization should ideally show one atomic mechanism rather than an unrelated read and write node.

The detailed panel may report:

```text
Compare-and-set
Object: object.order
Compared: version
Expected from: read.cancel_order.order.version
Mutates: status
```

Domain-specific mechanisms should retain their own names:

```text
transition
cursor
fence
```

with additional observed-state comparisons shown as guards.

---

# 35. Confluence graph and reference extraction

All exhaustive matches over `TransactionStep` must be updated.

This includes at least:

```text
src/confluence/patch.rs
src/confluence/sketch.rs
src/confluence/graph_build.rs
src/confluence/graph_query.rs
src/confluence/commit.rs
src/confluence/mcp.rs
```

`CompareAndSet` contributes:

- target object reference;
- predicate value roots;
- comparison expected-value roots;
- update derivation roots.

`Upsert` contributes:

- target object reference;
- target predicate roots;
- insert derivation roots;
- update derivation roots.

`ValidateVersion` and `BumpVersion` branches are deleted.

---

# 36. Harness prompts and synthesis

Any model-facing documentation or prompting that currently teaches agents:

```text
read version
validate_version
...
bump_version
```

must be rewritten.

Agents should instead be taught:

> If a decision depends on an identified object read and the transaction later mutates that object, use an atomic conditional mutation comparing either the observed fields or the object's observed concurrency token.

They should also be explicitly told:

> Do not introduce a compare-and-set merely because an object has a version. Use it where the transaction actually relies on a stale-read check. Ordinary blind updates may remain ordinary updates.

This should reduce unnecessary concurrency ceremony in generated programs.

---

# 37. DSL migration guide

DSL 4/5 documents are not silently accepted as DSL 6.

The following transformations describe re-authoring.

## 37.1 Ordinary write

Before:

```yaml
- kind: write
  target: ...
  fields:
    - balance
  values: ...
```

After:

```yaml
- kind: update
  target: ...
  fields:
    - balance
  values: ...
```

## 37.2 Blind versioned mutation

Before:

```yaml
- kind: write
  target: ...
  fields:
    - balance
  values: ...

- kind: bump_version
  target: ...
```

After:

```yaml
- kind: update
  target: ...
  fields:
    - balance
  values: ...
```

The version publication is intrinsic.

## 37.3 Read-modify-write OCC

Before:

```yaml
- kind: read
  bind: read.account
  target: ...
  fields:
    kind: only
    fields:
      - balance
      - version

- kind: validate_version
  target: ...
  expected:
    source: transaction_read:read.account
    path: version

- kind: write
  target: ...
  fields:
    - balance
  values: ...

- kind: bump_version
  target: ...
```

After:

```yaml
- kind: read
  bind: read.account
  target: ...
  fields:
    kind: only
    fields:
      - balance
      - version

- kind: compare_and_set
  target: ...
  compare:
    - field: version
      expected:
        source: transaction_read:read.account
        path: version
  fields:
    - balance
  values: ...
```

## 37.4 Guarded transition

Before:

```yaml
read
validate_version
transition
bump_version
```

After:

```yaml
read

transition:
    compare observed version
```

Version publication is automatic.

## 37.5 Read-only validation

Before:

```text
read A
validate_version A
write B
```

There is no direct syntactic translation.

The architecture must choose a real mechanism:

```text
lock A
read A
write B
```

or:

```text
serializable isolation
read A
write B
```

or restructure the transaction around an actual conditional mutation.

This intentional refusal is one of the main correctness improvements of DSL 6.

---

# 38. Implementation changes by module

## 38.1 `src/spec/model.rs`

- bump `DSL_VERSION` to 6;
- make `DSL_READS` contain only 6;
- document this revision;
- remove version-validation terminology.

## 38.2 `src/spec/operation/transaction.rs`

Remove:

```rust
Write
ValidateVersion
BumpVersion
```

Introduce:

```rust
Update
CompareCondition
CompareAndSet
Upsert
```

Add optional comparison guards to:

```rust
StateTransition
AdvanceCursor
Fence
```

Update:

- `TransactionStep`;
- `roots()`;
- `rejects()`;
- serialization derives;
- documentation.

## 38.3 `src/spec/data_model.rs`

Rewrite `ObjectVersion` documentation.

Remove:

```text
every Write or Transition requires BumpVersion
```

Replace with intrinsic mutation-token semantics.

Retain version field type and managed-field restrictions.

## 38.4 `src/analyzer/validation/mod.rs`

Delete `validate_version_protocol`.

Replace it with focused validation covering:

- direct version writes;
- CAS identity;
- CAS comparison structure;
- upsert identity;
- upsert identity mutation;
- managed-field rules.

Reuse:

```text
selector_identifies_instance
version_field
object_identity
```

where applicable.

## 38.5 `src/analyzer/validation/error.rs`

Remove obsolete version-protocol errors and diagnostic codes.

Add CAS/upsert validation errors.

Rewrite direct-version-write diagnostics.

## 38.6 `src/analyzer/verification/transaction_conflicts.rs`

This is the principal checker change.

Remove:

```rust
VersionValidation
validations
bumps
observes_version()
validates()
bumps()
AccessMode::VersionValidate
AccessMode::VersionBump
```

Introduce internal conditional-mutation metadata.

Extend access derivation for:

- Update;
- CompareAndSet;
- Upsert;
- comparison guards on transition/cursor/fence.

Include synthetic version publication in mutation footprints.

Replace version-validation dependency evidence with observed-state conditional-mutation evidence.

## 38.7 `src/analyzer/verification/transaction_serializability.rs`

Replace descriptions and proof evidence referring to version validation.

The high-level SCC/cycle algorithm remains unchanged.

The implementation should continue to consume `DependencyEvidence`; only the evidence classification changes.

## 38.8 `src/analyzer/verification/transaction_ordering.rs`

No conceptual redesign is required.

Update exhaustive matches and shared access metadata.

Do not treat CAS as proof of `OrderedBy`.

## 38.9 `src/harness/executors/remedies.rs`

Delete the current version-validation/bump remedy.

Introduce conditional-mutation repair only when an actual same-instance mutation exists.

Fall back to lock or serializable isolation when it does not.

## 38.10 `src/confluence/sketch.rs`

Remove automatic `ValidateVersion` generation.

Remove `BumpVersion` finalization.

Generate CAS or guarded specialized mutation where a same-instance optimistic guard is appropriate.

## 38.11 Visualization

Update:

```text
src/viz/transaction_proofs.rs
src/viz/graph.rs
viz/src/types/model.ts
viz/src/lib/*
viz/src/panels/DetailPanel.tsx
viz/src/graph/OperationView.tsx
```

Remove old version-step rendering.

Add CAS/upsert rendering and conditional-mutation evidence.

## 38.12 Fixtures and documentation

Update:

```text
tests/fixtures/*
CONSEQA_DSL_SEMANTICS.md
src/harness/task_prompt.rs
reports/transaction-serialization-graph.md
```

and any revision documents that are treated as current guidance.

Historical revision documents need not be rewritten if explicitly historical.

---

# 39. Test plan

The implementation is incomplete until the following cases are covered.

## 39.1 Basic CAS

A CAS on a fully identified instance parses and validates.

A CAS without full identity fails validation.

A CAS without comparisons fails validation.

A CAS with invalid comparison paths fails ordinary path validation.

## 39.2 Rejection

A transaction containing CAS without a `rejected` arm fails validation.

A transaction containing CAS with a rejection arm succeeds.

## 39.3 Direct observed-field proof

Given:

```text
T1 reads X.a
T1 CAS X where a = observed(a)

T2 writes X.a
```

the relevant anti-dependency is constrained.

Changing the CAS comparison to an unrelated field makes the proof fail.

## 39.4 Version-token proof

Given:

```text
T1 reads X.a and X.version
T1 CAS X where version = observed(version)

T2 updates X.a
```

the anti-dependency is constrained.

No explicit writer bump exists.

## 39.5 Stale expected version

A comparison against:

```text
input.expected_version
```

is valid application behavior but is not credited as proof that a previous transaction read remained valid.

## 39.6 Unversioned CAS

Direct field CAS works on an unversioned object.

No version declaration is required.

## 39.7 Version publication

An ordinary update of a versioned object requires no bump step and produces no missing-bump error.

Conflict indexing nevertheless treats the operation as publishing a version change.

## 39.8 Direct version assignment

`update.fields` containing the version field fails.

`compare_and_set.fields` containing it fails.

`upsert.update_fields` containing it fails.

Comparing the version field is allowed.

## 39.9 Transition with observed version

A transition carrying an observed-version comparison constrains a relevant stale-read dependency.

Removing the comparison exposes the dependency where no other proof exists.

## 39.10 Cursor/fence guard composition

Existing cursor/fence ordering tests continue to pass.

Additional observed-state comparisons do not alter their ordering semantics.

## 39.11 Upsert identity

Two upserts of the same potential identity are treated as conflicting atomic identity mutations.

Upserts of identities proven disjoint do not conflict.

## 39.12 Upsert does not guard unrelated reads

Given:

```text
read A
upsert B
```

the upsert does not discharge an anti-dependency on A.

## 39.13 Read-only OCC removal

A former:

```text
read A
validate_version A
write B
```

test must become unproven unless:

- A is locked;
- the closure is serializable;
- or another genuine mechanism is introduced.

## 39.14 DSL version

DSL 6 parses.

DSL 4 and DSL 5 are refused by the v6 parser.

---

# 40. Acceptance criteria

The revision is complete when all of the following hold.

1. No public DSL type named `ValidateVersion` exists.
2. No public DSL type named `BumpVersion` exists.
3. No v6 YAML contains `validate_version` or `bump_version`.
4. No analyzer proof requires matching a reader's validation with a writer's explicit bump.
5. Versioned mutations publish concurrency-token changes intrinsically.
6. `compare_and_set` represents a genuine atomic compare-and-mutate operation.
7. Direct observed-field CAS can discharge appropriate identified-instance anti-dependencies.
8. Observed-version CAS can discharge such dependencies without writer-side annotations.
9. `upsert` represents atomic identity-based insert-or-update behavior without being treated as generic stale-read protection.
10. `transition`, `advance_cursor`, and `fence` remain domain-specific primitives while participating in common conditional-mutation analysis.
11. Transactions that require read stability but possess no real mechanism are refused rather than repaired with an abstract deferred validation.
12. Existing strict-lock and serializable-closure proof routes continue to function.
13. Existing transaction ordering semantics remain intact.
14. Visualization and diagnostics describe concrete mechanisms rather than the retired version protocol.
15. All exhaustive `TransactionStep` consumers compile against the v6 vocabulary.
16. Repository fixtures and tests contain no accidental dependence on the removed protocol.

---

# 41. Resulting conceptual model

After this revision, Conseqa's serialization vocabulary has three clear classes of concurrency mechanism.

## Isolation

```text
read_committed
snapshot
serializable
```

These describe database transaction isolation.

## Explicit locking

```text
lock shared
lock exclusive
```

These describe pessimistic concurrency control.

## Atomic guarded mutation

```text
compare_and_set
transition
advance_cursor
fence
upsert identity arbitration
```

These describe actual atomic state transitions whose success or failure constrains committed histories.

Object versions support guarded mutation but are not themselves transaction operations.

That yields a considerably simpler conceptual rule:

> **A stale observation is safe only when some real storage mechanism prevents a transaction from successfully committing on the basis of that stale observation.**

The serialization checker then asks which declared mechanism establishes that fact.

It no longer asks whether the model contains the synthetic pair:

```text
validate_version + bump_version
```

This makes the DSL closer to implementation practice, makes generated architectures more realistic, and makes the proof model easier to explain without weakening Conseqa's conservative serializability analysis.