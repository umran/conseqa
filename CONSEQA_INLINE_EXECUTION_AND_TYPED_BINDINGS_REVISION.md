# Conseqa — Inline Operation Execution and Typed Bindings Revision

**Status:** Proposed comprehensive semantic revision  
**Repository:** https://github.com/umran/conseqa  
**Baseline:** `master` @ `e32a192a13b8a1be71345d16092698ffc6ab63a8`  
**Baseline date:** 2026-09-05

## 1. Purpose

Conseqa now models each operation as one explicit causal `OperationBlock`, but the surface still predeclares four execution-local registries:

```text
Operation.effects
Operation.effect_intents
Operation.transaction_outputs
Operation.transactions
```

The program then refers back to those declarations. This revision removes that indirection.

The governing rule is:

> If a semantic object exists because control reaches a particular execution site, declare it at that site. Keep a separate shared declaration only when the contract exists independently of any one execution occurrence.

Accordingly:

```text
INLINE IN THE OPERATION PROGRAM
    transactions
    direct effects
    explicitly established effect intents
    transaction outputs

RETAIN AS SHARED DECLARATIONS
    inputs
    schemas
    data models / objects
    topics
    state machines
    state-machine transition side-effect contracts
    requirements
    execution guarantees
```

The revision also formalizes **typed immutable bindings** for values and artifacts produced during execution.

This is not a general variable system. Bindings are single-producer semantic names whose meaning is determined by the construct that introduces them.

---

## 2. Goals

This revision MUST:

1. remove `Operation.transactions` and inline transactions at `OperationStep::Transaction`;
2. remove `Operation.effects` and inline direct effect contracts at `ExecuteEffect`;
3. remove `Operation.effect_intents` and bind intents where transactions establish them;
4. remove `Operation.transaction_outputs` and bind typed outputs where transactions establish them;
5. rename existing producer fields toward binding terminology (`Read.result`, effect `result`, etc.);
6. preserve the semantic distinction among transaction reads, transaction outputs, effect intents, and effect results;
7. preserve all existing replay, idempotency, recoverability, ordering, serialization, and state-machine semantics;
8. retain `Model.state_machines` because state machines are shared persistent domain contracts;
9. retain transition-owned effect contracts on state-machine transitions;
10. derive analyzer indexes from the program instead of using operation-level declaration maps.

Non-goals:

```text
mutable variables
assignment
loops
functions
transaction/effect calls or reuse
phi nodes
general expressions
retry policies
durable workflows
checkpoints
new correctness requirements
```

`Derivation` remains the opaque computation/provenance vocabulary.

---

## 3. New `Operation` shape

### Before

```rust
pub struct Operation {
    pub service: Id,
    pub description: Option<String>,
    pub inputs: BTreeMap<Id, Input>,

    pub effects: BTreeMap<Id, Effect>,
    pub effect_intents: BTreeMap<Id, EffectIntent>,
    pub transaction_outputs: BTreeMap<Id, TransactionOutput>,
    pub transactions: BTreeMap<Id, Transaction>,

    pub program: OperationBlock,
    pub requirements: OperationRequirements,
    pub execution: ExecutionSemantics,
}
```

### After

```rust
pub struct Operation {
    pub service: Id,
    pub description: Option<String>,
    pub inputs: BTreeMap<Id, Input>,

    pub program: OperationBlock,

    pub requirements: OperationRequirements,
    pub execution: ExecutionSemantics,
}
```

The program is now the source of truth for all operation-owned execution occurrences.

---

## 4. Stable IDs versus bindings

This revision distinguishes two concepts.

### Stable execution-site IDs

Some inline occurrences need stable semantic identity:

```text
Transaction.id
ExecuteEffect.effect_id
EstablishEffectIntent.effect_id
```

These IDs do not reference another declaration. They identify the inline declaration itself.

They are useful for:

