# Patch Specification — Runtime Grouping and Ordering Scope

**Applies to:** `Conseqa Hierarchical Semantics Refactor Specification`  
**Status:** Proposed patch  
**Purpose:** Replace the current topic-runtime ordering model with a stricter and more compositional runtime grouping/ordering model.

---

# 1. Summary

The current refactor draft places topic ordering in `TopicRuntime` and uses subscription dispatch routing separately.

This patch changes that design.

Grouping and ordering SHALL become two independent L1 runtime facts that may be declared at exactly one of two scopes:

- **topic-wide**, through `TopicRuntime`; or
- **subscription-local**, through `SubscriptionRuntime`.

For any logical topic, these scopes are mutually exclusive.

There SHALL be:

- no inheritance;
- no override semantics;
- no mixed topic/subscription declaration;
- no implicit fallback from one scope to another.

The governing rule is:

> **For a given topic, runtime grouping and ordering are declared either once for the topic as a whole or independently for each subscription, but never at both scopes.**

Grouping and ordering SHALL remain separate sibling fields. No `TransportSemantics` wrapper is introduced.

---

# 2. Motivation

The existing model conflates several distinct concerns.

Historically, a keyed topic-ordering declaration effectively carried two facts:

```text
semantic/runtime grouping domain
+
ordering guarantee within that domain
```

These facts are independent.

A runtime may provide:

```text
grouping by K
without ordering
```

or:

```text
ordering
without keyed grouping
```

or:

```text
grouping by K
with ordering within each group
```

Bundling the grouping key inside a keyed-ordering variant therefore overconstrains the model and forces unrelated consumers to depend on an ordering declaration merely to recover a grouping domain.

This is particularly problematic for serialization reasoning, where the analyzer may need only:

```text
same K
    ->
same runtime grouping domain
```

without requiring any transport precedence.

---

# 3. Why grouping remains L1

A grouping key SHALL remain an L1 fact.

The fields used to compute a grouping key may come from logical message schemas, but the statement:

> this runtime groups messages by these fields

is not an intrinsic property of the L0 application model.

For example:

```text
account_id
```

may be a logical field in L0.

But:

```text
this runtime groups topic traffic by account_id
```

is a runtime-realization fact.

Therefore the distinction is:

```text
L0:
    account_id exists and has application meaning

L1:
    runtime grouping key = account_id
```

The grouping key is semantic in value but runtime-semantic in use.

It SHALL NOT be interpreted as:

```text
worker ID
partition number
physical shard ID
host ID
storage partition ID
```

---

# 4. Why grouping may be topic-scoped

Some runtime systems impose one grouping domain for the topic as a whole.

A Kafka-like realization is the canonical example.

Conceptually:

```text
TopicRuntime
    grouping = K
```

means:

> The runtime realization of this topic establishes one grouping domain derived from K, and all subscriptions consuming this runtime topic observe that same grouping structure.

For example:

```text
TopicRuntime:
    grouping:
        key = account_id
```

asserts:

```text
equal account_id values
    ->
same topic runtime group
```

for every subscription of that topic.

A subscription MAY NOT replace this grouping with another grouping key.

---

# 5. Why grouping may instead be subscription-scoped

Other runtime architectures may establish grouping independently for each subscriber.

For example:

```text
logical topic
        |
        +--> subscription A
        |       grouping = account_id
        |
        +--> subscription B
                grouping = region_id
```

This may arise when publication fans out into independently configured queues or transport paths.

In such an architecture, a topic-wide grouping declaration would be false.

The model must therefore allow:

```text
TopicRuntime:
    no grouping/order declaration

SubscriptionRuntime A:
    grouping = account_id

SubscriptionRuntime B:
    grouping = region_id
```

This is not an override.

It is a different declaration mode.

---

# 6. Exclusive scope

For every logical topic, grouping and ordering SHALL have exactly one declaration scope.

The valid modes are:

## Topic-scoped mode

```text
TopicRuntime:
    grouping
    ordering

SubscriptionRuntime A:
    no grouping
    no ordering

SubscriptionRuntime B:
    no grouping
    no ordering
```

## Subscription-scoped mode

