# Conseqa Outbox Semantics Revision Specification

## 1. Status and scope

This revision introduces first-class **Outbox** semantics into Conseqa's hierarchical L0/L1 architecture model.

The design treats an outbox as:

- an **L0 typed logical message collection** belonging to a `DataModel`;
- writable only through an **`OutboxWriteEffect` executed inside a transaction**;
- consumable through an **L0 `OutboxInput`**, where one logical outbox message induces one logical operation invocation;
- realized at L1 through an **`OutboxRuntime`** describing delivery, partitioning, ordering, dispatch, member assignment, and optional opaque runtime batching.

The revision deliberately aligns outbox semantics with the existing topic/publication/subscription/idempotency model wherever the underlying mechanism is genuinely the same.

It deliberately does **not** introduce generic abstractions merely because two domain-specific concepts have similar mathematical structure.

In particular:

```text
Topic
Outbox

SubscriptionInput
OutboxInput

SubscriptionOrdering
OutboxOrdering

SubscriptionDispatch
OutboxDispatch
```

remain distinct semantic concepts.

Shared primitives are reused only where they already mean the same thing:

```text
Schema
MessageSelector
MessageIdentity
DeliverySemantics
IdempotencyKeyPropagation
MemberAssignment
ExecutionPool
```

---

# 2. Goals

This revision SHALL allow Conseqa to express the following architecture:

```text
request / message
      |
      v
Operation
      |
      v
Transaction
   /       \
  /         \
DataObject   OutboxWriteEffect
mutation          |
                  v
             Outbox message
                  |
                  v
             OutboxInput
                  |
                  v
             Operation
                  |
                  v
          arbitrary effect cascade
```

while preserving the following properties:

1. The application-state mutation and outbox message admission are one atomic transaction outcome.
2. The outbox write is visible to Conseqa as part of the producing operation's side-effect blast radius.
3. Outbox messages have typed schemas and may have logical message identities analogous to topic messages.
4. Idempotency-key lineage may propagate through an outbox write and into an outbox consumer operation.
5. Idempotency analysis follows outbox consumers transitively in the same manner that it currently follows topic subscribers.
6. L0 remains an opaque per-message logical application machine.
7. Runtime batching does not require batch iteration, wait groups, explicit concurrency, or explicit acknowledgment statements in the L0 program.
8. L1 exposes only the runtime facts needed for topology and correctness analysis.
9. Outbox runtime partitioning and grouping are one semantic concept: **partitioning**.
10. No priority mechanism is introduced initially.
11. No polling mechanism is prescribed.
12. No exactly-once guarantee is inferred merely from use of an outbox.

---

# 3. Non-goals

This revision SHALL NOT model:

```text
physical outbox tables
SQL polling queries
SELECT ... FOR UPDATE
SKIP LOCKED
database indexes
CDC implementation
WAL decoding
broker implementation
poll intervals
batch sizes
batch wait durations
retry backoff
dead-letter queues
visibility timeouts
lease durations
priority scheduling
physical partition count
worker replica count
exactly-once external execution
```

These may be supplied by realization tooling or an external simulation/deployment scenario where appropriate.

The revision also SHALL NOT introduce:

```text
generic RuntimeDomainKey
generic MessageChannel
generic Consumer
generic Dispatch supertype
explicit Ack program statements
batch collections in the L0 program
batch iteration constructs
wait-group constructs
general-purpose loops
```

---

# 4. Layering overview

The intended decomposition is:

```text
L0 — Abstract Application Semantics
────────────────────────────────────

DataModel
    |
    +-- DataObject
    |
    +-- Outbox
          |
          +-- admitted message schemas
          +-- logical message identity

Transaction
    |
    +-- DataObject mutations
    +-- OutboxWriteEffect

Outbox
    |
    v
OutboxInput
    |
    v
Operation(message)


L1 — Runtime Topology and Realization Semantics
────────────────────────────────────────────────

OutboxRuntime
    |
    +-- delivery
    +-- partitioning
    +-- ordering
    |
    +-- dispatch
           |
           +-- MemberAssignment
           +-- optional batching semantics
           |
           v
      ExecutionPool
```

L0 describes the logical application machine.

L1 describes how committed outbox messages are presented to and placed onto runtime execution resources.

---

# 5. L0 — `DataModel` owns Outboxes

`DataModel` SHALL be extended to contain logical outboxes in addition to persistent objects.

Conceptually:

```rust
pub struct DataModel {
    pub objects: BTreeMap<Id, DataObject>,
    pub outboxes: BTreeMap<Id, Outbox>,
}
```

An outbox belongs to the logical transactional state boundary represented by its owning `DataModel`.

This means that a transaction declaring:

```text
data_model = Orders
```

may atomically mutate:

```text
Orders.DataObjects
```

and admit messages to:

```text
Orders.Outboxes
```

within the same logical commit.

The declaration does not imply that objects and outboxes use:

```text
one SQL database
one physical server
one table namespace
one storage technology
```

A conforming implementation must merely realize the declared atomic transactional boundary.

---

# 6. L0 — `Outbox`

Conceptually:

```rust
pub struct Outbox {
    pub messages: BTreeSet<Id>,
    pub message_identity: MessageIdentity,
}
```

`messages` is the set of message schemas that the outbox may contain.

For example:

```yaml
data_models:
  data.orders:
    objects:
      ...
    outboxes:
      outbox.integration_events:
        messages:
          - schema.OrderCreated
          - schema.OrderCancelled
```

Membership means:

> The outbox is permitted to durably contain a logical message of this schema.

It does not assert that any such message is ever produced.

An `Outbox` is not a `Topic`.

The distinction is:

```text
Topic
    logical messaging boundary
    written by PublicationEffect

Outbox
    logical transactional message collection
    owned by DataModel
    written by OutboxWriteEffect
```

---

# 7. Outbox message identity

An outbox SHALL support the same `MessageIdentity` semantic concept used by topics.

No `OutboxMessageIdentity` duplicate type is necessary unless implementation ergonomics later require one.

For:

```text
message_identity = keyed(...)
```

the guarantee is:

> Any two outbox messages covered by the identity mapping whose evaluated identity tuples are equal represent the same logical message and therefore have the same schema and equal modeled payload.

For example:

```yaml
message_identity:
  kind: keyed
  mapping:
    schema.OrderCreated:
      - event_id
    schema.OrderCancelled:
      - event_id
```

establishes:

```text
equal event_id
    =>
same logical outbox message
```

for mapped schemas.

As with topic message identity:

```text
message identity
    !=
partition key

message identity
    !=
business object identity
```

For example:

```text
message_identity = event_id
partitioning     = tenant_id
```

is entirely coherent.

Outbox message identity does not imply:

```text
deduplicated physical writes
at-most-once delivery
at-most-once consumer execution
exactly-once processing
```

It establishes logical message identity only.

---

# 8. L0 — `OutboxWriteEffect`

A new effect kind SHALL represent transactional publication of a message into an outbox.

Conceptually:

```rust
pub struct OutboxWriteEffect {
    pub outbox: OutboxRef,
    pub schema: Id,
    pub idempotency_key_propagation:
        Vec<IdempotencyKeyPropagation>,
}
```

and:

```rust
pub enum Effect {
    Publication(PublicationEffect),
    Request(RequestEffect),
    External(ExternalEffect),
    OutboxWrite(OutboxWriteEffect),
}
```

The public concepts remain distinct.

`OutboxWriteEffect` does not become a subtype of `PublicationEffect`.

They simply share appropriate message-producing semantics.

---

# 9. Effect semantics must be broadened

The existing conceptual definition:

```text
Effects describe work outside the operation's immediate transaction state.
```

is too narrow once `OutboxWriteEffect` exists.

The revised definition SHOULD be approximately:

> An `Effect` describes semantically relevant work or state transition caused by an operation beyond ordinary `DataObject` mutation, including external interactions, message publication, downstream requests, and transactional admission of messages to an Outbox.

The important property is that Effects form edges in the operation's externally relevant or transitively relevant side-effect graph.

`OutboxWriteEffect` therefore participates in:

```text
operation side-effect enumeration
idempotency analysis
value lineage
effect blast-radius analysis
visualization
downstream traversal
```

even though its execution is transaction-bound.

---

# 10. Outbox writes are transaction-exclusive effects

Unlike ordinary Effects, an `OutboxWriteEffect` SHALL be executable only as a transaction step.