```text
keyed transaction commit identity
diagnostics
conformance
proof evidence
visualization
future performance / telemetry attachment
```

### Bindings

Bindings name something produced by execution:

```text
transaction read observation
transaction output artifact
effect-intent artifact
effect result observation
```

Bindings are:

```text
immutable
single-producer
operation-local
scoped by program / transaction structure
```

No rebinding or shadowing is introduced in this revision.

Binding IDs SHOULD be unique across all binding-producing sites of one operation.

---

## 5. Value-source kinds remain typed

Do not collapse all values into an untyped `binding` source.

The current distinctions encode important solver semantics and should remain:

```rust
pub enum ValueSource {
    Input(Id),
    Effect(Id),
    TransactionOutput(Id),
    StateMachineSubject(Id),
    TransactionRead(Id),
    EffectResultOk(Id),
    EffectResultErr(Id),
}
```

What changes is where those IDs are introduced.

For example:

```text
TransactionOutput(o)
```

formerly resolves through:

```text
Operation.transaction_outputs[o]
```

and after this revision resolves through:

```text
EstablishTransactionOutput { bind: o, schema, ... }
```

Likewise:

```text
Effect(e)
```

for an operation-owned effect resolves to an inline effect site whose `effect_id = e`.

This preserves semantic source kinds without preserving predeclaration registries.

---

## 6. Inline transactions

Replace:

```rust
OperationStep::Transaction(RunTransaction)
```

with:

```rust
OperationStep::Transaction(Transaction)
```

and change `Transaction` to carry its stable ID:

```rust
pub struct Transaction {
    pub id: Id,
    pub data_model: Option<Id>,
    pub isolation: TransactionIsolation,
    pub idempotency: IdempotencyGuarantee,
    pub steps: Vec<TransactionStep>,
}
```

Remove `RunTransaction`.

### Semantics

Reaching:

```text
OperationStep::Transaction(T)
```

means execute that inline atomic unit.

If:

```text
T.idempotency = DeduplicatedBy(K)
```

and the logical commit already exists, the step resolves the prior commit and restores its retained artifacts rather than re-running the body.

Conceptually the durable identity is:

```text
Commit(operation_id, T.id, K)
```

`StepLocation` is not a substitute for `T.id`: moving an inline transaction should not silently change durable commit identity.

### No semantic transaction reuse

One inline transaction declaration is one transaction occurrence.

If two locations genuinely execute transactions, declare two inline transactions with distinct IDs.

If authoring reuse is later desired, it should be a macro/template layer that expands before semantic analysis rather than a semantic transaction-call primitive.

---

## 7. Transaction read bindings

Change:

```rust
pub struct Read {
    pub result: Id,
    ...
}
```

to:

```rust
pub struct Read {
    pub bind: Id,
    pub target: ObjectSelector,
    pub fields: FieldSelection,
}
```

Example:

```yaml
- kind: read
  bind: read.stock
  target:
    object: object.stock
    ...
  fields:
    kind: only
    fields: [on_hand, reserved]
```

Later steps use:

```yaml
source: transaction_read:read.stock
path: reserved
```

The binding:

- exists only after the read;
- is visible only in the same transaction;
- never becomes a transaction artifact;
- retains the current conservative transaction-read replay rule.

---

## 8. Inline transaction outputs

Remove:

```rust
Operation.transaction_outputs
```

and remove `TransactionOutput` as a separate operation-level declaration.

Change:

```rust
pub struct EstablishTransactionOutput {
    pub output: Id,
    pub values: Derivation,
}
```

to:

```rust
pub struct EstablishTransactionOutput {
    pub bind: Id,
    pub schema: Id,
    pub values: Derivation,
}
```

Example:

```yaml
- kind: establish_transaction_output
  bind: output.create_order
  schema: schema.CreateOrderResponse
  values:
    kind: deterministic
    from:
      - source: input:input.create_order.request
        path: order_id
```

The binder now declares in one place:

```text
artifact identity
schema
producer transaction
producer step
derivation
```

### Semantics are unchanged

A transaction output still means:

> a typed logical value deliberately exported from a transaction into subsequent operation control.

It still does not imply response, success/failure, database storage, idempotency, or memoization.

Replay remains:

```text
Route A:
    naturally replayable transaction
    +
    replay-deterministic derivation
    ->
    reconstruct same output

Route B:
    DeduplicatedBy(K)
    +
    stable commit key
    ->
    recover exact original output from Commit(T,K)
```

---

## 9. Inline direct effects

Remove:

```rust
Operation.effects
```

Change:

```rust
pub struct ExecuteEffect {
    pub effect: Id,
    pub values: Derivation,
    pub result: Option<Id>,
}
```

to:

```rust
pub struct ExecuteEffect {
    pub effect_id: Id,
    pub effect: Effect,
    pub values: Derivation,
    pub bind: Option<Id>,
}
```

Example:

```yaml
- kind: execute_effect
  effect_id: effect.charge_payment.card
  effect:
    kind: external
    name: payment-provider.charge
    idempotency:
      kind: not_deduplicated
    result:
      ok: schema.ChargeAccepted
      err: schema.ChargeDeclined
  values:
    kind: unspecified
  bind: result.charge_payment.card
```

`effect_id` names the inline site itself; it does not reference an operation registry.

The distinction remains:

```text
effect
    = logical contract

values
    = provenance of the concrete instance constructed here
```

Publication, request, external-idempotency, and external-result semantics remain unchanged.

---

## 10. Inline explicit effect intents

Remove:

```rust
Operation.effect_intents
```

and remove the standalone operation-level:

```rust
EffectIntent { effect: Id }
```

declaration.

Change:

```rust
pub struct EstablishEffectIntent {
    pub intent: Id,
    pub values: Derivation,
}
```

to:

```rust
pub struct EstablishEffectIntent {
    pub bind: Id,
    pub effect_id: Id,
    pub effect: Effect,
    pub values: Derivation,
}
```

Example:

```yaml
- kind: establish_effect_intent
  bind: intent.publish_created
  effect_id: effect.publish_created
  effect:
    kind: publication
    topic: topic.order_events
    schema: schema.OrderCreated
    idempotency_key_propagation: []
  values:
    kind: deterministic
    from:
      - source: input:input.create_order.request
        path: order_id
```

Later:

```yaml
- kind: execute_effect_intent
  intent: intent.publish_created
```

The transaction step simultaneously:

1. declares the effect contract;
2. constructs the concrete logical effect instance from `values`;
3. establishes that exact instance as the bound `EffectIntent` artifact.

The intent binding is not the effect declaration. The separate `effect_id` identifies the captured logical effect site.

### Replay semantics remain unchanged

For an intent `I` established by transaction `T`:

```text
natural route:
    T naturally replayable
    +
    intent derivation replay-deterministic
    ->
    reconstruct same I

keyed route:
    T DeduplicatedBy(K)
    ->
    recover exact I from Commit(T,K)
```

`ExecuteEffectIntent(I)` never recomputes the effect values and still does not imply exactly-once effect execution.

---

## 11. Effect result bindings

Rename:

```text
ExecuteEffect.result
ExecuteEffectIntent.result
```

to:

```text
bind
```

Conceptually:

```rust
pub struct ExecuteEffectIntent {
    pub intent: Id,
    pub bind: Option<Id>,
}
```

The binding is allowed only when the underlying effect contract is result-bearing.

It remains an attempt-local observation, not a transaction artifact.

`MatchResult` can remain:

```rust
pub struct MatchResult {
    pub result: Id,
    pub ok: OperationBlock,
    pub err: OperationBlock,
}
```

where `result` names the effect-result binding.

The current arm-local:

```text
effect_result_ok:<result>
effect_result_err:<result>
```

semantics remain unchanged.

