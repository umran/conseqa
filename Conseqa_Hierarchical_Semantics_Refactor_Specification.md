# Conseqa Hierarchical Semantics Refactor

**Status:** Proposed  
**Baseline:** Conseqa `main` at `2fb9bca`  
**Scope:** DSL semantics, serialized surface, validation, verification, proof provenance, runtime topology, and external-analysis interfaces

---

# 1. Summary

Conseqa SHALL adopt a hierarchical semantic model with two initial layers:

- **L0 — Abstract Application Semantics**
- **L1 — Runtime Topology and Realization Semantics**

L0 defines the abstract application machine: logical structure, invocation relationships, programs, transactions, effects, behavioral guarantees, and correctness requirements.

L1 defines selected runtime facts describing how that machine is realized: message transport ordering and delivery, request routing, subscription dispatch, execution-resource topology, execution concurrency, and physical storage partitioning.

The governing architectural rule is:

> **L0 says what application machine exists and what properties it requires. L1 says how invocations and persistent data are arranged in a particular runtime realization of that machine.**

The analyzer MAY combine L0 and L1 facts to prove L0 requirements.

Any proof relying on L1 facts SHALL remain explicitly conditional on that runtime realization.

---

# 2. Design goals

The refactor SHALL provide:

1. **A lean L0 surface.**  
   Users uninterested in runtime topology can model only application behavior.

2. **Progressive semantic depth.**  
   Runtime topology is introduced only when the desired analysis requires it.

3. **One home for runtime concurrency.**  
   Runtime concurrency SHALL be modeled exclusively by `ExecutionPool.member_concurrency`.

4. **Separation of routing from execution.**  
   `Router` and `SubscriptionDispatch` determine which execution-pool member owns an invocation domain.  
   `ExecutionPool` determines the concurrency characteristics of the members executing those invocations.

5. **Minimal routing semantics.**  
   Routing SHALL consist only of:
   - a semantic routing key; and
   - a member-assignment rule.

6. **Request/subscription symmetry.**  
   Request routing and subscription dispatch remain distinct public primitives but use the same basic routing model and both terminate at `ExecutionPool`.

7. **Whole-architecture reasoning.**  
   L1 facts may discharge L0 obligations without becoming intrinsic L0 guarantees.

8. **External quantitative analysis.**  
   L1 exposes qualitative topology while workload, pool cardinality, capacity, and latency remain external simulation concerns.

---

# Part I — Semantic hierarchy

## 3. L0 — Abstract Application Semantics

L0 describes application execution over an idealized abstract machine.

It includes at minimum:

```text
Services

Schemas
FieldPaths

DataModels
DataObjects
DataObject identity

StateMachines

Topics as logical channels
Topic message membership
Message identity

Operations

RequestInputs
SubscriptionInputs

Operation programs

Transactions
Transaction atomicity
Transaction isolation
Explicit locks
Unique claims
Reads / writes / inserts / deletes
State transitions
Transaction outputs
Effect intents
Transaction DeduplicatedBy(K)

Publication effects
Request effects
RequestEffect.retry
External effects
Effect idempotency guarantees

Serialization requirements
Ordering requirements
Idempotency requirements
Result-replay requirements
Recoverability requirements
```

L0 SHALL remain meaningful and structurally valid in the complete absence of L1.

---

## 4. L0 is not merely a correctness layer

A declaration does not move to L1 merely because infrastructure implements it.

For example:

```text
transaction isolation = serializable
```

remains L0.

Conseqa treats serializable transaction execution as an abstract primitive. It need not expose MVCC, SSI, database lock managers, or other implementation mechanisms.

The same principle applies to:

```text
explicit locks

Transaction DeduplicatedBy(K)

RequestEffect.retry

effect idempotency

message identity

request identity

unique claims
```

These describe the behavior of Conseqa's abstract application machine.

Their implementation mechanisms lie below Conseqa's semantic floor.

---

## 5. L1 — Runtime Topology and Realization Semantics

The initial L1 vocabulary SHALL consist of:

```text
TopicRuntime

SubscriptionRuntime
SubscriptionDispatch

Router

ExecutionPool

StorageLayout
```

There SHALL initially be:

```text
no OperationRuntime

no operation-level concurrency

no logical execution-lane abstraction

no Router lane concurrency

no SubscriptionDispatch lane concurrency
```

All runtime execution concurrency SHALL be declared through:

```text
ExecutionPool.member_concurrency
```

---

## 6. Semantic layer and semantic category are independent

Conseqa's existing classification of declarations as:

```text
structural facts

implementation guarantees / assumptions

requirements / obligations
```

SHALL remain independent of semantic layer.

Examples:

| Declaration | Layer | Category |
|---|---|---|
| `Operation` | L0 | structural |
| `SubscriptionInput` | L0 | structural |
| transaction isolation | L0 | guarantee |
| `RequestEffect.retry` | L0 | guarantee |
| transaction `DeduplicatedBy(K)` | L0 | guarantee |
| `SerializedBy(K)` | L0 | requirement |
| `OrderedBy(K)` | L0 | requirement |
| topic runtime ordering | L1 | guarantee |
| subscription delivery | L1 | guarantee |
| routing/member assignment | L1 | guarantee |
| execution-pool identity | L1 | structural |
| member concurrency | L1 | guarantee |
| storage partition layout | L1 | structural/runtime fact |

Correctness relevance does not determine semantic layer.

---

# Part II — L0 refactor

## 7. Logical Topic

`Topic` SHALL remain L0 but lose its runtime-ordering field.

Conceptually:

```rust
pub struct Topic {
    pub messages: BTreeSet<Id>,
    pub message_identity: MessageIdentity,
}
```

