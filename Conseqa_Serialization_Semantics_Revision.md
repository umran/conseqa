# Conseqa Serialization Semantics Revision

## 1. Add L0 `InvocationLock`

Add an optional operation-level declaration:

```rust
pub struct Operation {
    ...
    pub invocation_lock: Option<InvocationLock>,
    ...
}

pub struct InvocationLock {
    pub key: ValueRef,
}
```

An `InvocationLock` is eligible only at operation entry. It is not a program step.

Semantics:

```text
evaluate key from invocation context
    ->
acquire exclusive InvocationLock(key)
    ->
execute first program step
    ->
...
    ->
operation terminal
    ->
release InvocationLock(key)
```

The lock is acquired before any operation program step executes and is held until that invocation reaches `return` or `complete`.

Two invocations whose evaluated lock keys are equal cannot execute their operation programs concurrently.

Therefore:

```text
InvocationLock(K)
    =>
SerializedBy(K)
```

provided the lock key is established to be the same logical value as the serialization-requirement key.

The primitive asserts the abstract exclusion guarantee, not its mechanism. PostgreSQL advisory locks, distributed mutexes, fenced lock services, etc. are Confluence realization concerns.

`InvocationLock` does not establish invocation ordering. In particular, it makes no FIFO acquisition guarantee.

Async effects allowed to outlive the operation terminal are not implicitly kept under the lock after terminal.

---

## 2. Revise `MemberAssignment`

Remove the current implicit rule that every assignment used by a serialization proof necessarily provides safe ownership transfer.

`MemberAssignment::ConsistentHash` should assert only:

> During a stable ownership epoch, equal routing domains are assigned to the same ExecutionPool member.

Thus:

```text
ConsistentHash(K)
    =>
same K -> same member
within one stable ownership epoch
```

It does **not** by itself assert anything about overlap between a previous owner and its successor during:

- worker replacement;
- failure recovery;
- scaling;
- membership changes;
- partition reassignment;
- ownership rebalance.

`RoundRobin` remains non-affine and remains unusable for keyed serialization proofs.

---

## 3. Add an explicit execution-handoff guarantee

Extend `ExecutionPool`:

```rust
pub struct ExecutionPool {
    pub member_concurrency: MemberConcurrency,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_handoff: Option<ExecutionHandoff>,
}

pub enum ExecutionHandoff {
    ExclusiveOwnership,
}
```

Absence means no usable fact about execution overlap across ownership/member transitions.

### `ExclusiveOwnership`

The declaration:

```yaml
execution_handoff: exclusive_ownership
```

asserts:

> When execution authority for a routing domain transfers from one pool member or member incarnation to another, the runtime preserves exclusive execution ownership of that domain across the transition.

If:

```text
owner(K, E)   = A
owner(K, E+1) = B
```

then the runtime cannot allow an invocation for `K` executing under A's old authority to overlap an invocation for `K` executing under B's successor authority.

This covers both:

```text
domain reassignment:
    A -> B
```

and:

```text
member replacement:
    A(old incarnation) -> A'(replacement)
```

The guarantee concerns execution authority, not merely control-plane membership or agreement.

Therefore none of the following alone establishes it:

```text
membership lease expiry
worker declared unhealthy
new member started
consistent-hash ring recomputed
consensus agrees on new owner
```

A conforming realization must actually preserve exclusive execution ownership.

---

## 4. Revised topology serialization proof

The topology proof becomes explicitly four-legged:

```text
semantic key equivalence
        |
        v
routing-domain equivalence
        |
        v
stable-epoch member affinity
        |
        v
exclusive execution handoff
        |
        v
member concurrency = 1
        |
        v
SerializedBy(K)
```

More concretely:

```text
routing key == serialization key
+
MemberAssignment::ConsistentHash
+
ExecutionPool.execution_handoff
    == ExclusiveOwnership
+
ExecutionPool.member_concurrency
    == Bounded(1)

=>

SerializedBy(K)
```

All four facts are required.

---

## 5. Why `bounded(1)` is insufficient

`member_concurrency = bounded(1)` means only:

> One member executes at most one invocation at a time.

It does not imply:

> A stale invocation belonging to a former member/member incarnation cannot coexist with work running on its replacement.

Thus this execution is admitted without the new handoff guarantee:

```text
member A:
    M ---------------------------->

A becomes unreachable

replacement B:
              M ------------------>

A may actually still be executing
```

Even though both A and B individually satisfy:

```text
member_concurrency = 1
```

the same-key invocations overlap.

Therefore `bounded(1)` cannot by itself bridge execution ownership transitions.

---

## 6. Serialization proof routes

Conseqa now has two independent positive proof routes.

### Route A — explicit synchronization

```text
InvocationLock(K)
    =>
SerializedBy(K)
```

This route is L0-only.

It does not depend upon routing, pool concurrency, member lifecycle, or handoff guarantees.

### Route B — runtime topology

```text
same semantic routing domain
+
ConsistentHash affinity
+
ExclusiveOwnership execution handoff
+
member_concurrency = bounded(1)
    =>
SerializedBy(K)
```

This route is runtime-dependent.

Neither route implies ordering.

---

## 7. Subscription example

```yaml
routing:
  key: grouping_key
  member_assignment:
    kind: consistent_hash

execution_pools:
  pool.consumers:
    member_concurrency:
      kind: bounded
      value: 1
    execution_handoff: exclusive_ownership
```

For serialization key `tenant_id`, the verifier must establish:

```text
tenant_id
    == effective grouping key

grouping key
    == SubscriptionRouting domain

ConsistentHash
    -> same tenant routed to one member per stable epoch

ExclusiveOwnership
    -> replacement/reassignment cannot create stale-owner overlap

Bounded(1)
    -> that member cannot overlap invocations

therefore:
    SerializedBy(tenant_id)
```

---

## 8. Outbox example

After the proposed Outbox revision:

```yaml
routing:
  key: partition_key
  member_assignment:
    kind: consistent_hash
```

plus:

```yaml
execution_pool:
  member_concurrency:
    kind: bounded
    value: 1
  execution_handoff: exclusive_ownership
```

provides the same topology proof over the outbox partition domain.

Ordinary outbox message leases remain weaker:

```text
lease expiry
    -> may permit redelivery

lease expiry
    -/-> old attempt terminated
```

Therefore an ordinary polling lease cannot satisfy `ExclusiveOwnership` unless additional realization machinery actually provides that stronger guarantee.

---

## 9. Request routing example

Request routing follows exactly the same composition:

```text
request routing key
+
ConsistentHash
+
ExclusiveOwnership
+
Bounded(1)
    =>
serialization
```

This makes the execution-handoff concept completely ingress-independent.

---

## 10. Remove implicit handoff semantics from routing

The existing semantic statement equivalent to:

```text
Any MemberAssignment used to establish keyed serialization
must preserve exclusive ownership through reassignment.
```

should be removed.

Replace it with two explicit facts:

```text
MemberAssignment
    -> describes assignment/affinity

ExecutionPool.execution_handoff
    -> describes continuity of exclusive execution authority
       across assignment/member transitions
```

This prevents `ConsistentHash` from silently carrying a much stronger distributed-systems guarantee than its declaration visibly states.

---

## 11. Revised proof evidence

Topology-based proof output should become straightforward:

```text
Requirement:
    SerializedBy(tenant_id)

Verdict:
    Proven

Scope:
    runtime_dependent

Evidence:
    SubscriptionRouting
        key                 = grouping_key
        member_assignment   = consistent_hash

    ExecutionPool workers
        execution_handoff   = exclusive_ownership
        member_concurrency  = bounded(1)
```

Invocation-lock proof:

```text
Requirement:
    SerializedBy(tenant_id)

Verdict:
    Proven

Scope:
    l0_only

Evidence:
    InvocationLock
        key = tenant_id
```

No special proof-dependency category is necessary.

---

## 12. Important non-implications

```text
ConsistentHash
    -/-> safe failover

Bounded(1)
    -/-> safe member replacement

ExclusiveOwnership
    -/-> bounded member concurrency

InvocationLock
    -/-> ordering

idempotency
    -/-> serialization

message lease
    -/-> invocation fencing
```

Each fact has one narrow semantic responsibility.