No second explicit variant-payload binder is introduced in this revision.

---

## 12. State machines remain global

`Model.state_machines` remains unchanged.

A state machine exists independently of any one operation and may be applied by multiple operations.

Likewise, transition-owned side-effect contracts remain on:

```text
StateMachine
    -> Transition
        -> side_effects
```

This is intentionally different from operation-owned effects, which are execution-local and are now inline.

---

## 13. Transition-established intent bindings

The current transition application carries only:

```text
effect_values: BTreeMap<effect_id, Derivation>
```

and relies on `Operation.effect_intents` to provide handles for the implicit artifacts.

Replace it with:

```rust
pub struct StateTransition {
    pub machine: Id,
    pub transition: Id,
    pub subject: ObjectSelector,
    pub effect_intents: BTreeMap<Id, TransitionEffectIntent>,
}

pub struct TransitionEffectIntent {
    pub bind: Id,
    pub values: Derivation,
}
```

The map key remains the state-machine transition side-effect ID.

Example:

```yaml
- kind: transition
  machine: machine.order_lifecycle
  transition: transition.order.mark_paid
  subject: ...
  effect_intents:
    effect.order.paid:
      bind: intent.order_paid
      values:
        kind: deterministic
        from:
          - source: input:input.apply_payment.captured
            path: order_id
```

Later:

```yaml
- kind: execute_effect_intent
  intent: intent.order_paid
```

Exact coverage remains mandatory:

```text
transition.side_effects.keys()
==
state_transition.effect_intents.keys()
```

Each entry supplies exactly:

```text
one concrete derivation
one operation-local intent binding
```

A successful transaction atomically:

1. applies the state transition;
2. constructs each transition side-effect instance;
3. establishes each bound intent artifact;
4. commits state and artifacts together.

The old operation-level transition-intent uniqueness/establishability checks disappear because the application site now names the artifact directly.

---

## 14. Effect IDs and `ValueSource::Effect`

`ValueSource::Effect(effect_id)` remains valid.

For operation-owned inline effects, `effect_id` resolves to:

```text
ExecuteEffect.effect_id
```

or:

```text
EstablishEffectIntent.effect_id
```

For transition-owned effects, it resolves as today to the state-machine transition side-effect declaration.

This preserves existing idempotency-key-propagation/value-lineage semantics without needing an operation effect registry.

Every inline operation-owned `effect_id` MUST be unique within the operation.

---

## 15. Binding scope and definite availability

The current forward availability analysis remains conceptually correct, but producers are discovered inline.

### Transaction read

```text
before Read(bind=r): r unavailable
after Read(bind=r):  r available inside same transaction
transaction exit:    r unavailable
```

### Transaction output / effect intent

For inline transaction `T`:

```text
Artifacts(T)
=
outputs explicitly bound by T
+
intents explicitly bound by T
+
transition intents bound by T
```

After successful execution or keyed commit resolution:

```text
Available(after T)
=
Available(before T)
∪ Artifacts(T)
```

### Effect result

After a result-bearing effect execution with `bind: r`:

```text
r becomes definitely bound
```

### Join

Retain:

```text
Available(join)
=
intersection(
    Available(each predecessor that falls through)
)
```

Terminated predecessor arms impose no condition on the join.

A binding with one syntactic producer cannot be magically merged with a differently produced value in another arm. This revision adds no phi/merge construct.

If both falling-through arms need to produce different values for a later common consumer, keep the consumer inside each arm or restructure the program. A future explicit merge primitive can be added only if real models justify it.

---

## 16. No forward references

Bindings exist only after their producer.

Invalid:

```text
use transaction output x
...
later establish x
```

Invalid:

```text
execute intent i
...
later bind i
```

Invalid:

```text
match r
...
later execute effect bind r
```

The validator reports a use-before-bind / not-definitely-available error rather than resolving an operation-wide declaration and hoping control eventually produces it.

---

## 17. Operation requirements do not see program-local bindings