`Topic.messages` remains L0 because it defines the logical message channel.

`Topic.message_identity` remains L0 because it defines logical message identity.

The existing:

```text
Topic.ordering
```

moves to L1 `TopicRuntime`.

---

## 8. SubscriptionInput

`SubscriptionInput` SHALL become purely logical:

```rust
pub struct SubscriptionInput {
    pub topic: Id,
    pub messages: MessageSelector,
}
```

It means:

> A logical message admitted through this subscription may invoke this operation.

The following current facts SHALL move out of L0:

```text
delivery

dispatch routing

lane concurrency
```

Lane concurrency is removed entirely rather than relocated.

---

## 9. RequestInput

`RequestInput` remains L0.

It defines a logical request boundary and its payload/result/identity contracts.

The logical request boundary referenced by L1 is the existing pair:

```text
(operation_id, input_id)
```

No new L0 `InvocationBoundary` primitive is required.

---

## 10. Operation

`Operation` remains the L0 logical executable unit.

Conceptually:

```rust
pub struct Operation {
    pub service: Id,
    pub description: Option<String>,
    pub inputs: BTreeMap<Id, Input>,
    pub program: OperationBlock,
    pub requirements: OperationRequirements,
}
```

The existing operation execution-concurrency declaration SHALL be removed.

There SHALL initially be no replacement `OperationRuntime`.

Runtime concurrency is an execution-resource property.

---

## 11. Service

`Service` SHALL NOT participate in L1 runtime topology.

No inference may be made that:

```text
same service
    => same execution pool

different service
    => different execution pool

same service
    => same process

same service
    => same deployment

same service
    => same runtime member

different service
    => physical execution isolation
```

Same-service operations may target different execution pools.

Different-service operations may target the same execution pool.

Until `Service` acquires stronger normative semantics, it remains independent of runtime placement.

---

# Part III — L1 model

## 12. Runtime root

The L0 collections SHOULD remain directly on the model root so L0-only documents remain concise.

Conceptually:

```rust
pub struct Model {
    pub revision: Revision,

    // L0
    pub services: BTreeMap<Id, Service>,
    pub schemas: BTreeMap<Id, Schema>,
    pub data_models: BTreeMap<Id, DataModel>,
    pub topics: BTreeMap<Id, Topic>,
    pub state_machines: BTreeMap<Id, StateMachine>,
    pub operations: BTreeMap<Id, Operation>,

    // L1
    pub runtime: Option<RuntimeModel>,
}
```

and:

```rust
pub struct RuntimeModel {
    pub topics: BTreeMap<Id, TopicRuntime>,

    pub subscriptions:
        BTreeMap<Id, BTreeMap<Id, SubscriptionRuntime>>,

    pub execution_pools:
        BTreeMap<Id, ExecutionPool>,

    pub routers:
        BTreeMap<Id, Router>,

    pub storage_layouts:
        BTreeMap<Id, StorageLayout>,
}
```

L1 SHALL be optional.

---

# Part IV — Execution topology

## 13. ExecutionPool

`ExecutionPool` is the central L1 execution-resource primitive.

It identifies:

> **A logical population of interchangeable runtime members capable of executing operation invocations assigned to that pool.**

Conceptually:

```rust
pub struct ExecutionPool {
    pub member_concurrency: MemberConcurrency,
}
```

An execution pool establishes:

1. runtime population identity; and
2. qualitative execution concurrency for each member of that population.

---

## 14. Pool identity

If two execution paths target the same `ExecutionPool`, they share one logical runtime execution population.

For example:

```text
Router GetMessages
        \
         -> DataWorkers

SubscriptionDispatch RefreshCache
        /
```

means request-driven and subscription-driven work share `DataWorkers`.

This fact is meaningful even when Conseqa does not declare pool cardinality.

---

## 15. Distinct pools

If:

```text
A -> Pool X
B -> Pool Y
```

where:

```text
X != Y
```

then A and B target distinct logical runtime populations.

This does NOT imply:

```text
different physical hosts

different processes

different Kubernetes deployments

different availability zones

different physical failure domains
```

Those remain outside the initial L1 abstraction.

---

## 16. Member concurrency

`MemberConcurrency` SHALL initially support:

```rust
enum MemberConcurrency {
    Unspecified,
    Unbounded,
    Bounded(NonZeroUsize),
}
```

### `unspecified`

No usable fact exists about simultaneous execution on one pool member.

### `unbounded`

No finite member-level execution bound may be assumed.

### `bounded(n)`

At most `n` operation invocations assigned to one member of the pool may simultaneously be active.

### `bounded(1)`

One pool member executes at most one operation invocation at a time.

The bound applies across all invocations assigned to that member, irrespective of operation or ingress mechanism.

Unlike routing, member concurrency has genuine semantic value in distinguishing unknown from an explicitly unconstrained execution resource. It therefore retains its explicit `unspecified` and `unbounded` states.

---

## 17. Member concurrency is the only runtime concurrency primitive

The hierarchical model SHALL NOT contain:

```text
OperationRuntime.concurrency

Router.lane_concurrency

SubscriptionDispatch.lane_concurrency

ExecutionLane.concurrency
```

All L1 execution-concurrency reasoning SHALL consume:

```text
ExecutionPool.member_concurrency
```

Routing determines **where work executes**.

ExecutionPool determines **how much work may execute concurrently there**.

---

## 18. Pool cardinality is external

`ExecutionPool` SHALL NOT initially contain:

```text
member count

replica count

CPU

memory

autoscaling rules

host count

container count
```

An external scenario may independently instantiate:

```text
DataWorkers.members = 16
```

or:

```text
DataWorkers.members = 128
```

against the same Conseqa architecture.

---

# Part V — Minimal routing model