Conceptually:

```rust
pub struct WriteOutboxEffect {
    pub effect_id: Id,
    pub effect: OutboxWriteEffect,
    pub values: Derivation,
}
```

and:

```rust
pub enum TransactionStep {
    ...
    WriteOutboxEffect(WriteOutboxEffect),
}
```

Surface syntax may use a concise name such as:

```yaml
- kind: write_outbox
  effect_id: effect.order_created_outbox
  effect:
    outbox: outbox.integration_events
    schema: schema.OrderCreated
    idempotency_key_propagation:
      ...
  values:
    kind: deterministic
    from:
      ...
```

The transaction step is an **effect execution site**, not merely a persistent-object insertion statement.

`effect_id` SHALL have the same stable execution-site role as existing inline effect site IDs:

```text
value lineage
idempotency-key propagation
diagnostics
proof evidence
visualization
```

---

# 11. Prohibited execution sites

In the initial revision, `OutboxWriteEffect` SHALL NOT be accepted by:

```text
execute_effect
establish_effect_intent
execute_effect_intent
state-machine transition side-effect declarations
```

The only legal execution site is:

```text
transaction -> write_outbox
```

This prevents the DSL from accidentally claiming transactional outbox semantics where no containing application transaction exists.

If a state-machine transition should cause an outbox message, an operation may initially express:

```text
transaction:
    transition(...)
    write_outbox(...)
```

as two logically ordered transaction steps.

A future revision may add transition-owned transactional outbox effects if there is a concrete need.

---

# 12. Effect instance construction

As with other effect execution sites:

```text
effect
```

declares the contract, while:

```text
values
```

declares provenance for the concrete effect instance.

For:

```text
WriteOutboxEffect(effect_id, E, D)
```

reaching the transaction step:

1. constructs one logical message instance conforming to `E.schema`;
2. evaluates its values according to derivation `D`;
3. stages the resulting outbox-write effect inside the current transaction;
4. makes the outbox message durable only if the containing transaction commits.

`D` is evaluated in the transaction context at that step.

It may therefore reference transaction-local values that are valid at that point according to the existing transaction provenance rules.

---

# 13. Atomicity semantics

An outbox write participates in the atomicity of its containing transaction.

Suppose:

```text
Transaction T:
    mutate W
    write outbox message M
```

Then:

```text
T aborts
    =>
W does not commit
AND
M is not admitted to the Outbox
```

while:

```text
T commits
    =>
W commits
AND
M is durably admitted to the Outbox
```

There is no state in the L0 abstract machine where the transaction successfully commits its ordinary application mutations while the declared outbox write from that commit is absent.

Likewise there is no successful outbox admission from an aborted transaction.

This is the defining atomic outbox property.

---

# 14. Outbox write versus ordinary publication

The two message-producing effects differ primarily in their boundary:

```text
PublicationEffect
    destination = Topic
    execution is external to transaction state
    no transaction atomicity implied

OutboxWriteEffect
    destination = DataModel.Outbox
    execution only inside Transaction
    admission occurs atomically with transaction commit
```

Both may declare:

```text
schema
idempotency_key_propagation
```

Both produce logical typed messages.

Both participate in transitive idempotency analysis.

Neither alone implies exactly-once downstream processing.

---

# 15. `OutboxWriteEffect` has no synchronous result

An outbox write SHALL have no first-class synchronous `Result`.

The transaction determines whether the staged write commits.

Therefore the outbox-write transaction step SHALL NOT bind an effect result.

Application control that needs data produced inside the transaction must continue to use normal transaction outputs.

An outbox write is not a substitute for:

```text
TransactionOutput
```

and its message payload is not implicitly available as a later operation binding merely because the write succeeded.

---

# 16. Outbox writes and transaction idempotency

An `OutboxWriteEffect` does not introduce a second independent transaction.

Its logical admission participates in the containing transaction's commit.

Therefore:

```text
Transaction T
DeduplicatedBy(K)
```

also constrains outbox writes contained in `T`.

If:

```text
Commit(T,K)
```

already exists, the transaction body does not commit again.

Consequently, a contained `OutboxWriteEffect` does not produce another committed occurrence from that repeated encounter.

This provides one possible producer-side route for preventing duplicate outbox admission.

It must not be confused with outbox message identity.

The two facts answer different questions:

```text
Transaction DeduplicatedBy(K)
    -> may prevent another logical transaction commit

Outbox.message_identity
    -> identifies when messages represent the same logical message
```

---

# 17. Outbox writes are part of the operation effect blast radius

The operation's effect graph SHALL treat an outbox write exactly as a semantically visible side-effect edge.

For:

```text
Operation A
    |
    v
Transaction
    |
    v
OutboxWriteEffect M
    |
    v
Outbox O
    |
    +--> OutboxInput -> Operation B
    |
    +--> OutboxInput -> Operation C
```

the side-effect blast radius of `A` includes:

```text
M
B
C
```

and every transitive effect reachable through `B` and `C`.

This SHALL apply to:

```text
idempotency verification
effect-cascade visualization
dependency analysis
simulation graph construction
```

An outbox boundary does not terminate the causal effect graph.

---

# 18. L0 — `OutboxInput`

A new input kind SHALL represent invocation from a committed outbox message.

Conceptually:

```rust
pub struct OutboxInput {
    pub outbox: OutboxRef,
    pub messages: MessageSelector,
    pub acknowledge_on_success: bool,
}
```

and:

```rust
pub enum Input {
    Request(RequestInput),
    Subscription(SubscriptionInput),
    Outbox(OutboxInput),
}
```

An `OutboxInput` is a logical consumer boundary.

For each admitted outbox message `M`:

```text
Outbox message M
      |
      v
OutboxInput
      |
      v
one logical Operation invocation
```

The input payload is `M`.

The L0 operation program therefore remains defined over **one logical message**.

---

# 19. Message selection

`OutboxInput.messages` SHOULD reuse `MessageSelector`.

Thus:

```text
all
```

means every schema admitted by the source outbox may invoke the operation.

And:

```text
only [...]
```

restricts this input to the listed admitted schemas.

It does not restrict what other schemas the outbox itself may contain.

This is intentionally parallel to `SubscriptionInput`.

---

# 20. No L0 batch payload

An `OutboxInput` SHALL NOT expose:

```text
Vec<Message>
Batch<Message>
batch iterator
batch index
```

to the L0 program.

One logical outbox message always corresponds to one logical L0 invocation:

```text
M1 -> Operation(M1)
M2 -> Operation(M2)
M3 -> Operation(M3)
```

A runtime may execute those logical invocations using a physical/runtime batch, but the L0 application machine does not model that implementation structure.

Conseqa therefore does not need:

```text
loops over batch entries
parallel-for
wait groups
per-batch control flow
batch result aggregation
```

in order to model ordinary outbox consumers.

---

# 21. L0 acknowledgement on successful completion

Acknowledgement SHALL be represented as an input-level application semantic rather than an explicit program statement.

For:

```text
acknowledge_on_success = true
```

the semantic rule is:

> When an invocation triggered through this input reaches successful logical completion, the triggering logical source item is acknowledged for this consumer.

For an outbox-driven operation this is normally:

```text
Outbox message M
      |
      v
Operation(M)
      |
      v
complete
      |
      v
acknowledge M
```

The acknowledgement is associated with the particular logical `OutboxInput`, not globally with the message across every consumer.

Therefore, if multiple `OutboxInput`s consume the same outbox:

```text
Input A acknowledgement
    !=
Input B acknowledgement
```

Each represents an independent logical consumer relationship.

---

# 22. Successful completion

For an outbox-triggered invocation, successful completion means that the logical invocation reaches its normal terminal without an unresolved execution failure.

The program does not contain an explicit:

```text
Ack
```

statement.

Conseqa therefore intentionally does not distinguish:

```text
effect succeeded
ack system call began
ack system call returned
```

inside the application program.

Those mechanics are below the L0 abstraction.

The L0 fact is simply:

```text
successful logical completion
        +
acknowledge_on_success
        =>
source item acknowledged
```

---

# 23. Failure before acknowledgement

If an outbox-triggered invocation does not successfully complete:

```text
acknowledge_on_success
```

does not take effect.

Thus:

```text
dispatch M
    |
execute application work
    |
failure / crash
    |
M remains logically unacknowledged
```

Whether another delivery attempt may occur is governed by delivery semantics.