```text
TopicRuntime:
    no grouping
    no ordering

SubscriptionRuntime A:
    grouping
    ordering

SubscriptionRuntime B:
    grouping
    ordering
```

The following SHALL be invalid:

```text
TopicRuntime:
    grouping = account_id
    ordering = within_group

SubscriptionRuntime A:
    grouping = region_id
```

The following SHALL also be invalid:

```text
TopicRuntime:
    grouping = account_id

SubscriptionRuntime A:
    ordering = within_group
```

Grouping and ordering share one exclusive declaration scope.

---

# 7. No inheritance or override semantics

The analyzer SHALL NOT implement rules of the form:

```text
subscription grouping
    overrides topic grouping
```

or:

```text
subscription ordering
    inherits topic ordering unless replaced
```

Such semantics create ambiguity and make correctness reasoning dependent on implicit precedence between declarations.

Instead:

```text
topic-scoped
XOR
subscription-scoped
```

is a structural invariant.

A valid model has one unambiguous source of grouping/order semantics for every runtime subscription.

---

# 8. Flattened representation

No wrapper such as:

```text
TransportSemantics {
    grouping
    ordering
}
```

SHALL be introduced.

The wrapper would add no independent semantic meaning.

Grouping and ordering SHALL appear directly on their owning runtime object.

Conceptually:

```rust
pub struct TopicRuntime {
    pub grouping: GroupingSemantics,
    pub ordering: OrderingSemantics,
}
```

or, in subscription-scoped mode:

```rust
pub struct SubscriptionRuntime {
    pub delivery: DeliverySemantics,

    pub grouping: GroupingSemantics,
    pub ordering: OrderingSemantics,

    pub dispatch: SubscriptionDispatch,
}
```

The declaration scope itself provides the necessary coupling.

---

# 9. Grouping semantics

Grouping SHALL describe runtime equivalence domains.

Conceptually:

```rust
pub enum GroupingSemantics {
    None,
    Keyed(GroupingKey),
}
```

or an equivalent representation.

A keyed grouping declaration means:

```text
grouping_key(A) = grouping_key(B)
    =>
runtime_group(A) = runtime_group(B)
```

It does NOT imply:

```text
ordering

serialization

member assignment

execution affinity

uniqueness

storage partition identity
```

These require additional facts.

---

# 10. Ordering semantics

Ordering SHALL describe runtime precedence guarantees independently of grouping.

Conceptually:

```rust
pub enum OrderingSemantics {
    None,
    Global,
    WithinGroup,
}
```

The exact enum names may differ, but the semantic distinctions SHALL remain.

## `none`

No useful transport precedence guarantee exists.

## `global`

The runtime establishes one precedence relation across all relevant messages in that scope.

## `within_group`

The runtime establishes precedence among messages belonging to the same declared grouping domain.

`within_group` therefore requires a non-empty compatible grouping declaration at the same scope.

---

# 11. Grouping and ordering are independent facts

The model SHALL permit:

```text
grouping = account_id
ordering = none
```

This establishes grouping without transport ordering.

This case is important because it can contribute to serialization without proving ordered execution.

The model SHALL also permit:

```text
grouping = account_id
ordering = within_group
```

which establishes both grouping and same-group precedence.

If meaningful for the implementation space, the model MAY permit:

```text
grouping = none
ordering = global
```

because global precedence does not inherently require a grouping key.

The model SHALL NOT reconstruct a keyed-ordering sum type that couples key declaration and order declaration into one variant.

---

# 12. Effective runtime semantics

For any subscription `S` of topic `T`, the analyzer SHALL determine grouping and ordering from exactly one source.

Conceptually:

```text
if T uses topic-scoped runtime semantics:
    EffectiveGrouping(S) = T.runtime.grouping
    EffectiveOrdering(S) = T.runtime.ordering

else:
    EffectiveGrouping(S) = S.runtime.grouping
    EffectiveOrdering(S) = S.runtime.ordering
```

There is no fallback chain.

There is no override rule.

The model must validate into one mode before analysis begins.

---

# 13. SubscriptionRuntime

`SubscriptionRuntime` continues to own:

```text
delivery
dispatch
```

and, in subscription-scoped transport mode, additionally owns:

```text
grouping
ordering
```

Conceptually:

```rust
pub struct SubscriptionRuntime {
    pub delivery: DeliverySemantics,

    // Present only in subscription-scoped mode.
    pub grouping: Option<GroupingSemantics>,
    pub ordering: Option<OrderingSemantics>,

    pub dispatch: SubscriptionDispatch,
}
```

The implementation SHOULD prefer a representation that makes invalid partial states difficult or impossible to construct.

---

# 14. TopicRuntime

`TopicRuntime` SHALL remain an L1 primitive.

It no longer exists solely as a wrapper around `TopicOrdering`.

Instead, in topic-scoped mode it owns:

```text
grouping
ordering
```

Conceptually:

```rust
pub struct TopicRuntime {
    pub grouping: GroupingSemantics,
    pub ordering: OrderingSemantics,
}
```

If the topic uses subscription-scoped semantics, `TopicRuntime` SHALL contain neither grouping nor ordering facts.

The concrete Rust representation MAY encode this through optional fields, an explicit mode, or another structurally safe representation.

The public DSL SHALL preserve the XOR invariant regardless of internal representation.

---

# 15. Subscription dispatch remains separate

Grouping and ordering describe transport semantics.

`SubscriptionDispatch` remains responsible for realization into execution topology.

It SHALL continue to describe:

```text
target ExecutionPool

semantic routing key

MemberAssignment
```

Dispatch must not be conflated with transport grouping.

The conceptual chain is:

```text
message
    |
runtime grouping/order semantics
    |
    v
SubscriptionDispatch
    |
member assignment
    v
ExecutionPool member
```

---

# 16. Dispatch may contribute to ordering proofs

No additional explicit subscription-ordering declaration is required beyond the runtime ordering fact defined above.

`SubscriptionDispatch` contributes to ordering reasoning through its own normative semantics.

For example, a dispatch mechanism may guarantee that:

> established precedence among messages in one effective runtime group is preserved while assigning and admitting them to execution.

Thus dispatch does not create precedence.

It preserves or propagates precedence established by the effective runtime ordering semantics.

---

# 17. Serialization proof

Serialization does not require an ordering guarantee.

A runtime proof of:

```text
SerializedBy(K)
```

may use:

```text
EffectiveGrouping = K
+
SubscriptionDispatch routes K to one member-ownership domain
+
MemberAssignment preserves safe ownership through handoff
+
ExecutionPool.member_concurrency = bounded(1)
----------------------------------------------------------
SerializedBy(K)
```

No ordering fact participates.

This is a primary motivation for separating grouping from ordering.

---

# 18. Ordering proof

A runtime proof of:

```text
OrderedBy(K)
```

requires additional precedence evidence.

For a grouped case:

```text
EffectiveGrouping = K
+
EffectiveOrdering = within_group
+
SubscriptionDispatch preserves established group precedence
+
MemberAssignment preserves safe ownership
+
ExecutionPool.member_concurrency = bounded(1)
----------------------------------------------------------
OrderedBy(K)
```

Serialization remains a necessary component of the proof chain, but not a sufficient one.

---

# 19. Global ordering

If:

```text
EffectiveOrdering = global
```

the runtime establishes a precedence relation stronger than keyed ordering.

The analyzer MAY restrict that relation to invocations sharing a requirement key `K`.

For example:

```text
global precedence
+
dispatch compatible with K
+
member ownership compatible with K
+
member concurrency = 1
```

may prove:

```text
OrderedBy(K)
```

provided the dispatch path preserves the relevant precedence.

Global transport ordering SHALL NOT automatically imply globally ordered operation execution.

Execution topology must still preserve the precedence.

---

# 20. Example — topic-scoped grouping and ordering

```yaml
runtime:

  topics:

    topic.order_events:

      grouping:
        key:
          schema.OrderCreated:
            - account_id
          schema.OrderCancelled:
            - account_id

      ordering:
        kind: within_group


  subscriptions:

    op.process_order:

      input.events:

        delivery: at_least_once

        dispatch:
          pool: pool.order_workers

          routing:
            key: runtime_group

            member_assignment:
              kind: consistent_hash
```

Here:

```text
topic runtime
    -> establishes grouping by account_id
    -> establishes order within that group
```

Every subscription of this topic shares those runtime transport semantics.

---

# 21. Example — topic-scoped grouping without ordering

```yaml
runtime:

  topics:

    topic.order_events:

      grouping:
        key:
          schema.OrderCreated:
            - account_id

      ordering:
        kind: none
```

This may contribute to:

```text
SerializedBy(account_id)
```

through compatible dispatch and pool concurrency.

It SHALL NOT contribute transport precedence to an ordering proof.

---

# 22. Example — subscription-scoped transport semantics

```yaml
runtime:

  subscriptions:

    op.process_accounts:

      input.events:

        delivery: at_least_once

        grouping:
          key:
            schema.Event:
              - account_id

        ordering:
          kind: within_group

        dispatch:
          pool: pool.account_workers

          routing:
            key: runtime_group

            member_assignment:
              kind: consistent_hash


    op.process_regions:

      input.events:

        delivery: at_least_once

        grouping:
          key:
            schema.Event:
              - region_id

        ordering:
          kind: none

        dispatch:
          pool: pool.region_workers

          routing:
            key: runtime_group

            member_assignment:
              kind: consistent_hash
```

The logical topic has no topic-scoped grouping or ordering declaration.

Each subscription establishes its own runtime transport semantics.

---

# 23. Validation rules

For each topic with runtime subscriptions, validation SHALL determine exactly one mode.

## Topic-scoped mode

Valid iff:

```text
TopicRuntime declares grouping/order semantics
```

and:

```text
no SubscriptionRuntime for that topic
declares grouping/order semantics
```

## Subscription-scoped mode

Valid iff:

```text
TopicRuntime declares neither grouping nor ordering
```

and every runtime subscription requiring these semantics declares its own grouping/order pair explicitly.

Mixed declarations SHALL be rejected.

---

# 24. Pairing rule

Grouping and ordering SHALL be declared as a pair at one scope.

A model SHALL NOT declare:

```text
TopicRuntime.grouping
```

while declaring:

```text
SubscriptionRuntime.ordering
```

for that same topic.

Likewise, it SHALL NOT declare topic ordering while moving grouping into subscriptions.

The pair shares scope even though the two fields retain independent semantics.

This is necessary to prevent ambiguous effective transport semantics.

---

# 25. `within_group` validation

If:

```text
ordering = within_group
```

then the same declaration scope MUST contain a keyed grouping declaration.

The following is invalid:

```text
grouping = none
ordering = within_group
```

because no grouping domain exists over which the ordering guarantee could be interpreted.

---

# 26. Dispatch routing compatibility

If dispatch routes using the effective runtime grouping domain, the routing key SHOULD reference that domain directly rather than duplicate the underlying field mapping.

Conceptually:

```text
routing.key = runtime_group
```

means:

> Use the effective runtime grouping domain established for this subscription.

This avoids repeating schema mappings and gives the analyzer trivial grouping/routing-domain identity.

Explicit dispatch keys MAY still be supported where the runtime dispatch intentionally uses another semantic key.

---

# 27. Migration from the current refactor draft

The current draft:

```text
TopicRuntime.ordering
```

SHALL be replaced by:

```text
TopicRuntime.grouping
TopicRuntime.ordering
```

or by subscription-local:

```text
SubscriptionRuntime.grouping
SubscriptionRuntime.ordering
```

depending on runtime scope.

The current assumption that topic ordering itself contains the topic key SHALL be removed.

Key mappings SHALL move into the new grouping declaration.

---

# 28. Migration from `TopicOrdering::Keyed(TopicKey)`

An existing declaration equivalent to:

```text
TopicOrdering::Keyed(TopicKey K)
```

contains two facts:

```text
grouping = K
ordering = within_group
```

Migration tooling MAY split it mechanically into those two declarations when topic-scoped semantics remain appropriate.

This transformation preserves the existing semantics while removing the coupling.

---

# 29. Migration of `Global`

Existing:

```text
TopicOrdering::Global
```

becomes:

```text
ordering = global
```

It does not itself imply a grouping declaration.

Any grouping required by dispatch or serialization must be declared separately.