## 19. Routing has exactly two semantic components

Routing SHALL NOT be represented as an enum containing cases such as:

```text
Unspecified

Unconstrained

SingleMember

ByKey
```

These concepts are not equivalent semantic alternatives.

The routing model SHALL instead consist of exactly:

```text
semantic routing key

member assignment
```

Conceptually:

```text
invocation
    |
evaluate semantic routing key
    v
routing domain
    |
member assignment
    v
ExecutionPool member
```

The routing key determines which invocations belong to the same routing domain.

The member assignment determines which execution-pool member owns that routing domain.

---

## 20. Routing keys are semantic, not physical

A routing key SHALL be interpreted as a **semantic value derived from an L0 invocation**.

Its purpose is to define routing-domain identity.

For invocations `A` and `B`:

```text
routing_key(A) = routing_key(B)
    =>
routing_domain(A) = routing_domain(B)
```

A routing key is NOT:

```text
an ExecutionPool member ID

a worker ID

a process ID

a physical shard ID

a storage partition ID

a host ID
```

The routing key therefore remains stable as the physical/runtime realization changes.

For example:

```text
routing key:
    account_id = 42
```

may initially be assigned to:

```text
ExecutionPool member 17
```

and later reassigned to:

```text
ExecutionPool member 31
```

without changing either:

```text
account_id = 42
```

or the identity of its routing domain.

The semantic relationship is:

```text
L0 invocation data
        |
        v
semantic routing key
        |
        v
routing domain
        |
        | MemberAssignment
        v
L1 execution-pool member
```

`MemberAssignment`, not the routing key, performs the mapping onto the runtime execution topology.

---

## 21. Routing keys remain distinct from physical storage partition keys

A routing key and `StorageLayout.partition_key` MAY reference the same underlying fields, but they have different meanings.

For example:

```text
Router routing key:
    account_id

StorageLayout partition key:
    account_id
```

does not make them the same semantic concept.

The Router key defines:

```text
execution affinity domain
```

while the storage-layout key defines:

```text
physical storage partition identity
```

They may also deliberately differ:

```text
Router routing key:
    channel_id

StorageLayout partition key:
    (channel_id, bucket)
```

This means all work for one channel may share an execution-affinity domain while the channel's data remains distributed across multiple physical storage partitions.

Conseqa SHALL NOT infer equivalence between routing-domain identity and storage-partition identity merely because their key expressions happen to coincide.

---

## 22. Absence of routing means no routing fact

Routing SHALL be optional.

For example:

```rust
pub struct Router {
    pub boundary: OperationInputRef,
    pub pool: Id,
    pub routing: Option<RequestRouting>,
}
```

If:

```text
routing = None
```

the only available fact is:

> Invocations of this boundary execute within the referenced pool.

The analyzer SHALL NOT infer any stable member-affinity relationship.

There SHALL be no serialized:

```text
routing: unspecified
```

variant.

Absence already expresses epistemic absence.

---

## 23. No `Unconstrained` routing variant

The initial model SHALL NOT contain an explicit `Unconstrained` routing variant.

If no routing declaration exists, the analyzer has no usable routing-affinity fact.

That is sufficient for the initial proof model.

A future explicit negative routing guarantee MAY be introduced only if an analyzer requires a semantic distinction between:

```text
unknown routing behavior
```

and:

```text
known arbitrary routing behavior
```

No such distinction is required by this refactor.

---

## 24. No `SingleMember` routing variant

The initial model SHALL NOT contain a special:

```text
SingleMember
```

routing kind.

"All invocations happen to map to one member" is an outcome of an ordinary routing domain and assignment rule, not a fundamentally different category of routing.

If Conseqa later needs to express a single routing domain for all invocations, this SHOULD be represented through an ordinary key expression capable of producing one constant domain rather than through a special routing variant.

The DSL SHALL avoid promoting representable ordinary cases into top-level semantic variants.

---

# Part VI — Request routing

## 25. Router

`Router` SHALL remain a standalone first-class L1 primitive.

It describes how invocations entering through one existing L0 `RequestInput` are assigned into a target `ExecutionPool`.

Conceptually:

```rust
pub struct Router {
    pub boundary: OperationInputRef,
    pub pool: Id,
    pub routing: Option<RequestRouting>,
}
```

where:

```rust
pub struct OperationInputRef {
    pub operation: Id,
    pub input: Id,
}
```

The referenced input MUST be an L0 request input.

---

## 26. RequestRouting

`RequestRouting` SHALL be a simple product type rather than an enum:

```rust
pub struct RequestRouting {
    pub key: Vec<FieldPath>,
    pub member_assignment: MemberAssignment,
}
```

Its key MUST be non-empty.

The key is evaluated against the request input schema.

The resulting value is a semantic routing key as defined in §20.

---

## 27. Request routing-key semantics

For a request invocation `I`:

```text
routing_key(I) = tuple(field_1, ..., field_n)
```

Two invocations for the same Router belong to the same routing domain iff their evaluated routing-key tuples are equal.

Therefore:

```text
key(A) = key(B)
    =>
routing_domain(A) = routing_domain(B)
```

The key does NOT itself identify or select a physical execution member.

That is exclusively the responsibility of `member_assignment`.

---

## 28. MemberAssignment

The previously proposed `PlacementStrategy` SHALL be renamed to:

```text
MemberAssignment
```

because it describes:

> How a routing domain is assigned to a member of an `ExecutionPool`.

This terminology is preferable to "placement" because the semantic relation is specifically:

```text
semantic routing domain
    ->
runtime execution-pool member
```

and because assignment naturally accommodates ownership and handoff semantics.

Conceptually:

```rust
pub enum MemberAssignment {
    ConsistentHash,
    // future assignment mechanisms
}
```