Acknowledgement does not itself imply:

```text
at-most-once execution
exactly-once execution
eventual redelivery
eventual success
```

---

# 24. Companion acknowledgement semantics for subscriptions

To keep topic subscription and outbox consumption aligned, this revision SHOULD add the same:

```rust
acknowledge_on_success: bool
```

semantic to `SubscriptionInput`.

Thus both source-driven inputs follow:

```text
one logical source item
    ->
one logical operation invocation
    ->
successful completion
    ->
optional acknowledgement
```

The source item differs:

```text
SubscriptionInput -> Topic message delivery
OutboxInput       -> Outbox message delivery
```

but the application-level acknowledgement concept is the same.

No generic public `ConsumerInput` abstraction is required.

---

# 25. L1 — `OutboxRuntime`

Runtime consumption of an `OutboxInput` SHALL be described through an L1 `OutboxRuntime`.

Conceptually:

```rust
pub struct OutboxRuntime {
    pub delivery: DeliverySemantics,
    pub partitioning: OutboxPartitioning,
    pub ordering: OutboxOrdering,
    pub dispatch: OutboxDispatch,
}
```

Like `SubscriptionRuntime`, an `OutboxRuntime` SHOULD target an existing operation/input boundary:

```text
(operation_id, input_id)
```

where:

```text
input.kind == outbox
```

This allows one logical outbox to have multiple modeled consumer relationships, each with its own runtime behavior.

---

# 26. Outbox runtime absence

L1 remains optional.

If an `OutboxInput` has no corresponding `OutboxRuntime`, Conseqa knows that the logical consumption relationship exists but has no usable runtime facts about:

```text
delivery multiplicity
partitioning
ordering
member assignment
execution pool
batch behavior
```

Absence is epistemic.

It must not be interpreted as evidence that no concrete runtime exists.

---

# 27. L1 delivery semantics

`OutboxRuntime.delivery` SHOULD reuse the existing `DeliverySemantics` vocabulary.

The semantic subject is one logical outbox message relative to one `OutboxInput`.

### `unspecified`

No usable duplicate/loss fact exists.

### `at_most_once`

The same logical outbox message is delivered to this input no more than once.

Loss may occur.

It does not imply exactly-once execution.

### `at_least_once`

One committed logical outbox message may produce repeated delivery attempts to this input.

Duplicate operation invocation must therefore be considered possible.

No retry count, retry timing, or eventual-success guarantee is implied.

Where:

```text
acknowledge_on_success = true
```

a successful acknowledged logical invocation ends ordinary redelivery of that item for that consumer.

Uncertainty or failure before acknowledgement may admit another attempt according to delivery semantics.

---

# 28. L1 — `OutboxPartitioning`

Outbox runtime grouping SHALL be represented exclusively as partitioning.

Conceptually:

```rust
pub enum OutboxPartitioning {
    None,
    Keyed(OutboxPartitionKey),
}
```

There SHALL NOT also be:

```text
OutboxGrouping
```

in the initial model.

For the outbox abstraction:

> Partition identity is the logical grouping identity used for runtime consumption.

---

# 29. Unpartitioned outbox

For:

```text
partitioning = none
```

the runtime exposes no keyed subdivision of the outbox consumption population.

The relevant messages form one undivided runtime consumption domain for this consumer.

This does not imply:

```text
one physical database partition
one SQL table
one worker
one host
```

unless another declaration establishes such a fact.

---

# 30. Keyed outbox partitioning

Conceptually:

```rust
pub struct OutboxPartitionKey {
    pub mapping: BTreeMap<Id, Vec<FieldPath>>,
}
```

For messages `A` and `B`:

```text
partition_key(A) = partition_key(B)
    =>
partition(A) = partition(B)
```

Different partition keys may still be physically co-located.

Partition identity is semantic L1 topology, not a physical node identifier.

---

# 31. Typed partition mapping

Because an Outbox may carry several message schemas, keyed partitioning maps each consumed schema into the common logical partition-key domain.

For example:

```yaml
partitioning:
  kind: keyed
  mapping:
    schema.OrderCreated:
      - tenant_id
    schema.OrderCancelled:
      - tenant
```

may assert that:

```text
OrderCreated.tenant_id
OrderCancelled.tenant
```

represent the same logical outbox partition-key domain.

Tuple arity and logical component types must be compatible.

Only message schemas admitted by the targeted `OutboxInput` need participate in that consumer runtime's mapping.

---

# 32. Partitioning does not imply ordering

For:

```text
partitioning = keyed(tenant_id)
```

Conseqa may infer:

```text
equal tenant_id
    =>
same outbox partition
```

but may not infer:

```text
same partition
    =>
ordered

same partition
    =>
serialized

same partition
    =>
same execution member
```

Those require separate L1 facts.

---

# 33. L1 — `OutboxOrdering`

Outbox ordering SHALL use an outbox-specific enum.

Conceptually:

```rust
pub enum OutboxOrdering {
    None,
    Global,
    Partition,
}
```

This SHALL remain distinct from `SubscriptionOrdering`.

The fact that their proof structures may be analogous does not collapse their semantic concepts.

---

# 34. `OutboxOrdering::None`

`None` declares that the outbox runtime provides no usable relative delivery/dispatch precedence.

Keyed partitioning may still exist.

Thus:

```text
partitioning = tenant_id
ordering     = none
```

is valid and meaningful.

---

# 35. `OutboxOrdering::Global`

`Global` establishes one logical order over the messages consumed through this runtime.

Conceptually:

```text
A <outbox B
```

establishes a runtime precedence between their deliveries.

It does not by itself imply:

```text
A operation completes before B begins
A and B cannot overlap
A effects complete before B effects
```

Execution topology must preserve the precedence where such stronger conclusions are required.

---

# 36. `OutboxOrdering::Partition`

`Partition` establishes an independent logical order within each keyed outbox partition.

For:

```text
partition(A) = partition(B)
```

the runtime may establish:

```text
A <partition B
```

No ordering is asserted between distinct partitions.

`Partition` ordering is valid only when:

```text
partitioning = keyed(...)
```

It is structurally invalid with `partitioning = none`.

---

# 37. Outbox ordering does not invent business causality

As with topic ordering, an outbox runtime may impose a deterministic runtime sequence on messages produced by otherwise concurrent transactions.

That sequence is a real outbox runtime order.

It does not retroactively establish:

```text
business causality
transaction happens-before
semantic command precedence
```

between the producing operations.

Ordering verification must continue to distinguish:

```text
runtime-imposed precedence
```

from:

```text
upstream semantic precedence that must be preserved
```

---

# 38. L1 — `OutboxDispatch`

Outbox runtime placement SHALL use:

```rust
pub struct OutboxDispatch {
    pub pool: Id,
    pub member_assignment: MemberAssignment,
    pub batching: Option<BatchingSemantics>,
}
```

`OutboxDispatch` remains distinct from `SubscriptionDispatch`.

Both deliberately reuse:

```text
MemberAssignment
ExecutionPool
```

because those mechanisms genuinely have the same semantics.

---

# 39. Dispatch responsibility

`OutboxDispatch` answers:

> Which member of the referenced ExecutionPool owns the outbox consumption partition or scope from which this logical invocation is dispatched?

For a keyed outbox:

```text
outbox message
      |
      | partition key
      v
outbox partition
      |
      | MemberAssignment
      v
ExecutionPool member
      |
      v
logical OutboxInput invocation
```

For an unpartitioned outbox, the undivided consumption scope is the assignment subject.

---

# 40. Member assignment

`OutboxDispatch.member_assignment` SHALL use the existing normative `MemberAssignment` semantics.

For example:

```text
consistent_hash
```

means equal partition domains remain owned by the same execution-pool member during a stable ownership epoch, subject to the existing safe ownership-transfer semantics.

No outbox-specific hash, routing, lease, or handoff semantics are introduced.

Conseqa does not prescribe:

```text
Ketama
rendezvous hashing
partition leases
consumer-group protocol
database row locks
worker election
```

---

# 41. ExecutionPool remains the concurrency primitive

No outbox-specific general worker concurrency field SHALL be added.

There SHALL be no:

```text
OutboxRuntime.concurrency
OutboxDispatch.concurrency
OutboxConsumer.concurrency
partition_concurrency
poller_concurrency
```

General runtime execution concurrency remains represented by:

```text
ExecutionPool.member_concurrency
```

