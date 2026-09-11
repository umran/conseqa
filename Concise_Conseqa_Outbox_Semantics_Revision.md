# Conseqa Outbox Semantics — Concise Revision

## 1. L0 `Outbox`

Unchanged:

```rust
pub struct Outbox {
    pub messages: BTreeSet<Id>,
    pub message_identity: MessageIdentity,
}
```

An Outbox remains a typed durable message collection owned by a `DataModel`.

---

## 2. L0 `OutboxInput`

### Current

```rust
pub struct OutboxInput {
    pub outbox: Id,
    pub messages: MessageSelector,
    pub acknowledge_on_success: bool,
}
```

### Revised

```rust
pub struct OutboxInput {
    pub outbox: Id,
}
```

### Changes

```diff
 OutboxInput {
     outbox: Id,
-    messages: MessageSelector,
-    acknowledge_on_success: bool,
 }
```

Add the structural invariant:

> Exactly one `OutboxInput` in the model SHALL reference a given Outbox.

That input's owning Operation is the Outbox's exclusive logical consumer.

Consequently:

- the owning operation consumes every message schema admitted by the Outbox;
- multiple competing logical consumer operations are not permitted;
- downstream fan-out belongs to Topics rather than Outboxes.

An Outbox admitting heterogeneous message schemas remains valid. The owning operation must be capable of handling the admitted input variants.

---

## 3. Intrinsic consumption semantics

Remove configurable acknowledgement/delivery semantics from the Outbox abstraction.

Instead define intrinsically:

```text
committed message M
    ->
M becomes durably pending

while M is pending
    ->
the runtime continues to admit consumption attempts for M

successful logical completion of an attempt
    ->
M becomes consumed

failed or uncertain attempt
    ->
M remains pending
```

`Consumed(M)` means no further ordinary consumption attempts need be admitted.

It does **not** imply that previously admitted attempts have terminated or been cancelled.

Therefore the following is explicitly permitted:

```text
attempt A(M) starts
        |
        | timeout / lease expiry / uncertainty
        v
attempt B(M) starts

A(M) || B(M)
```

Multiple simultaneously active attempts may therefore exist for the same logical message.

This is why consumer idempotency remains necessary.

---

## 4. L1 `OutboxRuntime`

### Current

```rust
pub struct OutboxRuntime {
    pub delivery: DeliverySemantics,
    pub partitioning: OutboxPartitioning,
    pub ordering: OutboxOrdering,
    pub dispatch: OutboxDispatch,
}
```

### Revised

```rust
pub struct OutboxRuntime {
    pub partitioning: OutboxPartitioning,
    pub ordering: OutboxOrdering,
    pub dispatch: OutboxDispatch,
}
```

### Change

```diff
 OutboxRuntime {
-    delivery: DeliverySemantics,
     partitioning: OutboxPartitioning,
     ordering: OutboxOrdering,
     dispatch: OutboxDispatch,
 }
```

`delivery` is removed because durable re-drive until successful consumption is now intrinsic to the Outbox abstraction.

L1 describes only additional runtime organization of those consumption attempts.

---

## 5. `OutboxDispatch`

### Current

```rust
pub struct OutboxDispatch {
    pub pool: Id,
    pub member_assignment: MemberAssignment,
    pub batching: Option<BatchingSemantics>,
}
```

### Revised

```rust
pub struct OutboxDispatch {
    pub pool: Id,
    pub routing: Option<OutboxRouting>,
    pub batching: Option<BatchingSemantics>,
}
```

### Change

```diff
 OutboxDispatch {
     pool: Id,
-    member_assignment: MemberAssignment,
+    routing: Option<OutboxRouting>,
     batching: Option<BatchingSemantics>,
 }
```

Batching remains a dispatch semantic.

---

## 6. `OutboxRouting`

Add:

```rust
pub struct OutboxRouting {
    pub key: OutboxRoutingKey,
    pub member_assignment: MemberAssignment,
}

pub enum OutboxRoutingKey {
    PartitionKey,
}
```

This intentionally mirrors:

```rust
SubscriptionRouting {
    key,
    member_assignment,
}
```

rather than burying the routing domain implicitly inside `member_assignment`.

The declaration states two independent facts:

```text
key
    = which established semantic domain is routed

member_assignment
    = how that domain is assigned to ExecutionPool members
```

For V1 the only valid Outbox routing domain is:

```text
OutboxRoutingKey::PartitionKey
```

because `OutboxPartitioning` establishes the semantic consumption partition.

---

## 7. Routing semantics

When:

```text
routing = None
```

Conseqa establishes only:

> Consumption attempts execute on some member of the referenced `ExecutionPool`.

No stable partition-to-member affinity is known.

When:

```text
routing = Some {
    key: PartitionKey,
    member_assignment: A,
}
```

Conseqa establishes:

> The logical partition domain established by `OutboxRuntime.partitioning` is routed to ExecutionPool members according to `A`.

Routing does not imply attempt exclusivity.

For example, after redelivery or ownership uncertainty:

```text
attempt A(M) -> member X
attempt B(M) -> member Y
```

may overlap unless stronger routing/handoff semantics establish otherwise.

---

## 8. Routing validation

`OutboxRoutingKey::PartitionKey` SHALL require:

```text
OutboxPartitioning::Keyed(...)
```

It is invalid with:

```text
OutboxPartitioning::None
```

because no partition-key domain exists to route.

This mirrors the principle that routing consumes an already-declared semantic key rather than inventing one.

---

## 9. Partitioning

Unchanged conceptually:

```rust
pub enum OutboxPartitioning {
    None,
    Keyed(OutboxPartitionKey),
}
```

Partitioning establishes logical runtime grouping only.

It does not imply:

```text
ordering
serialization
member affinity
exclusive execution
```

---

## 10. Ordering

Unchanged conceptually:

```rust
pub enum OutboxOrdering {
    None,
    Global,
    Partition,
}
```

`Partition` requires keyed partitioning.

Ordering establishes source-message precedence.

It does not by itself establish:

```text
non-overlapping attempts
completion order
effect order
serialization
```

In particular, redelivery of the **same** logical message may overlap a stale prior attempt.

---

## 11. Batching

Remain on `OutboxDispatch`:

```rust
pub struct OutboxDispatch {
    pub pool: Id,
    pub routing: Option<OutboxRouting>,
    pub batching: Option<BatchingSemantics>,
}
```

Batching describes how several logical source-item invocations cross the source-to-execution boundary.

It does **not** describe whether the underlying database or transport happened to fetch several rows/records together.

For:

```text
batching.ordering = Preserved
```

the dispatch stage preserves any already-established Outbox ordering relation.

It does not establish serialization among batch members.

---

## 12. Execution concurrency

No Outbox-specific concurrency field is introduced.

Runtime invocation concurrency remains solely:

```rust
ExecutionPool.member_concurrency
```

Thus one Outbox has:

```text
one logical consumer Operation
```

but may be executed by:

```text
many ExecutionPool members
```

with bounded, unbounded, or otherwise declared member concurrency.

Logical consumer ownership and runtime worker membership are separate concepts.

---

## 13. Revised semantic shape

```text
L0
────────────────────────────────

Transaction
    |
    | atomic OutboxWriteEffect
    v
Outbox
    |
    | exactly one owning OutboxInput
    v
Consumer Operation


Intrinsic Outbox consumption
────────────────────────────────

committed
    -> durably pending

pending
    -> continually eligible for attempts

failed / uncertain attempt
    -> remains pending
    -> another attempt may overlap

successful attempt
    -> consumed


L1
────────────────────────────────

OutboxRuntime
    |
    +-- partitioning
    +-- ordering
    |
    v
OutboxDispatch
    |
    +-- batching?
    +-- routing?
    |      |
    |      +-- key: PartitionKey
    |      +-- member_assignment
    |
    +-- pool
           |
           v
      ExecutionPool
           |
           +-- member_concurrency
```

---

## 14. Shape transformation summary

```diff
 Outbox {
     messages,
     message_identity,
 }

 OutboxInput {
     outbox,
-    messages,
-    acknowledge_on_success,
 }

 OutboxRuntime {
-    delivery,
     partitioning,
     ordering,
     dispatch,
 }

 OutboxDispatch {
     pool,
-    member_assignment,
+    routing?,
     batching?,
 }

+OutboxRouting {
+    key,
+    member_assignment,
+}

+OutboxRoutingKey {
+    PartitionKey,
+}
```

Add structural rule:

```text
exactly one OutboxInput per Outbox
```

Add intrinsic semantic rule:

```text
durable pending message
    -> retried until successfully consumed
    -> retries may overlap stale attempts
```

Retain:

```text
partitioning
ordering
batching
ExecutionPool.member_concurrency
MessageIdentity
transactional OutboxWriteEffect
```

---

## 15. Core distinction from Subscription

The two ingress abstractions intentionally converge at dispatch:

```text
SubscriptionRuntime                  OutboxRuntime
───────────────────                  ─────────────

transport delivery                   intrinsic durable re-drive
grouping                             partitioning
ordering                             ordering
        |                                  |
        v                                  v
SubscriptionDispatch                OutboxDispatch
    pool                                pool
    routing?                            routing?
      key                                 key
      member_assignment                   member_assignment
    batching?                           batching?
```

Their source semantics differ.

Their dispatch semantics should have the same conceptual shape:

> identify the semantic routing domain, choose how that domain maps to pool members, optionally batch logical items, and execute the resulting operation invocations in an `ExecutionPool`.