The enum may grow only when materially different member-assignment guarantees are required.

---

## 29. `consistent_hash`

For:

```text
member_assignment: consistent_hash
```

the runtime guarantees:

> Equal routing domains are owned by the same execution-pool member during a stable ownership epoch.

Different routing domains MAY be assigned to the same member.

Conseqa does not prescribe:

```text
Ketama

rendezvous hashing

hash function

virtual-node count

membership discovery implementation
```

The declaration specifies semantic assignment behavior rather than implementation mechanics.

---

## 30. Member assignment includes safe ownership transfer

Any `MemberAssignment` used to establish keyed serialization MUST preserve exclusive ownership through reassignment.

Suppose routing domain `K` moves from member A to member B.

A conforming runtime SHALL NOT permit:

```text
A executing K
        ||
B executing K
```

in a manner that violates the declared one-owner routing semantics.

The transfer must ensure that ownership moves safely.

Possible implementation mechanisms include:

```text
draining

leases

generation fencing

coordinated handoff

partition ownership protocols
```

Conseqa does not model these mechanisms.

It models the resulting assignment guarantee.

---

## 31. Router responsibility

A Router answers:

> **Which member of the target execution pool owns the semantic routing domain for this request invocation?**

It does not choose which operation executes.

The L0 request boundary already determines that.

Conceptually:

```text
L0:

RequestInput
    |
    v
Operation


L1:

RequestInput
    |
    v
Router
    |
evaluate semantic routing key
    |
    v
routing domain
    |
MemberAssignment
    |
    v
ExecutionPool member
    |
    v
execute the L0 Operation
```

---

## 32. Router without routing

A Router MAY contain only:

```text
boundary
pool
```

with no routing declaration.

For example:

```yaml
routers:
  router.health:

    boundary:
      operation: op.health
      input: input.request

    pool: pool.web
```

This means:

> Requests through this boundary execute within `pool.web`.

It provides no member-affinity fact.

This is the sole initial representation of unspecified member routing.

---

# Part VII — Subscription transport and dispatch

## 33. SubscriptionRuntime

`SubscriptionRuntime` describes runtime behavior associated with an existing L0 subscription.

Conceptually:

```rust
pub struct SubscriptionRuntime {
    pub delivery: DeliverySemantics,
    pub dispatch: SubscriptionDispatch,
}
```

The declaration targets:

```text
(operation_id, input_id)
```

where that input MUST be an L0 `SubscriptionInput`.

---

## 34. Delivery semantics

Existing delivery meanings SHALL remain unchanged.

### `unspecified`

No usable duplicate/loss fact exists.

### `at_most_once`

The same logical message is delivered no more than once.

Loss may occur.

This does not imply exactly-once execution.

### `at_least_once`

One successfully published logical message may produce repeated delivery attempts.

Duplicate invocation must therefore be considered possible.

No retry-count, retry-timing, or eventual-success guarantee is implied.

---

## 35. SubscriptionDispatch

`SubscriptionDispatch` defines where subscription deliveries execute.

Conceptually:

```rust
pub struct SubscriptionDispatch {
    pub pool: Id,
    pub routing: Option<SubscriptionRouting>,
}
```

There is no lane abstraction and no lane-concurrency field.

The target pool MUST exist.

---

## 36. SubscriptionRouting

`SubscriptionRouting` SHALL also be a simple product type:

```rust
pub struct SubscriptionRouting {
    pub key: SubscriptionRoutingKey,
    pub member_assignment: MemberAssignment,
}
```

The initial key vocabulary SHOULD be:

```rust
pub enum SubscriptionRoutingKey {
    TopicKey,
}
```

A future extension MAY permit explicit message-field keys if a concrete use case requires them.

No routing-state variants such as:

```text
Unspecified
Unconstrained
SingleMember
ByTopicKey
```

are required.

The subscription routing key is semantic in the same sense as a request routing key: it defines an invocation-equivalence domain, not a physical execution identifier.

---

## 37. `topic_key`

For:

```text
key: topic_key
```

the subscription reuses the semantic keyed domain established by the associated topic runtime.

Conceptually:

```text
TopicKey(message A) = TopicKey(message B)
    =>
routing_domain(A) = routing_domain(B)
```

The associated `member_assignment` then maps that semantic routing domain onto an execution-pool member.

In the initial model, `topic_key` therefore requires an available compatible keyed `TopicRuntime.ordering` declaration.

A future refactor MAY separate topic-key identity from topic ordering if Conseqa needs keyed routing without a transport-ordering guarantee.

---

## 38. Subscription dispatch uses the same assignment abstraction

Subscription dispatch SHALL use the same `MemberAssignment` abstraction as request routing.

Conceptually:

```text
message delivery
    |
evaluate semantic SubscriptionRoutingKey
    v
routing domain
    |
MemberAssignment
    v
ExecutionPool member
```

This gives request and subscription execution the same basic topology:

```text
semantic routing domain
    ->
runtime member assignment
    ->
execution-pool member
```

while preserving distinct public request and subscription constructs.

---

## 39. Subscription routing without routing

A subscription dispatch MAY declare only:

```text
pool
```

with no routing block.

For example:

```yaml
dispatch:
  pool: pool.workers
```

This means:

> Deliveries execute within `pool.workers`.

No stable relationship between message keys and pool members may be inferred.

There is no explicit `Unspecified` or `Unconstrained` routing state.

---

## 40. Safe ownership transfer applies equally to subscriptions

When subscription routing declares a key and member assignment, reassignment MUST preserve the same ownership semantics required of Router.

If routing domain `K` moves from member A to B, member ownership must transfer in a way compatible with all guarantees derived from that assignment.