where applicable to the modeled logical invocation topology.

---

# 42. Runtime batching

Batching is permitted as an L1 realization fact because a concrete consumer may retrieve or dispatch several logical source items together even though L0 models each item as an independent logical invocation.

Conceptually:

```rust
pub struct BatchingSemantics {
    pub ordering: BatchOrderingPreservation,
}
```

with:

```rust
pub enum BatchOrderingPreservation {
    Preserved,
    Unspecified,
}
```

The exact names may be adjusted during implementation.

The important distinction is:

```text
Preserved
    -> existing source ordering may continue through batch processing

Unspecified
    -> no usable fact says batch processing preserves existing ordering
```

`Unspecified` does not assert that reordering definitely occurs.

---

# 43. Batch implementation remains opaque

A runtime batch may internally be implemented using:

```text
sequential iteration
parallel futures
bounded parallel map
thread pools
wait groups
vectorized APIs
cooperative tasks
```

Conseqa does not model which one occurs.

L0 continues to reason as:

```text
M1 -> Operation(M1)
M2 -> Operation(M2)
M3 -> Operation(M3)
```

not:

```text
Batch[M1,M2,M3] -> BatchOperation
```

Batching is therefore a realization of several logical source-item evaluations, not a new application payload type.

---

# 44. Batch ordering preservation

If an upstream Outbox runtime establishes:

```text
A < B
```

and:

```text
batching.ordering = Preserved
```

then the runtime guarantees that its opaque batch-processing mechanism does not allow `B` to overtake `A` in a way that violates the established ordering relation.

Conseqa does not require the implementation literally to execute:

```text
A completely
then
B
```

if an alternative implementation is observationally consistent with the declared ordering guarantee.

If batching ordering is unspecified, the verifier SHALL NOT propagate the source ordering guarantee through the batch-processing stage.

---

# 45. Batching and serialization

Batch ordering preservation is not automatically a serialization guarantee.

Opaque batch processing may permit overlap among logical item evaluations.

Therefore:

```text
batching.ordering = Preserved
```

does not by itself prove:

```text
SerializedBy(K)
```

Likewise, where batch-internal execution is opaque, `ExecutionPool.member_concurrency` must not silently be interpreted as a proof about unmodeled batch-internal parallelism.

If a future serialization proof requires explicit guarantees about batch-internal overlap, a separate semantic extension will be required.

This revision exposes only the ordering-preservation fact currently needed.

---

# 46. Acknowledgement under batching

Batching does not change L0 acknowledgement semantics.

If a runtime batch contains:

```text
M1
M2
M3
```

Conseqa still models three logical item invocations:

```text
Operation(M1)
Operation(M2)
Operation(M3)
```

If:

```text
acknowledge_on_success = true
```

then each logical message is acknowledged when its own logical invocation successfully completes.

Conseqa does not model whether the concrete runtime sends:

```text
one physical batch ACK
three physical ACKs
offset commit
row deletes
lease completion
```

The physical acknowledgement mechanism is below the abstraction boundary.

---

# 47. Idempotency-key propagation

`OutboxWriteEffect.idempotency_key_propagation` SHALL have exactly the same fundamental meaning as propagation on `PublicationEffect`:

> The declared target fields of the emitted outbox message carry the same logical idempotency identity as the declared source values.

It is lineage only.

It SHALL NOT imply:

```text
deduplication
at-most-once write
at-most-once delivery
consumer idempotency
exactly-once processing
```

---

# 48. Consumer-side lineage through an Outbox

The idempotency solver SHALL extend its current producer/consumer lineage rules to outboxes.

Suppose producer `P` has governing idempotency key:

```text
K = request_id
```

and writes:

```text
OutboxWriteEffect:
    schema = OrderCreated

propagation:
    request_id -> event_id
```

where:

```text
Outbox.message_identity(OrderCreated)
    = event_id
```

Then the analyzer may record:

```text
producer logical key
      |
      | propagation
      v
OrderCreated.event_id
      |
      | outbox message identity
      v
logical outbox message identity
```

An `OutboxInput` consuming that message may then use:

```text
input.event_id
```

as the root of its own `IdempotencyRequirement`.

This continues the same logical key lineage across the outbox boundary.

---

# 49. Message identity and propagation remain distinct

As with topics:

```text
message_identity
```

and:

```text
idempotency_key_propagation
```

do different jobs.

`message_identity` asserts:

```text
equal identity tuple
    =>
same logical message and payload
```

Propagation asserts:

```text
these message fields carry this upstream logical idempotency identity
```

Neither substitutes for the other.

A consumer may rely on a declared outbox message identity even where no modeled producer propagation exists.

Propagation supplies lineage/provenance about where that identity came from.

---

# 50. Duplicate OutboxWriteEffect — producer-suppressed route

For an upstream operation idempotency requirement, one route to safely handling a potentially repeated `OutboxWriteEffect` is to establish that the containing transaction does not commit the outbox effect again for the governing logical attempt class.

For example:

```text
Operation governing key K
        +
Transaction DeduplicatedBy(K')
        +
K' proven stable/equivalent for the repeated attempts
        |
        v
at most one logical transaction commit
        |
        v
at most one committed occurrence
of the contained OutboxWriteEffect
```

The existing transaction-idempotency rules SHOULD be reused rather than inventing an outbox-specific deduplication guarantee.

---

# 51. Duplicate OutboxWriteEffect — message/consumer route

When repeated transaction executions may commit repeated outbox writes, the publication-style route SHALL apply.

A duplicate `OutboxWriteEffect` is safe for an upstream idempotency requirement when:

1. the destination Outbox declares keyed message identity covering the written schema;
2. the outbox-write message instance is class-fixed relative to the governing attempt population;
3. repeated writes therefore represent the same logical outbox message; and
4. every modeled consumer of that logical message collapses duplicate resulting work.

This is directly analogous to duplicate topic publication.

Condition 1–3 establish:

```text
duplicate write attempts
    ->
same logical outbox message
```

rather than:

```text
two distinct logical messages
```

Condition 4 establishes that any additional delivery multiplicity does not create externally distinguishable duplicate downstream work.

---

# 52. Class-fixed outbox message instance

For an outbox write to be treated as the same logical message across repeated producer attempts, its constructed payload must be stable relative to the governing idempotency class.

The verifier SHALL use the same provenance/replay-stability principles used for other effect instances.

For example:

```text
values:
    deterministic
    from replay-stable roots
```

may establish a class-fixed outbox message instance.

A write containing a fresh random identifier or unstable timestamp on every repeated attempt is not class-fixed merely because the Outbox declares a message-identity field.

The declared implementation must actually conform to the identity claim.

---

# 53. Outbox consumer duplicate collapse

For an outbox message schema `S`, a modeled consumer is an operation whose `OutboxInput`:

```text
references the destination Outbox
AND
admits schema S
```

That consumer collapses duplicate delivery for upstream idempotency analysis when either:

1. it declares an `IdempotencyRequirement` keyed from that `OutboxInput` and that requirement is itself proven; or
2. its corresponding `OutboxRuntime.delivery` is `at_most_once`.

This mirrors the existing topic-consumer rule.

`acknowledge_on_success` alone does not prove duplicate collapse.

It defines the application's consumption-completion boundary, not a duplicate-delivery guarantee.

---

# 54. Closed-world consumer analysis

The upstream idempotency requirement remains transitive.

For:

```text
Operation A
   |
OutboxWriteEffect
   |
Outbox O
   |
   +--> Operation B
   |
   +--> Operation C
```

a duplicate outbox write is safe only if every modeled consumer whose selection admits that message schema safely collapses any admitted duplicate work.

An unmodeled external consumer remains outside Conseqa's closed world.

Proofs remain conditional on model completeness/conformance.

---

# 55. Idempotency fixpoint integration

Outbox consumer dependencies SHALL participate in the same greatest-fixpoint/coinductive idempotency solver used for publication and request cycles.

For example:

```text
A
 |
 v
Outbox
 |
 v
B
 |
 v
Topic
 |
 v
C
 |
 v
Request
 |
 +------> A
```

is one connected transitive idempotency problem.

An outbox boundary does not break the fixpoint.

The analyzer SHALL NOT run a separate "outbox idempotency solver."

---

# 56. Propagation plays no role in producer-side duplicate discharge

As with `PublicationEffect`, `idempotency_key_propagation` SHALL NOT itself make repeated outbox writes safe.