This makes `global` usable without inventing a synthetic keyed domain.

---

# 30. Migration of unordered topics

A runtime may now explicitly declare:

```text
grouping = K
ordering = none
```

which was not cleanly expressible when the key existed only inside keyed ordering.

This is an intentional increase in expressive power.

---

# 31. Rationale for strict XOR rather than inheritance

An inheritance model such as:

```text
subscription grouping if present
else topic grouping
```

creates several problems:

1. Effective semantics become dependent on declaration precedence.

2. Partial overrides become possible.

3. Grouping and ordering may accidentally come from different scopes.

4. Correctness proofs become harder to explain because evidence provenance becomes implicit.

5. Backend constraints such as one fixed topic-level grouping domain are easy to contradict accidentally.

The XOR model eliminates all five.

Every subscription's effective grouping/order source is structurally unambiguous.

---

# 32. Rationale for flattening

A wrapper such as:

```text
TransportSemantics
```

would contribute no independent semantics.

It would merely contain:

```text
grouping
ordering
```

The scope owner already communicates that these facts belong together.

Flattening therefore improves:

```text
DSL readability

AST simplicity

diagnostic paths

serialized form

proof evidence paths
```

without losing semantic structure.

---

# 33. Rationale for keeping grouping and ordering independent

Although grouping and ordering share declaration scope, they SHALL NOT be collapsed into one enum.

The analyzer has consumers that need only grouping.

For example:

```text
serialization verifier
routing compatibility verifier
```

should not have to destructure an ordering declaration merely to recover a grouping key.

Likewise, an ordering verifier should explicitly consume:

```text
grouping evidence
+
ordering evidence
```

when both are necessary.

This improves proof provenance and prevents accidental logical coupling.

---

# 34. Updated semantic ownership

After this patch, the ownership of relevant L1 facts is:

| Primitive | Responsibility |
|---|---|
| `TopicRuntime.grouping` | topic-wide runtime grouping domain |
| `TopicRuntime.ordering` | topic-wide transport precedence |
| `SubscriptionRuntime.grouping` | subscription-local runtime grouping domain |
| `SubscriptionRuntime.ordering` | subscription-local transport precedence |
| `SubscriptionRuntime.delivery` | delivery multiplicity |
| `SubscriptionDispatch` | execution routing, member assignment, precedence preservation |
| `ExecutionPool` | execution-member concurrency |

Grouping/order fields exist at either the topic or subscription scope for a given topic, never both.

---

# 35. Acceptance criteria

This patch is complete when:

1. `TopicOrdering::Keyed(TopicKey)` no longer couples grouping and ordering.

2. Grouping is modeled as an independent L1 fact.

3. Ordering is modeled as an independent L1 fact.

4. Grouping and ordering are flattened directly onto their owning runtime object.

5. No `TransportSemantics` wrapper exists.

6. A topic may use topic-scoped grouping/order semantics.

7. A topic may instead use subscription-scoped grouping/order semantics.

8. Topic-scoped and subscription-scoped declarations are mutually exclusive.

9. No inheritance semantics exist.

10. No override semantics exist.

11. Grouping and ordering cannot be declared at different scopes for the same topic.

12. `within_group` requires compatible grouping at the same scope.

13. Grouping without ordering is valid.

14. Global ordering without keyed grouping may be valid.

15. Serialization proofs may consume grouping without ordering.

16. Ordering proofs require explicit precedence evidence.

17. Subscription dispatch may preserve ordering but does not invent precedence.

18. Runtime grouping keys remain distinct from physical worker IDs and storage partition identities.

19. Existing keyed ordering can be migrated as:
    `grouping = K` + `ordering = within_group`.

20. Proof evidence records grouping and ordering as independent facts.

---

# 36. Normative principle

> **Runtime grouping and runtime ordering are independent L1 facts that share one exclusive declaration scope. For any topic, they are declared either at `TopicRuntime` for all subscriptions or independently at each `SubscriptionRuntime`, but never at both. No inheritance or override semantics exist. Grouping defines runtime equivalence domains; ordering defines runtime precedence; dispatch maps those domains into execution topology and may preserve, but never invent, established precedence.**