Operation requirements live outside the causal execution body.

Program-produced bindings are therefore not in lexical scope inside:

```text
Operation.requirements
```

This is consistent with the existing V1 requirement model, especially idempotency/recoverability governing keys, which must define the invocation equivalence class from the triggering boundary rather than from values produced later by the invocation.

---

## 18. Inline effect declaration roots

Effect contracts may contain value references, including external deduplication keys and idempotency-key propagation.

After inlining, evaluate those roots at the effect site's actual context.

### Direct effect

Both:

```text
effect declaration roots
values derivation roots
```

must be valid in the operation context immediately before the `execute_effect` step.

### Explicit intent

The effect contract roots and instance derivation are evaluated in the enclosing transaction context at the `establish_effect_intent` step.

They may therefore use preceding transaction reads/outputs where current rules permit them.

### Transition-owned effect

The effect contract remains on the state-machine transition and continues to use the applying transition context under existing rules.

---

## 19. Analyzer program index

Replace operation-map lookup with a derived program index.

Walk every operation program recursively and record at least:

```text
transaction_id
    -> operation + StepLocation + Transaction

effect_id
    -> operation + producer kind + StepLocation + Effect

binding_id
    -> operation
       + binding kind
       + producer location
       + enclosing transaction if any
       + schema/result contract where relevant

transition intent binding
    -> applying transaction
       + machine
       + transition
       + transition side-effect id
```

The program remains the semantic source of truth.

The index is analysis infrastructure only.

---

## 20. Validation changes

Remove ownership checks whose only purpose is:

```text
transaction belongs to Operation.transactions
effect belongs to Operation.effects
intent belongs to Operation.effect_intents
output belongs to Operation.transaction_outputs
```

Add/retain:

### Identity collection

Reject duplicate:

```text
Transaction.id
inline effect_id
binding id
```

within an operation.

### Transaction validation

Preserve:

```text
data-model ownership
object selectors
field paths
isolation
locks
lock ordering
transition references
commit-key roots
derivations
read-before-use
```

### Inline effect validation

Preserve:

```text
publication topic/schema membership
request target/input/schema
external result contract
external idempotency key
key propagation
result-binding legality
```

### Binding validation

Require:

```text
transaction_read
    produced earlier in same transaction

transaction_output
    produced by inline output binder
    definitely available
    path valid against binder schema

effect_intent
    produced by explicit or transition binder
    definitely available

effect result
    produced by result-bearing effect
    definitely bound before match/use

variant payload
    remains inside corresponding match arm
```

### Transition validation

Replace `effect_values` exact-coverage validation with `effect_intents` exact coverage plus bind validation.

Remove the old scan of `Operation.effect_intents` used to find the one handle for a transition side effect.

---

## 21. Solver changes

The representation changes, not the correctness meanings.

### Natural transaction replay

Analyze the inline transaction body exactly as today.

Amendment A remains:

```text
transition-containing transaction
    -> V1 natural replay route unavailable
    -> not structurally invalid
```

### Keyed transaction recovery

Resolve:

```text
Transaction.id
idempotency key
artifact bindings produced by this inline transaction
```

and retain exact route-B recovery semantics.

### Transaction-output replay

Resolve output schema/derivation/producer directly from the binding site rather than `Operation.transaction_outputs`.

### Effect-intent replay

Resolve explicit intents from `EstablishEffectIntent` and transition intents from `StateTransition.effect_intents`.

### Effect duplicate safety

Enumerate inline effect occurrences on admitted program paths rather than iterating `Operation.effects`.

### Effect-result replay

Resolve a result binding to its inline producer and contract:

```text
RequestEffect
    -> target operation proof

ExternalEffect
    -> strengthened DeduplicatedBy terminal-result rule

Publication
    -> no result
```

### Ordering / serialization / access analysis

Enumerate inline transactions/effects recursively through program blocks. The program location now directly supplies branch/path context.