If the outbox-write instance is class-fixed, repeated attempts already construct payload-equal logical messages.

Propagation is useful for tracing identity into downstream consumers.

It is not a producer-side deduplication mechanism.

This distinction must remain normative.

---

# 57. `ValueSource::effect`

`ValueSource::effect` SHALL be extended to permit payload-field references to an `OutboxWriteEffect` execution site.

For example:

```text
source: effect:effect.order_created_outbox
path: event_id
```

may identify a field in the logical message payload constructed at that outbox-write site where such a reference is otherwise valid.

The execution-site ID resolves to the inline `WriteOutboxEffect.effect_id`.

As elsewhere, this establishes inspectable value lineage only in semantic contexts that explicitly use that reference.

It does not imply that the effect has independently committed.

---

# 58. OutboxInput as an idempotency governing source

An `IdempotencyRequirement` on an outbox-consumer operation may use fields from the triggering `OutboxInput` exactly as a subscription-driven requirement uses fields from its subscription input.

For example:

```yaml
requirements:
  idempotency:
    key:
      - source: input:input.outbox_order_created
        path: event_id
```

The governing attempt population is:

> Invocations triggered through that OutboxInput sharing the declared key.

An invocation triggered through another input is outside that requirement's attempt population unless another requirement covers it.

---

# 59. Outbox message identity pins consumer payloads

When an `OutboxInput`'s governing idempotency key corresponds to the source Outbox's declared keyed message identity, the message identity may establish payload stability for repeated deliveries of the same logical message.

Thus the same principle applies to:

```text
Topic -> SubscriptionInput
```

and:

```text
Outbox -> OutboxInput
```

without introducing a generic channel abstraction.

---

# 60. Ordering requirements sourced from OutboxInput

An Outbox runtime ordering guarantee may serve as a precedence source for an `OrderingRequirement` on an outbox-consumer operation.

For keyed partition ordering, the relevant proof structure is conceptually:

```text
OutboxRuntime establishes
partition-relative precedence K
        +
consumer requirement key corresponds to K
        +
OutboxDispatch preserves assignment/ownership
        +
batch processing preserves established order
        +
any additional execution facts needed
        |
        v
OrderedBy(K)
```

The precise proof SHALL preserve the existing distinction between:

```text
ordering
```

and:

```text
serialization
```

A partition-order declaration is not automatically a no-overlap guarantee.

---

# 61. Global outbox ordering as precedence source

For:

```text
OutboxOrdering::Global
```

the runtime provides a stronger precedence source covering all messages in the targeted consumption population.

A keyed operation requirement may use the relevant restriction of that global order if the required key's attempt population is otherwise established.

Global order does not automatically imply globally serialized operation execution.

---

# 62. Partition assignment and ordering

For:

```text
partitioning = keyed(K)
ordering     = partition
```

the partition domain exists before member assignment.

`OutboxDispatch.member_assignment` maps that existing partition identity to an execution-pool member.

Thus:

```text
partition key
    -> partition identity
    -> member assignment
    -> ExecutionPool member
```

The analyzer SHALL NOT conflate:

```text
partition identity
```

with:

```text
worker identity
```

---

# 63. Ordering across ownership transfer

If a `MemberAssignment` is relied upon to preserve outbox partition ordering through reassignment, the same safe-handoff principle used elsewhere SHALL apply.

If partition `K` moves:

```text
member A -> member B
```

the realization must not permit an ownership transition that invalidates the declared ordering guarantee.

Conseqa models the resulting guarantee.

It does not prescribe leases, fencing, draining, or consumer-group protocols.

---

# 64. Interaction with batching and ordering proofs

When:

```text
dispatch.batching = null
```

no batch-specific ordering obstacle exists.

When batching exists with:

```text
ordering = Preserved
```

the established outbox ordering may pass through the batching stage.

When batching exists with:

```text
ordering = Unspecified
```

the batching stage provides no evidence that established order survives execution.

An ordering proof depending on that precedence must therefore stop unless some other declaration independently supplies the necessary guarantee.

---

# 65. Batching does not alter message identity or partition identity

Opaque batching SHALL NOT change the semantic identity of any source item.

For each batched item:

```text
message identity
partition identity
input identity
```

remain those of the individual logical message.

A physical batch is not a new logical message and does not create a new idempotency identity.

---

# 66. Multiple Outbox consumers

The initial model MAY allow several `OutboxInput`s to reference the same `Outbox`.

Each represents a separate logical consumer relationship.

For:

```text
Outbox O
    |
    +--> Input A
    +--> Input B
```

the two inputs may have independent:

```text
message selection
acknowledgement behavior
OutboxRuntime delivery semantics
partitioning
ordering
dispatch
ExecutionPool
```

This permits both classic single-relay outbox designs and architectures where several modeled consumers independently consume the same durable logical message collection.

Acknowledgement is consumer-relative.

One input acknowledging a message does not imply that another input has acknowledged it.

---

# 67. Outbox versus Topic

Outbox and Topic remain structurally related but semantically distinct.

| Concern | Topic | Outbox |
|---|---|---|
| Layer | L0 logical channel | L0 `DataModel` entity |
| Payload | typed message | typed message |
| Allowed schemas | `messages` | `messages` |
| Logical message identity | `message_identity` | `message_identity` |
| Producer effect | `PublicationEffect` | `OutboxWriteEffect` |
| Producer execution | ordinary effect execution | transaction-exclusive |
| Atomic with DataModel mutation | not implied | yes, through containing transaction |
| Consumer input | `SubscriptionInput` | `OutboxInput` |
| Runtime | topic/subscription runtime | `OutboxRuntime` |

The similarity is intentional.

The difference in transactional placement is fundamental.

---

# 68. Outbox versus EffectIntent

Outbox messages SHALL NOT be `EffectIntent`s.

An `EffectIntent` remains:

> a captured logical effect instance established for later `ExecuteEffectIntent` within the existing operation/program artifact semantics.

An outbox message is:

> durable typed application message data admitted to a `DataModel` outbox.

Thus:

```text
Outbox message
    !=
EffectIntent
```

An Outbox consumer operation may subsequently:

```text
receive message
    ->
establish EffectIntent
    ->
execute EffectIntent
```

if its application program requires that structure.

Or it may directly execute an ordinary effect.

The Outbox does not predetermine which downstream effect the message causes.

---

# 69. Outbox versus TransactionOutput

An outbox message is also not a `TransactionOutput`.

A transaction output exports typed data back into the same enclosing operation's subsequent logical control.

An outbox write creates durable asynchronous work visible to independent Outbox consumers.

Thus:

```text
TransactionOutput
    transaction -> same operation continuation

Outbox message
    transaction -> durable Outbox -> independent operation invocation
```

The two may coexist in the same transaction.

---

# 70. Canonical transactional outbox example

Consider:

```yaml
data_models:
  data.orders:
    objects:
      object.order:
        schema: schema.Order
        identity:
          - order_id

    outboxes:
      outbox.order_events:
        messages:
          - schema.OrderCreated

        message_identity:
          kind: keyed
          mapping:
            schema.OrderCreated:
              - event_id
```

Producer:

```yaml
operations:
  op.create_order:
    inputs:
      input.request:
        kind: request
        schema: schema.CreateOrderRequest
        ...

    requirements:
      idempotency:
        - key:
            - source: input:input.request
              path: request_id

    program:
      steps:
        - kind: transaction
          id: tx.create_order
          data_model: data.orders
          ...

          steps:
            - kind: insert
              object: object.order
              ...

            - kind: write_outbox
              effect_id: effect.outbox_order_created

              effect:
                outbox: outbox.order_events
                schema: schema.OrderCreated

                idempotency_key_propagation:
                  - source:
                      - source: input:input.request
                        path: request_id
                    target:
                      - event_id

              values:
                kind: deterministic
                from:
                  - source: input:input.request
                    path: request_id
                  - source: input:input.request
                    path: order_id

        - kind: return
          ...
```

The transaction establishes atomically:

```text
Order mutation
+
OrderCreated outbox message
```

---

# 71. Canonical Outbox consumer example