A rebalance cannot silently invalidate:

```text
same semantic routing domain
    ->
one current execution-member ownership domain
```

while the runtime continues to claim conformance to the declared routing semantics.

---

# Part VIII — Topic runtime

## 41. TopicRuntime

The existing topic-ordering semantics SHALL move into L1:

```rust
pub struct TopicRuntime {
    pub ordering: TopicOrdering,
}
```

The existing meanings of:

```text
unspecified
unordered
global
keyed
```

SHALL remain unchanged.

For keyed ordering, the existing schema-to-field mapping establishes the logical topic-key domain.

---

## 42. Topic ordering remains distinct from routing

Topic ordering answers:

> What transport precedence exists among logical messages?

Subscription routing answers:

> Which messages belong to the same semantic routing domain and which execution member currently owns that domain?

ExecutionPool answers:

> How many invocations may that member execute simultaneously?

These facts SHALL remain separate.

---

## 43. Dispatch must preserve established transport precedence

When a subscription uses:

```text
key: topic_key
```

and the topic runtime provides keyed ordering, dispatch SHALL preserve the topic's established same-key precedence when admitting invocations to execution.

This includes failure-driven redelivery and ownership reassignment.

A conforming runtime SHALL NOT:

1. establish delivery A before B for the same ordered key;
2. leave A semantically incomplete;
3. admit B in a manner that permits B to overtake A contrary to the declared ordering guarantee.

This order-preservation responsibility replaces the semantics previously associated with subscription logical lanes.

---

# Part IX — Storage topology

## 44. StorageLayout

`StorageLayout` SHALL describe physical partition identity for an L0 `DataObject`.

Conceptually:

```rust
pub struct StorageLayout {
    pub object: DataObjectRef,
    pub partition_key: Vec<FieldPath>,
}
```

where:

```rust
pub struct DataObjectRef {
    pub data_model: Id,
    pub object: Id,
}
```

Example:

```text
StorageLayout Message:
    partition_key = (channel_id, bucket)
```

means:

> Physical storage partition identity for a Message instance is determined by `(channel_id, bucket)`.

---

## 45. Object identity and storage identity are distinct

The following is valid:

```text
DataObject identity:
    message_id

StorageLayout partition identity:
    (channel_id, bucket)
```

Object identity answers:

> Which logical object instance is this?

Storage layout answers:

> Into which physical partition is that instance mapped?

The analyzer SHALL NOT substitute one identity for the other.

---

## 46. StorageLayout non-guarantees

`StorageLayout` SHALL NOT imply:

```text
database vendor

database node count

replication factor

replica placement

consistency level

partition capacity

latency

availability
```

These remain external analysis/deployment concerns.

---

# Part X — Correctness reasoning

## 47. Serialization requirement remains L0

The L0 serialization requirement continues to mean:

> Invocations sharing the required logical key must not execute concurrently.

Different keys may execute concurrently.

Serialization establishes non-overlap.

It does not establish an execution order.

---

## 48. Request-side serialization proof

A request-side runtime proof MAY establish `SerializedBy(K)` when the analyzer proves all of:

```text
1. Router.routing.key is semantically equivalent to K.

2. Equal routing-key tuples therefore belong
   to the same semantic routing domain.

3. Router.member_assignment guarantees one
   active member ownership domain for that
   routing domain, including safe handoff.

4. The target ExecutionPool has:
       member_concurrency = bounded(1).
```

Therefore:

```text
equal K
    ->
same semantic routing domain
    ->
same active pool member
    ->
member concurrency 1
    ->
no overlap
```

The resulting proof is runtime-dependent.

---

## 49. Subscription-side serialization proof

A subscription-side runtime proof MAY establish `SerializedBy(K)` when:

```text
1. SubscriptionDispatch.routing.key
   is semantically equivalent to K.

2. Equal K values therefore belong
   to the same semantic routing domain.

3. member_assignment guarantees one
   active member ownership domain.

4. The target ExecutionPool has:
       member_concurrency = bounded(1).
```

For the common topic-key case:

```text
same topic key
    ->
same semantic routing domain
    ->
same active pool member
    ->
member concurrency 1
    ->
no overlap
```

The resulting proof is runtime-dependent.

---

## 50. No special single-member serialization rule

There SHALL be no separate proof rule based on a routing mode named `SingleMember`.

If a model needs every invocation to belong to one routing domain, that fact should arise from an ordinary semantic routing-key definition rather than a special semantic case.

Serialization reasoning therefore always follows the same structure:

```text
semantic key equivalence
    ->
routing-domain equivalence
    ->
member ownership
    ->
member concurrency
```

This keeps the proof model uniform.

---

## 51. Routing absence proves nothing about member affinity

If:

```text
routing = None
```

the analyzer may know that the invocation executes within a particular pool.

It SHALL NOT infer:

```text
same key -> same member

different key -> different member

one member only

round robin

random routing
```

No member-affinity proof is available.

---

## 52. Shared ExecutionPool does not establish shared routing domains

Suppose:

```text
Router A -> Pool X

Router B -> Pool X
```

Even if A and B use semantically equal-looking keys, Conseqa SHALL NOT infer that their routing domains share ownership.

Likewise:

```text
Router A -> Pool X

SubscriptionDispatch B -> Pool X
```

does not imply cross-boundary member affinity.

Pool identity establishes a shared execution population.

Routing-domain identity establishes semantic affinity.

`MemberAssignment` maps that affinity onto runtime member ownership.

These are distinct facts.

---

## 53. Removal of operation-global concurrency

The old operation-level concurrency fact is intentionally removed.

Conseqa SHALL no longer permit:

```text
Operation X:
    concurrency = 1
```

as a runtime assertion detached from the execution topology that realizes concurrency.