No requirement definition changes.

---

## 22. Canonical migration examples

### Transaction/output before

```yaml
transaction_outputs:
  output.create_order:
    schema: schema.CreateOrderResponse

transactions:
  tx.create_order:
    data_model: data.checkout
    isolation: read_committed
    idempotency:
      kind: unspecified
    steps:
      - kind: establish_transaction_output
        output: output.create_order
        values:
          kind: deterministic
          from:
            - source: input:input.create_order.request
              path: order_id

program:
  steps:
    - kind: transaction
      transaction: tx.create_order
```

### After

```yaml
program:
  steps:
    - kind: transaction
      id: tx.create_order
      data_model: data.checkout
      isolation: read_committed
      idempotency:
        kind: unspecified
      steps:
        - kind: establish_transaction_output
          bind: output.create_order
          schema: schema.CreateOrderResponse
          values:
            kind: deterministic
            from:
              - source: input:input.create_order.request
                path: order_id
```

---

### Direct effect before

```yaml
effects:
  effect.charge:
    kind: external
    name: payment-provider.charge
    idempotency:
      kind: not_deduplicated
    result:
      ok: schema.ChargeAccepted
      err: schema.ChargeDeclined

program:
  steps:
    - kind: execute_effect
      effect: effect.charge
      values:
        kind: unspecified
      result: result.charge
```

### After

```yaml
program:
  steps:
    - kind: execute_effect
      effect_id: effect.charge
      effect:
        kind: external
        name: payment-provider.charge
        idempotency:
          kind: not_deduplicated
        result:
          ok: schema.ChargeAccepted
          err: schema.ChargeDeclined
      values:
        kind: unspecified
      bind: result.charge
```

---

### Explicit intent before

```yaml
effects:
  effect.publish_created:
    kind: publication
    topic: topic.order_events
    schema: schema.OrderCreated
    idempotency_key_propagation: []

effect_intents:
  intent.publish_created:
    effect: effect.publish_created

transactions:
  tx.create:
    ...
    steps:
      - kind: establish_effect_intent
        intent: intent.publish_created
        values: ...
```

### After

```yaml
program:
  steps:
    - kind: transaction
      id: tx.create
      ...
      steps:
        - kind: establish_effect_intent
          bind: intent.publish_created
          effect_id: effect.publish_created
          effect:
            kind: publication
            topic: topic.order_events
            schema: schema.OrderCreated
            idempotency_key_propagation: []
          values: ...
```

---

### Transition side effect before

```yaml
effect_intents:
  intent.order_paid:
    effect: effect.order.paid

transactions:
  tx.mark_paid:
    steps:
      - kind: transition
        machine: machine.order_lifecycle
        transition: transition.order.mark_paid
        effect_values:
          effect.order.paid:
            kind: deterministic
            from: [...]
```

### After

```yaml
program:
  steps:
    - kind: transaction
      id: tx.mark_paid
      ...
      steps:
        - kind: transition
          machine: machine.order_lifecycle
          transition: transition.order.mark_paid
          effect_intents:
            effect.order.paid:
              bind: intent.order_paid
              values:
                kind: deterministic
                from: [...]
```

---

## 23. Mechanical migration algorithm

For each operation:

1. For every `Transaction(transaction=T)` program step, inline `Operation.transactions[T]` and set `Transaction.id = T`.
2. For every direct `ExecuteEffect(effect=E)`, inline `Operation.effects[E]`, set `effect_id = E`, and rename `result -> bind`.
3. For every `EstablishEffectIntent(intent=I)`, resolve `Operation.effect_intents[I].effect = E`, inline `Operation.effects[E]`, set `bind = I`, `effect_id = E`.
4. For every `EstablishTransactionOutput(output=O)`, inline `Operation.transaction_outputs[O].schema`, set `bind = O`.
5. For every transition side-effect handle, move its intent ID into the applying `StateTransition.effect_intents[effect_id].bind` next to the existing derivation.
6. Rename `Read.result -> Read.bind`.
7. Rename `ExecuteEffectIntent.result -> bind`.
8. Remove the four operation-level registries.