```yaml
operations:
  op.publish_order_event:
    inputs:
      input.outbox:
        kind: outbox
        outbox: outbox.order_events
        messages:
          kind: only
          schemas:
            - schema.OrderCreated
        acknowledge_on_success: true

    requirements:
      idempotency:
        - key:
            - source: input:input.outbox
              path: event_id

    program:
      steps:
        - kind: execute_effect
          effect_id: effect.publish_order_created

          effect:
            kind: publication
            topic: topic.order_events
            schema: schema.OrderCreated

            idempotency_key_propagation:
              - source:
                  - source: input:input.outbox
                    path: event_id
                target:
                  - event_id

          values:
            kind: deterministic
            from:
              - source: input:input.outbox

        - kind: complete
```

The logical cascade is:

```text
CreateOrder
    |
    v
OutboxWriteEffect
    |
    v
OrderCreated outbox message
    |
    v
PublishOrderEvent
    |
    v
PublicationEffect
    |
    v
Topic
```

The outbox does not break idempotency lineage.

---

# 72. Canonical Outbox runtime example

```yaml
runtime:
  outboxes:
    op.publish_order_event:
      input.outbox:

        delivery:
          kind: at_least_once

        partitioning:
          kind: keyed
          mapping:
            schema.OrderCreated:
              - tenant_id

        ordering:
          kind: partition

        dispatch:
          pool: pool.outbox_workers

          member_assignment:
            kind: consistent_hash

          batching:
            ordering:
              kind: preserved
```

and:

```yaml
execution_pools:
  pool.outbox_workers:
    member_concurrency:
      kind: bounded
      value: 8
```

The model establishes:

```text
equal tenant_id
    =>
same logical outbox partition
```

and:

```text
same partition
    =>
partition-relative message precedence
```

plus:

```text
same partition
    =>
same active assigned worker member
during a stable ownership epoch
```

and:

```text
batch execution
    =>
preserves the established partition order
```

subject to the precise limits of those declarations.

---

# 73. Example idempotency lineage

For the preceding example:

```text
CreateOrder.request_id
        |
        | OutboxWriteEffect propagation
        v
OrderCreated.event_id
        |
        | Outbox.message_identity
        v
logical outbox message
        |
        | OutboxInput
        v
PublishOrderEvent.event_id
        |
        | consumer IdempotencyRequirement
        v
duplicate consumer work collapsed
        |
        | PublicationEffect propagation
        v
Topic.OrderCreated.event_id
        |
        v
further subscribers...
```

This is one continuous lineage/proof graph.

No outbox-specific idempotency-key type is introduced.

---

# 74. Example producer retry analysis

Suppose two attempts of `CreateOrder(request_id=R)` reach the producer transaction.

### Route A — transaction commit deduplication

If:

```text
tx.create_order
    DeduplicatedBy(request_id)
```

and the key is valid for the retry class, then:

```text
attempt 1 -> Commit(tx,R)
attempt 2 -> resolves Commit(tx,R)
```

so the second encounter does not commit another outbox write.

### Route B — repeated committed writes

If the transaction may commit again, the analyzer may instead establish:

```text
outbox write payload replay-stable
+
event_id identifies logical message
+
every modeled Outbox consumer collapses duplicate deliveries
```

and discharge the duplicate outbox side effect transitively.

The two proof routes remain distinct.

---

# 75. Acknowledgement does not close the idempotency proof

For an at-least-once Outbox runtime:

```text
delivery M
    |
execute downstream effect successfully
    |
crash / uncertainty before logical successful completion
    |
no acknowledgement
    |
M may be delivered again
```

Therefore:

```text
acknowledge_on_success = true
```

does not eliminate the need for consumer idempotency where duplicate attempts are admitted.

This is deliberately analogous to subscription processing.

---

# 76. Required analyzer graph changes

The analyzer SHALL add the edge family:

```text
Operation
    ->
OutboxWriteEffect
    ->
Outbox
    ->
OutboxInput
    ->
Operation
```

alongside existing families such as:

```text
Operation
    ->
PublicationEffect
    ->
Topic
    ->
SubscriptionInput
    ->
Operation
```

and:

```text
Operation
    ->
RequestEffect
    ->
RequestInput
    ->
Operation
```

The graph SHALL support cycles involving any combination of these edges.

---

# 77. Idempotency solver changes

The idempotency solver SHALL:

1. enumerate `OutboxWriteEffect` occurrences in the producing operation's reachable paths;
2. determine whether the containing transaction suppresses duplicate commits where applicable;
3. otherwise test class-fixity of the written outbox message;
4. resolve the destination Outbox's message identity;
5. enumerate every modeled `OutboxInput` admitting the schema;
6. inspect the consumer's delivery semantics;
7. inspect any consumer idempotency requirement keyed from that input;
8. follow the consumer's transitive effects;
9. include these dependencies in the existing greatest-fixpoint computation;
10. emit exact proof evidence identifying producer effect site, Outbox, consumer input, runtime delivery fact, and downstream requirement used.

There SHALL NOT be a separate outbox-only notion of idempotency.

---

# 78. Ordering verifier changes

The ordering verifier SHALL admit Outbox runtime order as an L1 precedence source for an OutboxInput-triggered operation.

For partition ordering, evidence should identify:

```text
OutboxRuntime partition mapping
OutboxOrdering::Partition
OutboxDispatch member assignment
batch ordering preservation if batching exists
ExecutionPool facts where relevant
```

The verifier must retain the distinction between:

```text
partition affinity
ordering
serialization
```

and must not infer one from another.

---

# 79. Serialization verifier changes

Keyed Outbox partitioning plus member assignment may establish same-key member affinity.

It does not automatically establish no-overlap.

Where batching is absent, ordinary ExecutionPool concurrency facts may participate in serialization reasoning as appropriate.

Where batching exists and its internal overlap is intentionally opaque, the verifier SHALL NOT use batching order preservation as a serialization guarantee.

If this prevents a desired proof, a future batch-execution concurrency semantic may be introduced explicitly.

---

# 80. Value-lineage changes

The value-lineage system SHALL treat the concrete message created by `WriteOutboxEffect` as a typed effect payload for purposes such as:

```text
idempotency propagation
proof evidence
visualization
```

The `values` derivation remains the source of complete instance provenance.

No special outbox-only provenance language is introduced.

---

# 81. Structural validation — Outbox

Validation SHALL reject:

- unknown message schemas;
- malformed message-identity mappings;
- identity mappings referencing schemas not admitted by the Outbox;
- invalid or incompatible identity tuple field paths/types according to the same rules used for Topic message identity.

Outbox IDs remain subject to normal global ID uniqueness.

---

# 82. Structural validation — OutboxWriteEffect

Validation SHALL require:

1. the destination Outbox exists;
2. the destination Outbox belongs to the transaction's declared `data_model`;
3. the transaction's `data_model` is not null;
4. the declared schema exists;
5. the destination Outbox admits that schema;
6. the complete `values` derivation is structurally valid in the transaction context;
7. propagation sources resolve in the execution context;
8. propagation targets resolve against the written message schema;
9. `effect_id` is globally unique according to existing execution-site ID rules.

An Outbox belonging to another `DataModel` SHALL be rejected.

Conseqa SHALL NOT infer a distributed cross-data-model atomic transaction.

---

# 83. Structural validation — execution-site restrictions

Validation SHALL reject an `OutboxWriteEffect` appearing under:

```text
execute_effect
establish_effect_intent
transition side effect
```

in the initial revision.

Likewise:

```text
execute_effect_intent
```

can never resolve to an outbox write because no `EffectIntent` may contain this effect kind initially.

---

# 84. Structural validation — OutboxInput

Validation SHALL require:

1. the referenced Outbox exists;
2. every explicitly selected schema is admitted by that Outbox;
3. `acknowledge_on_success` is explicitly declared;
4. ordinary input ID and operation ownership rules hold.

An OutboxInput has no synchronous request result.

Its normal terminal is `complete`.

---

# 85. Structural validation — OutboxRuntime

An `OutboxRuntime` SHALL identify:

```text
existing operation
existing input
input.kind == outbox
```

Its dispatch pool SHALL reference an existing `ExecutionPool`.

Its partition mapping SHALL be valid for every message schema admitted through the target input where keyed partitioning is declared.

`OutboxOrdering::Partition` SHALL require keyed partitioning.

Its `MemberAssignment` SHALL satisfy the ordinary assignment validation rules.

---

# 86. Runtime batching validation

If:

```text
dispatch.batching
```

is absent, no batching fact is declared.

If present, its ordering-preservation field must contain a valid explicit value.

No defaults SHALL silently state ordering preservation.

Quantitative values such as:

```text
batch_size
max_wait
```

are not part of the initial Conseqa L1 declaration.

---

# 87. Runtime model shape

Conceptually, the hierarchical runtime model may gain:

```rust
pub struct RuntimeModel {
    pub topics: ...,
    pub subscriptions: ...,
    pub outboxes:
        BTreeMap<Id, BTreeMap<Id, OutboxRuntime>>,
    pub execution_pools: ...,
    pub routers: ...,
    pub storage_layouts: ...,
}
```

where the nested keys identify:

```text
operation
input
```

as with subscription runtime targeting.

Exact container layout is an implementation choice so long as one runtime declaration resolves unambiguously to one L0 `OutboxInput`.

---

# 88. Alignment with SubscriptionRuntime

As a companion cleanup, the intended structures become approximately:

```rust
pub struct SubscriptionRuntime {
    pub delivery: DeliverySemantics,
    pub grouping: SubscriptionGrouping,
    pub ordering: SubscriptionOrdering,
    pub dispatch: SubscriptionDispatch,
}

pub struct OutboxRuntime {
    pub delivery: DeliverySemantics,
    pub partitioning: OutboxPartitioning,
    pub ordering: OutboxOrdering,
    pub dispatch: OutboxDispatch,
}
```

and:

```rust
pub struct SubscriptionDispatch {
    pub pool: Id,
    pub routing: Option<SubscriptionRouting>,
    pub batching: Option<BatchingSemantics>,
}

pub struct OutboxDispatch {
    pub pool: Id,
    pub member_assignment: MemberAssignment,
    pub batching: Option<BatchingSemantics>,
}
```

The exact subscription surface should follow the separately agreed grouping/ordering refactor.

The important alignment is semantic:

```text
source-domain formation
    ->
ordering guarantee
    ->
dispatch/member assignment
    ->
ExecutionPool
```

without introducing a public generic superclass.

---

# 89. Why Outbox partitioning remains distinct from Subscription grouping

These declarations may induce analogous equivalence relations:

```text
equal subscription grouping key
    -> same subscription group

equal outbox partition key
    -> same outbox partition
```

but they describe different architecture concepts.

Conseqa SHALL therefore retain:

```text
SubscriptionGrouping
OutboxPartitioning
```

rather than exposing:

```text
RuntimeDomainKey
```

as a semantic abstraction.

Internal verifier utilities may share implementation.

The public semantic contract must retain the domain-specific concepts.

---

# 90. Why OutboxOrdering remains distinct

Likewise:

```text
SubscriptionOrdering
OutboxOrdering
```

remain distinct enums.

For Outboxes:

```text
None
Global
Partition
```

is clearer than forcing terminology such as:

```text
WithinDomain
```

onto authors.

Equivalent proof machinery is not sufficient reason to erase semantic vocabulary.

---

# 91. No priority semantics

The initial `OutboxRuntime` SHALL contain no priority declaration.

In particular:

```text
priority
priority_key
priority_order
```

are deferred.

Introducing priority would require defining its interaction with:

```text
hard ordering constraints
eligibility
starvation
mutable scheduling state
cross-partition selection
```

No such complexity is required for the initial outbox abstraction.

---

# 92. No polling semantics

The model SHALL use:

```text
OutboxDispatch
```

rather than:

```text
OutboxPolling
```

or a poller-specific primitive.

A conforming realization may use:

```text
database polling
CDC
notifications
stream cursors
blocking reads
broker-backed dispatch
```

without changing the Conseqa model.

---

# 93. No explicit acknowledgment program primitive

This revision SHALL NOT introduce:

```text
Acknowledge(item)
Acknowledge(batch)
```

into `OperationBlock`.

The program remains deliberately opaque with respect to consumer implementation details.

The only application-level fact required initially is:

```text
acknowledge_on_success
```

on source-driven inputs.

This retains crash/redelivery semantics without requiring loops, batch iteration, concurrency constructs, or explicit acknowledgement control flow.

---

# 94. No L0 batch input

Likewise, the revision SHALL NOT introduce:

```text
BatchSubscriptionInput
BatchOutboxInput
```

as different L0 invocation shapes.

Batching is an L1 execution realization over multiple logical per-item invocations.

This preserves the existing simple operation-program model.

---

# 95. Simulation boundary

The new semantics provide useful qualitative structure to downstream simulation.

Conseqa may supply:

```text
outbox message-production graph
producer transaction boundary
message schemas
message identity
consumer graph
delivery multiplicity
partition key
ordering mode
member assignment
ExecutionPool
batching presence
batch ordering-preservation guarantee
```

An external simulation scenario may independently supply:

```text
message arrival rates
partition-key frequency distribution
number of runtime partitions
ExecutionPool member count
service-time distributions
outbox storage latency
delivery latency
batch-size distribution
batch wait policy
batch-internal parallelism
failure probabilities
redelivery timing
```

This preserves Conseqa L1 as qualitative topology rather than turning it into a queue simulator.

---

# 96. Latency-analysis interpretation

The simulation layer may treat an Outbox boundary as an asynchronous queueing boundary:

```text
producer transaction latency
        |
        v
outbox admission
        |
        +---- queue residence time
        |
        v
dispatch
        |
        v
consumer operation latency
```

Where batching exists, simulation may supply implementation-specific quantitative batch behavior while Conseqa supplies only the fact that batching exists and whether established ordering must survive it.

---

# 97. Infrastructure realization interpretation

An infrastructure realization layer may lower:

```text
Outbox
OutboxRuntime
partitioning
delivery
ordering
OutboxDispatch
ExecutionPool
```

into concrete infrastructure obligations.

Possible realizations include:

```text
Postgres outbox table + worker
DynamoDB outbox + stream processor
database CDC + relay
durable queue-backed implementation
```

Conseqa does not select these merely from the abstract declaration unless an external realizer profile provides the mapping.

---

# 98. Required changes to the semantic contract

The authoritative semantics document will need updates in at least these areas:

1. `Model` / `DataModel` — introduce Outboxes.
2. Topic/message identity discussion — define equivalent Outbox message identity.
3. `Operation` / `Input` — introduce `OutboxInput`.
4. Subscription acknowledgement — add `acknowledge_on_success`.
5. Idempotency requirements — include Outbox edges in the transitive cascade.
6. `ValueSource::effect` — admit OutboxWrite payloads.
7. `IdempotencyKeyPropagation` — extend producer/consumer lineage to Outboxes.
8. `Effect` — broaden the general definition.
9. Effects — introduce `OutboxWriteEffect`.
10. Transaction steps — introduce transaction-exclusive `write_outbox`.
11. Transaction atomicity — state that committed OutboxWriteEffects participate in the same atomic application transaction.
12. Program validation — enforce OutboxWrite execution-site restrictions.
13. Idempotency solver — add duplicate-outbox-write discharge and Outbox consumer traversal.
14. Ordering solver — admit Outbox runtime precedence.
15. Hierarchical runtime model — introduce `OutboxRuntime`.
16. Execution/dispatch semantics — integrate OutboxDispatch.
17. Simulation interpretation — expose Outbox topology.

---

# 99. Suggested Rust surface additions

Illustratively:

```rust
pub struct DataModel {
    pub objects: BTreeMap<Id, DataObject>,
    pub outboxes: BTreeMap<Id, Outbox>,
}

pub struct Outbox {
    pub messages: BTreeSet<Id>,
    pub message_identity: MessageIdentity,
}

pub struct OutboxRef {
    pub data_model: Id,
    pub outbox: Id,
}

pub struct OutboxWriteEffect {
    pub outbox: OutboxRef,
    pub schema: Id,
    pub idempotency_key_propagation:
        Vec<IdempotencyKeyPropagation>,
}

pub struct WriteOutboxEffect {
    pub effect_id: Id,
    pub effect: OutboxWriteEffect,
    pub values: Derivation,
}

pub struct OutboxInput {
    pub outbox: OutboxRef,
    pub messages: MessageSelector,
    pub acknowledge_on_success: bool,
}

pub struct OutboxRuntime {
    pub delivery: DeliverySemantics,
    pub partitioning: OutboxPartitioning,
    pub ordering: OutboxOrdering,
    pub dispatch: OutboxDispatch,
}

pub enum OutboxPartitioning {
    None,
    Keyed(OutboxPartitionKey),
}

pub struct OutboxPartitionKey {
    pub mapping: BTreeMap<Id, Vec<FieldPath>>,
}

pub enum OutboxOrdering {
    None,
    Global,
    Partition,
}

pub struct OutboxDispatch {
    pub pool: Id,
    pub member_assignment: MemberAssignment,
    pub batching: Option<BatchingSemantics>,
}

pub struct BatchingSemantics {
    pub ordering: BatchOrderingPreservation,
}

pub enum BatchOrderingPreservation {
    Preserved,
    Unspecified,
}
```