If serialization follows from runtime execution topology, the model should expose the relevant routing and execution-pool facts.

If serialization follows from L0 locks or transactions, those proof routes remain available.

A future explicit global execution-gate primitive may be introduced if a genuine architectural need appears.

It SHALL NOT be hidden inside `Operation`.

---

## 54. Ordering remains stronger than serialization

An L0 ordering requirement continues to mean:

> Where meaningful same-key precedence exists, semantically relevant execution must preserve that precedence.

A serialization proof alone is insufficient.

---

## 55. Subscription ordering proof

The common runtime proof becomes:

```text
TopicRuntime establishes keyed precedence K
        +
SubscriptionDispatch semantic routing key = topic_key
        +
member assignment preserves keyed ownership
and established precedence
        +
ExecutionPool.member_concurrency = 1
        |
        v
ordered serial execution for K
```

Therefore:

```text
TopicRuntime keyed ordering K
+
SubscriptionRouting key = topic_key
+
safe precedence-preserving member assignment
+
ExecutionPool member concurrency 1
+
required-key identity
-----------------------------------
OrderedBy(K)
```

---

## 56. Router does not invent request ordering

For request inputs:

```text
Router semantic key K
+
member assignment
+
ExecutionPool member concurrency 1
```

may establish serialization.

It does NOT establish:

```text
OrderedBy(K)
```

because routing does not itself define meaningful precedence among independent requests.

A separate precedence source would be required.

---

# Part XI — Other existing requirements

## 57. Idempotency

Idempotency requirements remain L0.

Relevant L0 facts continue to include:

```text
RequestEffect.retry

request identity

message identity

Transaction DeduplicatedBy(K)

effect idempotency

program replay semantics
```

Subscription delivery belongs to L1.

Therefore any proof relying on:

```text
delivery = at_most_once
```

is runtime-dependent.

---

## 58. Recoverability

Recoverability requirements remain L0.

Relevant L0 mechanisms continue to include:

```text
RequestEffect.retry = may_repeat

transaction replay

recoverable effect intents

Transaction DeduplicatedBy(K)
```

L1 may additionally provide retry opportunity through:

```text
subscription delivery = at_least_once
```

A proof relying on this fact is runtime-dependent.

Neither `may_repeat` nor `at_least_once` implies eventual success.

---

# Part XII — Proof provenance

## 59. Whole-model reasoning

The analyzer SHALL have access to all declared semantic layers.

L1 does not exist merely for performance analysis.

The analyzer may prove an L0 requirement from:

```text
L0 facts
+
L1 runtime facts
```

provided every inference is valid under the declared semantics.

---

## 60. Proof scope

Every successful proof SHOULD record at minimum:

```rust
enum ProofScope {
    L0Only,
    RuntimeDependent,
}
```

### `L0Only`

No explicit L1 fact was required.

### `RuntimeDependent`

At least one L1 fact was necessary.

---

## 61. Exact evidence provenance

Preferably, proof evidence SHALL identify the declarations consumed.

Example:

```text
Requirement:
    SerializedBy(account_id)

Verdict:
    Proven

Scope:
    RuntimeDependent

Evidence:

    Router update_account_router
        semantic routing key = account_id
        member_assignment = consistent_hash

    ExecutionPool account_workers
        member_concurrency = bounded(1)
```

This allows topology-dependent proofs to be invalidated when the runtime architecture changes.

---

## 62. L0-only does not mean implementation-free

A proof based on:

```text
transaction isolation = serializable
```

is L0-only even though the concrete database must actually implement serializable execution.

Proof scope identifies dependency on semantic layers.

It does not remove implementation-conformance assumptions.

---

# Part XIII — Validation

## 63. TopicRuntime validation

A `TopicRuntime` MUST target an existing L0 `Topic`.

Keyed-ordering mappings MUST satisfy the existing schema/path/key-domain validation rules.

---

## 64. SubscriptionRuntime validation

A subscription runtime entry MUST identify:

```text
existing operation

existing input

input.kind == subscription
```

Its dispatch pool MUST reference an existing `ExecutionPool`.

If `dispatch.routing` exists:

```text
routing.key
routing.member_assignment
```

are both required.

`key: topic_key` requires a compatible topic-key domain.

---

## 65. Router validation

A Router MUST identify:

```text
existing operation

existing input

input.kind == request

existing target ExecutionPool
```

If `routing` exists:

```text
routing.key must be non-empty

every key field must resolve
against the request schema

member_assignment must be present
```

One request boundary SHALL have at most one Router in the initial model.

---

## 66. MemberAssignment validation

A `MemberAssignment` value MUST be valid for the routing context in which it appears.

Assignment-specific structural requirements MAY be imposed by individual assignment strategies.

No assignment strategy may silently imply a routing key that is absent from the routing declaration.

---

## 67. ExecutionPool validation

`ExecutionPool` IDs MUST be unique.

For:

```text
member_concurrency = bounded(n)
```

`n` MUST be greater than zero.

---

## 68. StorageLayout validation

A storage layout MUST identify:

```text
existing DataModel

existing DataObject
```

Its partition key MUST:

```text
be non-empty

resolve against the object's schema
```

V1 SHOULD permit at most one primary `StorageLayout` per object.

---

# Part XIV — Canonical surface

## 69. L0-only example

```yaml
revision: 1

topics:
  topic.order_events:

    messages:
      - schema.OrderCreated

    message_identity:
      kind: keyed
      mapping:
        schema.OrderCreated:
          - event_id


operations:
  op.process_order:

    service: service.orders

    inputs:
      input.events:

        kind: subscription
        topic: topic.order_events

        messages:
          kind: all


    requirements:

      serialization:
        - key:
            source: input:input.events
            path: order_id

      ordering: []
      idempotency: []
      recoverability: []


    program:
      ...
```