Migration MUST stop for manual resolution rather than guess when:

```text
one transaction declaration is referenced from multiple program sites
one effect declaration is used at multiple semantically distinct sites
one output has multiple independent producers
one transition intent handle corresponds ambiguously to multiple applications
new binding IDs collide
```

Those cases expose exactly the execution-site ambiguity this revision removes.

---

## 24. Impact on agent design

This revision should improve LLM architecture synthesis and patch merging.

Before, adding one transaction commonly requires two edits:

```text
Operation.transactions
+
Operation.program
```

Adding an intent may require:

```text
Operation.effects
+
Operation.effect_intents
+
Operation.transactions
+
Operation.program
```

After, the semantic unit is one localized program subtree.

That gives sub-agents cleaner write scopes and fewer cross-registry ID coordination failures.

The deterministic analyzer still decides whether independently proposed subtrees compose correctly.

---

## 25. Future performance-layer compatibility

Inline sites are a better attachment surface for higher-level probabilistic performance analysis.

Metrics can later attach to:

```text
operation
Transaction.id
effect_id
StepLocation
```

Two calls to the same provider can have distinct distributions because they are distinct effect sites.

No performance semantics are added by this revision.

---

## 26. Visualization/reporting impact

The visualizer should derive transactions/effects directly from the program instead of rendering separate operation declaration inventories.

Proof/report language should prefer:

```text
inline transaction `tx.reserve`
inline effect `effect.payment.charge`
bound intent `intent.order_paid`
bound output `output.create_order`
```

State machines remain separate model-level entities.

---

## 27. Expected source touchpoints

At least:

```text
src/spec/operation/mod.rs
src/spec/operation/program.rs
src/spec/operation/transaction.rs
src/spec/operation/effect.rs
src/spec/operation/effect_intent.rs        remove/shrink
src/spec/operation/transaction_output.rs   remove/shrink
src/spec/operation/value.rs

src/analyzer/validation/*
src/analyzer/verification/*
reference/index construction
idempotency solver
recoverability solver
ordering / serialization / access indexes

src/bin/viz/*
viz/src/*

tests/parser.rs
tests/validation.rs
tests/verification.rs
tests/report.rs
tests/examples.rs
tests/fixtures/*.yaml

CONSEQA_DSL_SEMANTICS.md
CONSEQA_VIZ.md
```

---

## 28. Required tests

### Parsing / surface

- operation without the four removed registries;
- inline transactions;
- inline direct effects;
- inline explicit intents;
- inline transaction output schema;
- transition intent-binding map;
- old registry fields rejected.

### Identity

- duplicate transaction ID rejected;
- duplicate effect ID rejected;
- duplicate binding ID rejected.

### Scope

- read bind usable only after read and only in same transaction;
- output bind usable after producer transaction;
- output bind unavailable when not present on every falling-through predecessor;
- intent unavailable before producer transaction;
- result unavailable before effect execution;
- result variant scope unchanged.

### Transition

- exact transition side-effect coverage;
- each side effect receives one bind and one derivation;
- keyed transaction restores exact transition intent;
- transition without keyed dedup remains structurally valid under Amendment A.

### Verification regression

Migrate existing fixtures and preserve existing verdicts for:

```text
serialization
ordering
idempotency
result replay
recoverability
natural transaction replay
keyed artifact recovery
publication/request duplicate cascade
external DeduplicatedBy terminal results
transition replay gaps
```

Any changed verdict must be justified by an intentional semantic change, not by the representation migration.

---

## 29. Normative replacement text

### Operation structure

> An operation contains its invocation sources, one explicit causal program, requirements, and execution facts. Execution-local transactions, direct effects, transaction outputs, and effect intents are declared at the program or transaction site that executes or establishes them. They are not predeclared as operation-level capabilities or handles.