Exact Rust representation may differ where existing project types make a more natural implementation.

The normative semantics above take precedence over illustrative struct shape.

---

# 100. Required additions to existing enums

Conceptually:

```rust
pub enum Input {
    Request(RequestInput),
    Subscription(SubscriptionInput),
    Outbox(OutboxInput),
}
```

```rust
pub enum Effect {
    Publication(PublicationEffect),
    Request(RequestEffect),
    External(ExternalEffect),
    OutboxWrite(OutboxWriteEffect),
}
```

```rust
pub enum TransactionStep {
    ...
    WriteOutboxEffect(WriteOutboxEffect),
}
```

However, validators must preserve the rule that `OutboxWriteEffect` has exactly one legal execution context.

---

# 101. Diagnostics

The implementation SHOULD provide specific diagnostics rather than collapsing all failures into generic invalid references.

Useful diagnostics include:

```text
UnknownOutbox
OutboxSchemaNotAdmitted
OutboxBelongsToDifferentDataModel
OutboxWriteOutsideTransaction
OutboxWriteEffectCannotBeIntent
InvalidOutboxMessageIdentity
IncompleteOutboxPartitionMapping
InvalidOutboxPartitionOrdering
UnknownOutboxRuntimeInput
InvalidOutboxRuntimeInputKind
OutboxDispatchUnknownPool
```

Names may follow existing diagnostic conventions.

---

# 102. Visualization

Conseqa visualization SHOULD render Outbox edges distinctly from Topic publication.

For example:

```text
Operation A
   |
   | transaction
   v
[OutboxWrite]
   |
   v
Outbox O
   |
   v
Operation B
```

The graph SHOULD make it possible to inspect:

```text
transaction atomic boundary
message schema
message identity
idempotency propagation
OutboxInput consumer edges
partitioning
ordering
dispatch pool
```

This is especially valuable when displaying an operation's side-effect blast radius.

---

# 103. Compatibility and migration

Existing models without Outboxes remain semantically unchanged.

No existing `EffectIntent` should be mechanically converted to an Outbox.

No existing `PublicationEffect` should be mechanically converted to an Outbox write.

No DataObject named `outbox` should acquire special meaning.

Outbox semantics exist only through the explicit new `Outbox` entity and `OutboxWriteEffect`.

For subscriptions, adding explicit `acknowledge_on_success` may require migration if the field is made mandatory.

Migration tooling should not guess the value where the existing model provides no equivalent fact.

---

# 104. Key semantic invariants

The implementation SHALL preserve all of the following:

1. **Outbox is L0 application data structure.**
2. **OutboxRuntime is L1.**
3. **Outbox contains typed messages, not EffectIntents.**
4. **OutboxWriteEffect is a real Effect.**
5. **OutboxWriteEffect executes only inside a transaction.**
6. **Its admission is atomic with that transaction's commit.**
7. **Outbox writes participate in the producing operation's effect blast radius.**
8. **Outbox message identity has the same logical-message meaning as Topic message identity.**
9. **Idempotency propagation is lineage only.**
10. **Outbox idempotency uses the existing transitive solver model.**
11. **OutboxInput is an L0 single-message logical invocation source.**
12. **Acknowledgement is an L0 on-success input semantic, not an explicit program step.**
13. **Batching is an L1 realization over logical single-message invocations.**
14. **Batch internals remain opaque.**
15. **Partitioning is the sole Outbox grouping concept.**
16. **Partitioning does not imply ordering.**
17. **OutboxOrdering is distinct from SubscriptionOrdering.**
18. **OutboxDispatch and SubscriptionDispatch remain distinct public concepts.**
19. **Both reuse the same MemberAssignment semantics.**
20. **General execution concurrency remains with ExecutionPool.**
21. **No priority semantics exist initially.**
22. **No polling mechanism is prescribed.**
23. **No exactly-once effect execution follows from Outbox use.**
24. **Outbox runtime ordering does not invent upstream business causality.**
25. **An Outbox boundary does not terminate transitive idempotency analysis.**

---

# 105. Acceptance criteria

The revision is complete when Conseqa can represent and correctly analyze the following architecture without implementation-specific constructs:

```text
Request R
    |
    v
Operation A
    |
    | IdempotencyRequirement(K)
    v
Transaction T
    |
    +-- mutate DataObject
    |
    +-- OutboxWriteEffect M
             |
             | propagation K -> message identity
             v
         Outbox O
             |
             | at-least-once runtime delivery
             | keyed partitioning
             | partition ordering
             | member assignment
             | optional order-preserving batching
             v
         OutboxInput
             |
             v
         Operation B
             |
             | IdempotencyRequirement(message identity)
             v
         PublicationEffect
             |
             v
           Topic
             |
             v
       SubscriptionInput
             |
             v
         Operation C
```

and can:

- prove that `M` is atomically committed with `T`;
- include `M` and all downstream consumers in `A`'s side-effect cascade;
- trace `K` through `M` using existing idempotency propagation semantics;
- use Outbox message identity to anchor the logical message;
- analyze duplicate OutboxWriteEffect execution;
- analyze duplicate Outbox delivery;
- follow B's idempotency requirement transitively;
- continue through B's Topic publication into C;
- use Outbox partition/order/dispatch facts in ordering proofs;
- refuse to infer serialization merely from partitioning or batch order preservation;
- model acknowledgement at successful per-message invocation completion;
- remain silent about physical polling, physical batch execution strategy, priority, or exactly-once processing.

---

# 106. Normative summary

> An L0 `Outbox` is a typed logical message collection belonging to a `DataModel`. It declares the message schemas it admits and may declare logical message identity using the same `MessageIdentity` semantics used by Topics.

> An `OutboxWriteEffect` is a first-class message-producing Effect that may execute only inside a transaction operating on the Outbox's owning `DataModel`. Its concrete message payload is constructed from the transaction-local derivation at its execution site. The resulting logical outbox message becomes durable if and only if the containing transaction commits.

> Because `OutboxWriteEffect` is an Effect, it participates in the producing operation's complete transitive side-effect blast radius and in operation idempotency verification.

> `OutboxWriteEffect.idempotency_key_propagation` has exactly the existing lineage meaning: it records that fields in the emitted outbox message carry upstream logical idempotency identity. It does not itself deduplicate the outbox write or downstream processing.

> `Outbox.message_identity` identifies when repeated writes represent the same logical message. Where repeated committed writes are possible, duplicate-write safety follows the same fundamental pattern as duplicate Topic publication: the message instance must be class-fixed under the governing retry class, repeated writes must therefore denote the same logical message, and every modeled downstream Outbox consumer must collapse any resulting duplicate work. A containing `Transaction::DeduplicatedBy` may alternatively prevent a second committed write.

> An L0 `OutboxInput` declares that one logical message admitted to an Outbox may invoke an operation. The operation program remains a per-message logical program and does not expose runtime batches.

> Source-item acknowledgement is an L0 application semantic declared as `acknowledge_on_success` on the consuming input. Successful logical completion acknowledges that individual source item for that consumer. No explicit acknowledgement program primitive is introduced.

> An L1 `OutboxRuntime` declares delivery multiplicity, Outbox partitioning, Outbox ordering, and Outbox dispatch for one L0 OutboxInput. Outbox partitioning is also the Outbox's grouping relation; no separate Outbox grouping primitive exists.

> `OutboxOrdering` remains a domain-specific enum with `None`, `Global`, and `Partition`. `OutboxDispatch` remains distinct from `SubscriptionDispatch` but reuses the existing `MemberAssignment` and `ExecutionPool` semantics.

> Runtime batching may combine multiple logical per-message evaluations as an opaque realization. Conseqa does not model iteration, parallelism, wait groups, or batch acknowledgement mechanics. L1 records only whether that batching stage preserves an already-established ordering guarantee.

> Outbox semantics prescribe neither polling nor physical storage representation, priority policy, exact worker topology, exactly-once external execution, nor eventual completion.