This is a complete Conseqa model.

No runtime topology is required.

---

## 70. Runtime-enriched example

```yaml
runtime:

  topics:

    topic.order_events:

      ordering:

        kind: keyed

        mapping:
          schema.OrderCreated: order_id


  execution_pools:

    pool.order_workers:

      member_concurrency:
        kind: bounded
        value: 1


    pool.message_reads:

      member_concurrency:
        kind: bounded
        value: 32


  subscriptions:

    op.process_order:

      input.events:

        delivery: at_least_once

        dispatch:

          pool: pool.order_workers

          routing:

            key: topic_key

            member_assignment:
              kind: consistent_hash


  routers:

    router.get_messages:

      boundary:
        operation: op.get_messages
        input: input.request

      pool: pool.message_reads

      routing:

        key:
          - channel_id

        member_assignment:
          kind: consistent_hash


  storage_layouts:

    layout.messages:

      object:
        data_model: data.chat
        object: object.message

      partition_key:
        - channel_id
        - bucket
```

Here:

```text
channel_id
```

is a semantic routing key defining execution-affinity domains.

It is not an execution-pool member identifier or physical storage partition identifier.

`consistent_hash` maps those semantic routing domains onto the current members of `pool.message_reads`.

---

## 71. Routing omitted example

A request boundary may be assigned to a pool without declaring member affinity:

```yaml
routers:

  router.health:

    boundary:
      operation: op.health
      input: input.request

    pool: pool.web
```

Likewise a subscription may declare:

```yaml
dispatch:
  pool: pool.workers
```

In either case, the analyzer knows only the target execution population.

---

# Part XV — Migration from `2fb9bca`

## 72. Directly relocatable declarations

The following migrations remain structural:

| `2fb9bca` | New model |
|---|---|
| `Topic.messages` | L0 unchanged |
| `Topic.message_identity` | L0 unchanged |
| `Topic.ordering` | `runtime.topics[topic].ordering` |
| `SubscriptionInput.topic` | L0 unchanged |
| `SubscriptionInput.messages` | L0 unchanged |
| `SubscriptionInput.delivery` | `SubscriptionRuntime.delivery` |
| transaction semantics | L0 unchanged |
| `RequestEffect.retry` | L0 unchanged |
| operation requirements | L0 unchanged |

---

## 73. Existing `by_topic_key` migration

Existing:

```text
dispatch.routing = by_topic_key
```

contains enough information to recover the new semantic routing key:

```text
routing.key = topic_key
```

but it does NOT contain enough information to determine:

```text
routing.member_assignment
```

or:

```text
dispatch.pool
```

Those are new L1 architectural facts.

Therefore `by_topic_key` cannot be converted into a complete canonical `SubscriptionRouting` without additional migration input.

---

## 74. Existing `single_lane` migration

Existing:

```text
dispatch.routing = single_lane
```

SHALL NOT be mechanically translated into a special `SingleMember` routing mode because no such primitive exists.

Nor SHALL migration invent an artificial routing key or constant routing domain.

A `single_lane` declaration therefore requires architectural review.

If the desired new architecture genuinely places all subscription invocations under one semantic routing domain, that should be expressed using the ordinary routing-key and member-assignment machinery supported by the resulting DSL.

---

## 75. Existing lane concurrency cannot be mechanically migrated

Existing:

```text
SubscriptionInput.dispatch.lane_concurrency
```

SHALL be removed.

It MUST NOT be automatically converted into:

```text
ExecutionPool.member_concurrency
```

because the scopes differ.

For example:

```text
old:
    concurrency 1 within each subscription lane
```

does not imply:

```text
new:
    one pool member executes at most one invocation
    across every workload assigned to it
```

The new statement is generally stronger.

Migration tooling MUST require an explicit architectural decision or leave pool concurrency unspecified.

---

## 76. Existing operation concurrency cannot be mechanically migrated

Existing operation-global concurrency SHALL also be removed.

It MUST NOT be converted into member concurrency.

The old:

```text
operation X concurrency = 1
```

means:

> No two invocations of X overlap globally.

The new:

```text
pool member concurrency = 1
```

means:

> One member executes no more than one invocation at once.

A multi-member pool can still execute many X invocations concurrently.

No sound mechanical translation exists.

---

## 77. Pool assignment is new information

The `2fb9bca` model contains no `ExecutionPool` identity.

Migration tooling MUST NOT infer:

```text
same service -> same pool

same operation -> same pool

one subscription -> one implicit unique pool

all operations -> one shared pool
```

unless an explicit migration policy is supplied.

Pool topology is new architectural information.

---

## 78. MemberAssignment is new information

Likewise, existing subscription dispatch declarations do not in general specify the algorithm or ownership semantics by which semantic routing domains are assigned to execution members.

Migration tooling MUST NOT invent:

```text
member_assignment: consistent_hash
```

merely because an existing dispatch had keyed affinity.

Member assignment is an explicit new L1 fact.

---

# Part XVI — External analysis boundary

## 79. Conseqa L1 remains qualitative

Conseqa L1 SHALL describe semantic topology.

An external simulation scenario MAY provide:

```text
ExecutionPool member counts

traffic rates

message rates

key-frequency distributions

service-time distributions

storage-node counts

replication factors

capacity

queueing

latency

failure probabilities
```

---

## 80. Example external simulation

Conseqa:

```text
Router GetMessages:
    semantic routing key = channel_id
    member_assignment = consistent_hash
    pool = MessageReads

ExecutionPool MessageReads:
    member_concurrency = 32

StorageLayout Message:
    partition_key = (channel_id, bucket)
```