### Inline transaction

> A `transaction` operation step declares and executes one atomic transaction at that point in the operation program. The transaction carries a stable logical ID together with its data-model boundary, isolation guarantee, idempotency guarantee, and ordered body. The ID identifies the inline transaction for keyed commit recovery, conformance, proof evidence, and diagnostics; it is not a reference to another declaration.

### Inline direct effect

> An `execute_effect` step declares one logical effect contract and one concrete execution site. Reaching the step constructs the concrete effect instance according to `values` and executes it. `effect_id` identifies the inline effect occurrence; it is not a lookup into an operation-level effect registry.

### Explicit effect-intent binding

> An `establish_effect_intent` transaction step declares an effect contract, constructs one concrete logical effect instance from `values`, and atomically establishes that captured instance as the `EffectIntent` artifact named by `bind`. `execute_effect_intent` later consumes the definitely available binding and executes the exact captured instance without recomputing its values.

### Transaction-output binding

> An `establish_transaction_output` step declares the output's binding, schema, and value derivation at its production site. Successful transaction commit establishes the typed logical value under that binding. Natural replay may reconstruct it; keyed commit recovery may restore the exact original artifact.

### Transition effect-intent binding

> A state-machine transition continues to own its side-effect contracts. Each transaction application of that transition supplies, for every side effect, the concrete instance derivation and an operation-local intent binding. Successful commit atomically applies the state transition and establishes those intents. No separate operation-level intent declaration is required.

### Typed bindings

> Execution sites introduce immutable semantic bindings. Every binding has one producer and may be consumed only after that producer and within the binding kind's scope. Transaction reads are transaction-local observations; transaction outputs and effect intents are transaction artifacts governed by transaction replay/recovery; effect results are attempt-local observations governed by effect-result replay rules. A binding is not mutable storage or a durability guarantee.

### Binding availability

> A producer existing somewhere in the operation does not make its binding globally available. A binding may be consumed only where every falling-through path to the consumer has produced it, subject to stronger binding-kind scope. Transaction reads never leave their transaction. Result variant payloads remain local to their corresponding `match_result` arm.

---

## 30. Acceptance criteria

The revision is complete when:

1. `Operation` has no `effects`, `effect_intents`, `transaction_outputs`, or `transactions` fields.
2. `OperationStep::Transaction` contains the complete transaction.
3. `RunTransaction` no longer exists.
4. Direct effect contracts live inside `ExecuteEffect`.
5. Explicit intent contracts live inside `EstablishEffectIntent`.
6. Transaction-output schema and derivation live at the output binding site.
7. Transition applications bind their implicit intents directly.
8. `Model.state_machines` and transition side-effect contracts remain shared model-level declarations.
9. read/output/intent/result producers use binding-oriented names.
10. the analyzer derives transaction/effect/binding indexes from the program.
11. existing replay and requirement semantics retain their verdicts after fixture migration.
12. no semantic declaration map is reintroduced merely as analyzer convenience.

---

## 31. Summary

Before:

```text
Operation
├── effects
├── effect_intents
├── transaction_outputs
├── transactions
└── program
      ├── references transaction
      ├── references effect
      └── references intent
```

After:

```text
Operation
└── program
      |
      +-- inline transaction {
      |      read -> bind transaction-local observation
      |      output -> bind exported data artifact
      |      intent -> bind captured effect artifact
      |      transition -> bind transition-owned intents
      |   }
      |
      +-- inline direct effect -> optional Result binding
      |
      +-- execute bound intent -> optional Result binding
      |
      +-- match / branch
      |
      +-- return / complete
```

Shared declarations remain only where they represent contracts independent of a particular execution site.

> **The operation program declares the causal things that happen. Bindings name the values and artifacts those happenings produce. Shared registries are reserved for genuinely shared domain contracts.**