External scenario:

```text
MessageReads.members = 64

GetMessages.rate = 250000/sec

channel_id ~ Zipf(...)

database_nodes = 96

replication_factor = 3
```

A simulator may then evaluate:

```text
hot execution members

hot storage partitions

routing skew

shared-pool contention

pool scaling

routing-key alternatives

partition-key alternatives

queue growth

latency

request amplification
```

without those quantitative values becoming Conseqa semantics.

The simulator may change:

```text
channel_id domain X -> member 17
```

into:

```text
channel_id domain X -> member 31
```

as pool membership changes without altering the semantic identity of routing domain X.

---

# Part XVII — Non-goals

## 81. Not modeled in this refactor

The refactor SHALL NOT attempt to model:

```text
physical database nodes

replica topology

consensus protocols

database lock-manager internals

hosts

containers

process IDs

CPU

memory

availability zones

network links

queue capacities

request rates

traffic distributions

latency SLOs

autoscaling policies
```

It SHALL also NOT introduce:

```text
PlacedInvocation

ExecutionLane

SingleMember routing

Unconstrained routing

explicit Unspecified routing
```

Routing keys SHALL NOT be repurposed as physical worker, member, shard, host, or storage-partition identifiers.

---

# Part XVIII — Acceptance criteria

## 82. Required outcomes

The refactor is complete when all of the following hold:

1. L0-only models remain valid and analyzable.

2. `Topic.ordering` moves from L0 to `TopicRuntime`.

3. Subscription delivery moves to L1.

4. Subscription dispatch moves to L1.

5. Subscription lane concurrency is removed.

6. Operation execution concurrency is removed.

7. No `OperationRuntime` is required by the initial L1 model.

8. `ExecutionPool.member_concurrency` is the only L1 execution-concurrency declaration.

9. `Router` remains a standalone first-class L1 primitive.

10. Every Router targets an `ExecutionPool`.

11. Every `SubscriptionDispatch` targets an `ExecutionPool`.

12. Routing is optional.

13. Absence of routing means no usable member-affinity fact.

14. There is no explicit `Unspecified` routing variant.

15. There is no initial `Unconstrained` routing variant.

16. There is no `SingleMember` routing variant.

17. Request routing consists only of a semantic key and a member assignment.

18. Subscription routing consists only of a semantic key and a member assignment.

19. Routing keys are semantic values derived from L0 invocation data.

20. Routing keys define semantic routing-domain equivalence.

21. Routing keys do not identify physical workers, execution-pool members, shards, hosts, or storage partitions.

22. `MemberAssignment` maps semantic routing domains onto execution-pool members.

23. `PlacementStrategy` is replaced by `MemberAssignment`.

24. Member assignment used for correctness proofs has normative safe-ownership-transfer semantics.

25. Reassignment of a semantic routing domain does not change the identity of that routing domain.

26. `ExecutionPool.member_concurrency = 1` combined with compatible routing may prove keyed serialization.

27. Member concurrency greater than one does not prove keyed serialization.

28. Shared pool identity alone does not prove shared member affinity.

29. Different routing declarations targeting the same pool do not implicitly share routing domains.

30. Routing-domain identity remains distinct from `StorageLayout` physical partition identity.

31. Equal routing and storage key expressions do not cause their semantic identities to collapse.

32. Topic transport ordering remains distinct from member routing and execution concurrency.

33. Subscription dispatch preserves applicable established topic precedence.

34. Topic ordering + compatible routing/member assignment + member concurrency one may prove compatible ordering requirements.

35. Request routing + member concurrency one does not invent request ordering.

36. Transaction isolation remains L0.

37. Explicit locks remain L0.

38. `RequestEffect.retry` remains L0.

39. Transaction `DeduplicatedBy(K)` remains L0.

40. Message/request identity remains L0.

41. `StorageLayout` maps a logical object to physical partition-key fields without modifying logical object identity.

42. `Service` has no implicit execution-topology meaning.

43. Same-service operations may use different pools.

44. Different-service operations may share a pool.

45. Removing L1 may make requirements unproven but does not structurally invalidate otherwise valid L0.

46. Proofs consuming L1 evidence are explicitly runtime-dependent.

47. Migration tooling never converts old lane concurrency or operation concurrency into member concurrency automatically.

48. Migration tooling never invents execution-pool topology.

49. Migration tooling never invents `MemberAssignment`.

50. External tools can deserialize and analyze L1 independently of the Rust verifier.

---

# 83. Normative architectural principle

The hierarchical model SHALL be governed by the following rule:

> **L0 defines the abstract application machine. L1 defines selected aspects of its runtime realization. A routing key is a semantic value derived from an L0 invocation and defines which invocations belong to the same routing domain. It is not a physical member, worker, shard, host, or storage-partition identifier. `MemberAssignment` maps semantic routing domains onto the current execution-pool topology. `ExecutionPool` is the sole owner of runtime execution concurrency. `TopicRuntime` determines transport ordering. `StorageLayout` determines physical data partitioning. The analyzer may reason across all declared layers, but every proof remains conditional on the facts and realization assumptions from which it was derived.**

The intended semantic progression is:

```text
L0
    abstract application behavior
    transactions
    effects
    correctness requirements

        +

L1
    transport semantics
    semantic routing key
    routing-domain identity
    member assignment
    execution topology
    execution concurrency
    storage topology

        +

external scenario
    cardinality
    workload
    capacity
    latency
    simulation
```

The routing model deliberately follows one additional design rule:

> **If a case can be represented as an ordinary value of a more fundamental semantic dimension, it SHALL NOT be promoted into a special routing variant.**

This keeps the L1 surface small and prevents semantic taxonomy from growing faster than the actual architecture being modeled.