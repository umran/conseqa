# Conseqa Asynchronous Effect Execution Revision Specification

## 1. Purpose

This revision introduces explicit asynchronous effect execution into the L0 Conseqa operation program.

The extension allows an operation to:

- initiate multiple effects without waiting for each effect to complete;
- permit those effect executions to overlap;
- wait for all selected asynchronous effects with `join_all`;
- wait for the first selected asynchronous effect to complete with `race`;
- consume synchronous effect results only after an appropriate synchronization point.

The purpose is to expose logical effect concurrency and synchronization while keeping the application program deliberately abstract.

The revision does **not** turn Conseqa into a general concurrent programming language.

---

# 2. Motivation

The existing operation program expresses sequential causal execution:

```text
Execute A
Execute B
Execute C
```

which establishes:

```text
complete(A)
    ->
start(B)
    ->
complete(B)
    ->
start(C)
```

For latency analysis, this implies a sequential critical path.

Many applications instead perform:

```text
start A
start B
join_all(A, B)
C
```

where A and B may overlap.

The resulting logical latency structure is approximately:

```text
       A --------\
                  -> join -> C
       B --------/
```

and therefore:

```text
T ≈ max(T_A, T_B) + T_C
```

rather than:

```text
T ≈ T_A + T_B + T_C
```

Similarly:

```text
start A
start B
race(A, B)
C
```

places C behind the first completed candidate rather than behind both.

These distinctions materially affect:

- critical-path analysis;
- tail-latency simulation;
- downstream fan-out;
- load amplification;
- request hedging;
- causal effect ordering;
- retry analysis;
- effect blast-radius reasoning.

They therefore belong in the L0 abstract application machine.

---

# 3. Layering

Asynchronous effect execution is an **L0 program semantic**.

It describes intentional overlap between logical effects belonging to one operation invocation.

It is distinct from L1 execution concurrency.

```text
L0 async execution
    =
overlap between effects caused by
one logical operation invocation
```

whereas:

```text
L1 ExecutionPool.member_concurrency
    =
concurrent logical runtime work
admitted to one execution member
```

Neither implies the other.

An `ExecutionPool` with:

```text
member_concurrency = bounded(1)
```

may execute one operation invocation at a time while that invocation itself has several asynchronous downstream effects in flight.

Conversely, a member may execute many operation invocations concurrently even if every individual invocation's L0 program is entirely sequential.

---

# 4. Design principles

The revision follows these principles.

### 4.1 Effects, not arbitrary program blocks, are asynchronous

The initial model permits asynchronous execution of:

```text
direct Effect instances
EffectIntent instances
```

It does not permit:

```text
async Transaction
async OperationBlock
async branch
async arbitrary program fragment
```

This prevents the program language from acquiring general shared-state concurrency semantics.

### 4.2 Existing effect contracts are reused

An asynchronous direct effect uses the same:

```text
Effect
Derivation
effect_id
```

semantics as `execute_effect`.

An asynchronous effect-intent execution uses the same captured instance semantics as `execute_effect_intent`.

There is no:

```text
AsyncEffect
```

contract type.

Async is an execution mode, not a new kind of effect.

### 4.3 Synchronization is explicit

Program order after an asynchronous launch does not imply that the launched effect has completed.

Completion becomes causally established only through synchronization such as:

```text
join_all
race
```

or through some future explicit synchronization primitive.

### 4.4 Async handles are semantic synchronization artifacts

An asynchronous launch introduces an operation-local handle identifying that particular in-flight effect execution.

The handle:

- is not application data;
- has no schema;
- cannot be persisted;
- cannot be used as an idempotency key;
- cannot be returned from an operation;
- cannot be placed in a transaction;
- does not identify the logical effect itself.

It identifies one asynchronous execution occurrence for later synchronization.

### 4.5 `race` does not imply cancellation

Waiting for the first completion does not establish anything about the remaining executions being cancelled, rolled back, prevented, or harmless.

Every launched candidate remains part of the operation's side-effect blast radius.

---

# 5. New operation-program steps

The initial extension adds four step kinds:

```text
execute_effect_async
execute_effect_intent_async
join_all
race
```

Thus the operation-program vocabulary becomes conceptually:

```text
transaction
execute_effect
execute_effect_async
execute_effect_intent
execute_effect_intent_async
join_all
race
match_result
branch
return
complete
```

No loop or general task primitive is introduced.

---

# 6. `execute_effect_async`

Conceptually:

```rust
pub struct ExecuteEffectAsync {
    pub handle: Id,
    pub effect_id: Id,
    pub effect: Effect,
    pub values: Derivation,
}
```

Reaching:

```text
ExecuteEffectAsync(H, effect_id, E, D)
```

does the following:

1. declares the inline effect contract `E`;
2. constructs one concrete logical effect instance according to `D`;
3. initiates execution of that instance;
4. establishes asynchronous handle `H`;
5. permits operation control to proceed without waiting for the effect execution to complete.

The effect contract and instance construction semantics are otherwise identical to `execute_effect`.

---

# 7. Asynchronous launch semantics

Consider:

```text
execute_effect_async A
B
```

The program establishes:

```text
start(A) < start(B)
```

where `start(B)` means whatever logical action the following step performs.

It does **not** establish:

```text
complete(A) < start(B)
```

Therefore A may overlap B.

Conceptually:

```text
time ----->

A:  [---------------------]

B:        [---------]
```

The exact scheduling mechanism is not modeled.

Conseqa does not assert:

```text
thread creation
task creation
future implementation
event-loop semantics
CPU parallelism
OS scheduling
```

It asserts only that the causal program does not require A to complete before subsequent control proceeds.

---

# 8. `execute_effect_async` versus `execute_effect`

Sequential:

```text
execute_effect A
execute_effect B
```

establishes:

```text
complete(A) < start(B)
```

Asynchronous:

```text
execute_effect_async A -> H
execute_effect B
```

establishes only:

```text
start(A) < start(B)
```

No completion relationship follows unless later synchronization establishes one.

The two primitives therefore represent materially different application semantics.

---

# 9. Stable effect IDs

`execute_effect_async.effect_id` has exactly the existing meaning of `execute_effect.effect_id`.

It identifies the inline logical effect occurrence for:

```text
value lineage
idempotency analysis
proof evidence
diagnostics
visualization
effect-cascade analysis
```

The asynchronous handle is separate:

```text
effect_id
    =
stable identity of the inline effect occurrence

handle
    =
operation-local synchronization reference
to this asynchronous execution
```

They must not be conflated.

---

# 10. Direct asynchronous effect instance provenance

`values` has exactly the existing `Derivation` semantics.

It is evaluated at the point where `execute_effect_async` is reached.

The effect instance is therefore fully determined at launch time.

Later changes in control or observations cannot change the launched effect's payload.

Conceptually:

```text
launch:
    evaluate D
    construct E(D)
    start exact E(D)

later:
    synchronization observes completion
```

Synchronization never reevaluates `D`.

---

# 11. No result binding at launch

`execute_effect_async` does not directly bind an effect result.

At launch time, the synchronous result does not yet necessarily exist.

Therefore this SHALL be invalid:

```text
execute_effect_async:
    ...
    bind: result.foo
```

if `bind` is interpreted as the existing immediately available effect-result binding.

Instead, results become available through synchronization primitives.

This prevents:

```text
start effect A
use result(A) before A completed
```

from being representable.

---

# 12. `execute_effect_intent_async`

Conceptually:

```rust
pub struct ExecuteEffectIntentAsync {
    pub intent: Id,
    pub handle: Id,
}
```

The step consumes a definitely available `EffectIntent` binding.

Reaching:

```text
ExecuteEffectIntentAsync(I, H)
```

does the following:

1. resolves the exact logical effect instance captured by `I`;
2. initiates execution of that exact instance;
3. establishes asynchronous handle `H`;
4. permits operation control to continue without waiting for execution completion.

There is no derivation.

The intent's effect instance was fixed when the intent was established.

---

# 13. EffectIntent authority

`execute_effect_intent_async` is an additional execution authority for an `EffectIntent`.

The relationship becomes:

```text
EffectIntent I
      |
      +-- execute_effect_intent
      |       synchronous execution
      |
      +-- execute_effect_intent_async
              asynchronous execution
```

Intent establishment alone still does not execute the underlying effect.

The async primitive does not alter EffectIntent durability, replay, or recovery semantics.

---

# 14. Async handle

Conceptually:

```rust
pub struct AsyncEffectHandle {
    // semantic identity represented by its binding Id
}
```

A handle refers to exactly one asynchronous effect execution occurrence.

It carries enough semantic information for validation to recover:

```text
effect contract
result contract, if any
launch site
effect_id, where applicable
```

The public DSL need not serialize this metadata separately.

The handle binding identifies the launch site that produced it.

---

# 15. Handle availability

Handles follow the normal operation-local definite-availability discipline.

For example:

```text
branch:
    then:
        execute_effect_async A -> H
    otherwise:
        ...

join_all(H)
```

is invalid because `H` is not definitely available on every path reaching `join_all`.

As with existing bindings:

```text
no forward references
no rebinding
no shadowing
no implicit phi/merge
```

are introduced.

---

# 16. `join_all`

Conceptually:

```rust
pub struct JoinAll {
    pub handles: Vec<AsyncJoin>,
}

pub struct AsyncJoin {
    pub handle: Id,
    pub bind: Option<Id>,
}
```

Example:

```yaml
- kind: execute_effect_async
  handle: async.profile
  effect_id: effect.profile
  effect: ...
  values: ...

- kind: execute_effect_async
  handle: async.orders
  effect_id: effect.orders
  effect: ...
  values: ...

- kind: join_all
  handles:
    - handle: async.profile
      bind: result.profile
    - handle: async.orders
      bind: result.orders
```

---

# 17. `join_all` semantics

For handles:

```text
H1, H2, ..., Hn
```

`join_all` does not complete until every referenced asynchronous execution has completed.

Thus:

```text
complete(H1) < continuation
complete(H2) < continuation
...
complete(Hn) < continuation
```

No relative completion order among the handles is established.

For:

```text
start A
start B
join_all(A,B)
C
```

Conseqa may infer:

```text
complete(A) < start(C)
complete(B) < start(C)
```

but cannot infer:

```text
complete(A) < complete(B)
```

or:

```text
complete(B) < complete(A)
```

---

# 18. `join_all` does not serialize its members

`join_all` is a synchronization barrier.

It does not imply:

```text
A before B
B before A
A and B do not overlap
same execution resource
same thread
same runtime member
```

Its only relevant ordering guarantee is that the continuation follows completion of every joined execution.

---

# 19. Results from `join_all`

If a joined handle refers to a result-bearing effect, its `AsyncJoin` entry may bind that effect's `Result`.

For example:

```text
join_all:
    H_request -> result.request
    H_external -> result.external
```

Each resulting binding has exactly the same result contract it would have had under ordinary synchronous effect execution.

A publication or other effect with no synchronous result must not declare a result binding.

A result-bearing effect may be joined without a binding when the result is deliberately ignored.

---

# 20. Join result identity

`join_all` does not construct a new combined result.

It simply makes each selected completed effect result independently available.

Thus:

```text
join_all(A, B)
```

does not produce:

```text
Result<(A,B), ...>
```

or any other aggregate application value.

Instead:

```text
result.A
result.B
```

remain independent bindings.

Conseqa therefore requires no tuple algebra, collection result type, or combined error model.

---

# 21. Result matching after `join_all`

After:

```text
join_all:
    H -> result.H
```

ordinary existing control may use:

```text
match_result result.H
```

with exactly the existing `Ok`/`Err` semantics.

For several result-bearing effects:

```text
join_all:
    A -> result.A
    B -> result.B
```

the program may inspect them in whatever subsequent causal order it declares.

`join_all` itself does not define application policy for combining multiple successes or failures.

---

# 22. `join_all` waits for all results, including errors

An `Err` result is still a completed effect interaction.

`join_all` therefore does not short-circuit merely because one result-bearing effect returns `Err`.

For:

```text
A -> Err
B -> still running
```

`join_all(A,B)` continues waiting for B.

After both complete, their individual results may be inspected.

A future "fail-fast" synchronization primitive would require separate semantics and is not implied by `join_all`.

---

# 23. `join_all` over one handle

A `join_all` containing one handle is valid.

It acts as the minimal await operation:

```text
execute_effect_async A -> H
...
join_all(H)
```

No separate `await_effect` primitive is required initially.

An empty `join_all` is invalid.

---

# 24. `race`

Conceptually:

```rust
pub struct Race {
    pub handles: Vec<Id>,
    pub bind: Option<Id>,
}
```

Example:

```yaml
- kind: execute_effect_async
  handle: async.primary
  effect_id: effect.primary
  effect: ...
  values: ...

- kind: execute_effect_async
  handle: async.hedge
  effect_id: effect.hedge
  effect: ...
  values: ...

- kind: race
  handles:
    - async.primary
    - async.hedge
  bind: result.read
```

---

# 25. `race` completion semantics

For:

```text
race(H1, H2, ..., Hn)
```

the race completes when at least one referenced execution completes.

If `W` is the winning completion:

```text
complete(W) < continuation
```

Conseqa does not establish that any other handle has completed.

Therefore:

```text
start A
start B
race(A,B)
C
```

may have:

```text
A ----------->
B -----|
       |
       +--> C
```

if B completes first.

---

# 26. Race winner

The initial `race` primitive does not expose the identity of the winning handle as a first-class application value.

This avoids introducing:

```text
handle comparison
handle matching
dynamic task identity
winner-dispatch control
```

into the L0 expression language.

Where a race binds a result, the continuation observes the winner's result rather than its handle identity.

---

# 27. Race result compatibility

If `race.bind` is present, every raced handle must correspond to a result-bearing effect with the same logical result contract.

Conceptually:

```text
ResultType(H1)
=
ResultType(H2)
=
...
=
ResultType(Hn)
```

The bound result is the exact result produced by whichever candidate completes first.

This naturally supports cases such as hedged requests where several alternatives provide the same logical result shape.

If the handles have incompatible result contracts, `bind` is invalid.

---

# 28. Result-less race

A `race` without `bind` may synchronize result-less effects or heterogeneous effect types.

Its semantic meaning is simply:

> Continue after the first referenced asynchronous execution completes.

No application value identifying that completion becomes available.

---

# 29. Race is first completion, not first success

`race` means:

```text
first completed execution
```

not:

```text
first Ok
first successful external mutation
first non-error response
```

Therefore, if:

```text
A completes first with Err
B later completes with Ok
```

then a result-binding race observes A's `Err`.

A future primitive such as `first_ok` would require distinct semantics and is not part of this revision.

---

# 30. Race does not cancel losing effects

This rule is normative.

For:

```text
race(A,B)
```

if A completes first, Conseqa SHALL NOT infer:

```text
B cancelled
B aborted
B did not execute
B cannot later complete
B cannot produce side effects
```

The only established fact is:

```text
A completed first
```

or, more generally, that one candidate completed before race continuation.

Every candidate was already initiated and therefore remains part of the operation's possible side-effect cascade.

A concrete implementation may attempt to cancel losing work.

Conseqa provides no cancellation guarantee unless a future explicit primitive introduces one.

---

# 31. Race and load analysis

Because losing executions remain potentially active, a downstream simulator must account for their work.

For a hedged request:

```text
start primary
start hedge
race(primary, hedge)
```

the response critical path may approximate:

```text
min(T_primary, T_hedge)
```

while generated load may still include:

```text
primary attempt
+
hedge attempt
```

This distinction is intentional.

The latency benefit of racing must not silently erase its resource or side-effect cost.

---

# 32. Race and later joins

`race` does not consume or invalidate its handles.

A later synchronization may still wait on them:

```text
start A -> HA
start B -> HB

race(HA, HB)

C

join_all(HA, HB)

D
```

The first barrier establishes:

```text
one of A/B completed before C
```

while the second establishes:

```text
A completed before D
B completed before D
```

If one candidate had already completed, joining it again requires no new effect execution.

Synchronization never re-executes an effect.

---

# 33. Operation termination with unresolved asynchronous effects

An operation terminal does not implicitly join asynchronous executions.

Therefore:

```text
execute_effect_async A -> H
complete
```

is structurally meaningful.

It establishes:

```text
start(A) < complete(operation_control)
```

but not:

```text
complete(A) < complete(operation_control)
```

The asynchronous effect remains part of the invocation's side-effect blast radius.

No durability or eventual-completion guarantee follows merely because it was launched.

This allows Conseqa to represent deliberate fire-and-forget behavior without adding another primitive.

---

# 34. Terminal result latency

For a request operation:

```text
start A
start B
race(A,B) -> R
return R
```

the request result is causally dependent only upon the race winner.

The loser may remain in flight beyond the request terminal.

A simulator may therefore distinguish:

```text
request latency
```

from:

```text
lifetime/resource cost of all effects initiated by the request
```

This is a major reason for representing asynchronous execution explicitly.

---

# 35. Operation success does not imply async-effect success

If an operation reaches:

```text
return
```

or:

```text
complete
```

with unresolved asynchronous handles, Conseqa SHALL NOT infer that those effects:

```text
completed
succeeded
failed
were cancelled
will eventually complete
```

The terminal describes completion of the operation's declared synchronous control path only.

---

# 36. Interaction with source acknowledgement

Where a source-driven input uses:

```text
acknowledge_on_success = true
```

successful operation completion may occur while asynchronously launched effects remain unresolved.

For example:

```text
execute_effect_async E
complete
```

may establish:

```text
source invocation completes successfully
source item becomes acknowledged
```

without establishing:

```text
E completed
```

This is an intentional and observable application-semantic distinction.

Conseqa must not silently turn source acknowledgement into an implicit async join.

---

# 37. Interaction with EffectIntent recoverability

For:

```text
execute_effect_intent_async I -> H
complete
```

the intent has been used to initiate its captured effect.

The operation terminal does not establish successful completion of that underlying effect.

Likewise, recovery of `I` does not make repeated async execution safe.

Existing effect-level idempotency and retry analysis continues to apply.

---

# 38. Effect idempotency analysis

An asynchronously launched effect participates in idempotency analysis as soon as its launch site is reachable.

The analyzer must conservatively treat:

```text
execute_effect_async E
```

as an effect attempt that may produce the same externally relevant consequences as synchronous:

```text
execute_effect E
```

The fact that the operation does not wait for completion does not remove E from its side-effect blast radius.

Therefore:

```text
Operation
    |
    +-- async E1
    +-- async E2
```

has both E1 and E2 in its transitive effect cascade.

---

# 39. Async execution does not add idempotency guarantees

None of:

```text
async launch
join_all
race
```

implies:

```text
deduplication
exactly-once execution
retry suppression
replay stability
effect-result consistency
```

Those facts remain governed by the existing effect, request, external-boundary, transaction, message-identity, and idempotency semantics.

---

# 40. Replay of asynchronous launch

On operation retry, the declared program is traversed again.

If an `execute_effect_async` launch site is encountered again, the effect may be initiated again.

Whether this repeated attempt is safe is analyzed using exactly the same effect-instance stability and downstream idempotency rules as a synchronous execution.

Asynchronous launch is not a durable checkpoint.

---

# 41. Direct async effect class-fixity

The concrete effect instance created by:

```text
execute_effect_async(effect_id, E, D)
```

is class-fixed for a governing retry class only when the existing derivation and replay-stability rules establish that `D` yields the same logical instance for every attempt in that class.

Async execution does not weaken this requirement.

---

# 42. EffectIntent replay

An `execute_effect_intent_async` execution uses the same exact captured effect instance represented by its intent.

Natural reconstruction or keyed-commit recovery of the intent follows existing EffectIntent rules.

Synchronization adds no new recovery route.

---

# 43. Result replay after `join_all`

A result binding produced by joining one asynchronous effect has the same logical result as that effect execution would have exposed synchronously.

Its replay stability therefore depends on the same underlying facts:

```text
effect instance stability
target result-replay guarantees
external deduplication guarantees
error disposition
```

The presence of other effects in the same `join_all` does not itself alter that result's replay semantics.

---

# 44. Race result replay

A race result introduces an additional source of nondeterminism:

```text
which candidate completes first
```

Even if each candidate independently has replay-stable results, retries need not select the same winner.

Therefore the initial verifier SHALL conservatively treat a result binding produced by `race` as **not established to be replay-stable**.

A future verifier may prove race-result replay stability when it can establish that every possible winner yields replay-equivalent observable results.

V1 need not perform that equivalence proof.

Consequently, a `match_result` or other replay-sensitive decision based on a race result may become an explicit replay obstacle.

This is preferable to silently assuming deterministic scheduling.

---

# 45. Causal ordering

The existing sequential operation program establishes happens-before relationships.

Async launch weakens only the relevant completion edge.

For synchronous:

```text
A
B
```

the causal relation includes:

```text
complete(A) < start(B)
```

For asynchronous:

```text
async A
B
```

it includes:

```text
start(A) < start(B)
```

but not:

```text
complete(A) < start(B)
```

A later:

```text
join_all(A)
C
```

adds:

```text
complete(A) < start(C)
```

The analyzer may therefore lower the logical operation to a partial-order execution graph.

---

# 46. Program paths remain acyclic

The synchronous control structure remains acyclic.

Async execution introduces concurrent edges but no loops.

Thus an operation may be represented conceptually as:

```text
control-flow DAG
+
effect-completion dependency edges
```

rather than requiring a general cyclic concurrent state machine.

The existing prohibition on iteration remains unchanged.

---

# 47. Critical-path graph

For simulation purposes:

```text
execute_effect_async A -> HA
execute_effect_async B -> HB
join_all(HA, HB)
execute_effect C
```

lowers approximately to:

```text
          A
        /   \
start -     join -> C
        \   /
          B
```

The simulator may attach quantitative latency distributions to A, B, and C.

Conseqa supplies the dependency graph, not those quantitative distributions.

---

# 48. Race critical path

For:

```text
execute_effect_async A -> HA
execute_effect_async B -> HB
race(HA, HB)
C
```

the synchronization condition is:

```text
first(complete(A), complete(B))
        ->
start(C)
```

rather than:

```text
complete(A)
AND
complete(B)
        ->
start(C)
```

The simulator may therefore model race latency using the distribution of the first completion while still accounting for both effect attempts.

---

# 49. Tail-latency analysis

The revised semantics allow downstream simulation to distinguish:

```text
sequential fan-out:
    A + B + C
```

from:

```text
parallel join:
    max(A,B) + C
```

from:

```text
race:
    min(A,B) + C
```

subject to actual launch times, runtime contention, and effect-specific failure behavior.

This allows latency distributions to emerge from the declared logical execution structure rather than from an inaccurate assumption that all effects execute serially.

---

# 50. L0 versus L1 contention

L0 says only that effects are eligible to overlap.

It does not guarantee that concrete runtime resources allow perfect physical parallelism.

For example:

```text
async RequestEffect A
async RequestEffect B
join_all
```

declares logical overlap.

L1 and simulation may reveal that both requests eventually contend for:

```text
the same ExecutionPool
the same storage partition
the same external capacity
```

and therefore experience queueing or reduced effective parallelism.

The correct analysis pipeline is:

```text
L0 logical parallelism
        +
L1 runtime topology
        +
simulation resource/capacity model
        =
quantitative latency behaviour
```

---

# 51. No transaction concurrency primitive

This revision does not permit:

```text
transaction_async
async transaction
join transactions
race transactions
```

Transactions retain their existing inline atomic semantics.

If two independent transactions need to execute concurrently within one operation in the future, that requires a separate design because transaction-produced artifact visibility and conflicting persistent-state access would need explicit semantics.

---

# 52. No asynchronous OutboxWriteEffect

Under the companion Outbox revision, an `OutboxWriteEffect` is executable only as a transaction step.

It therefore cannot be launched through:

```text
execute_effect_async
```

or captured/executed asynchronously through an `EffectIntent`.

Its atomic admission semantics remain part of its containing transaction.

This restriction follows from its effect kind, not from async machinery itself.

---

# 53. Allowed direct async effect kinds

Initially, `execute_effect_async` may execute exactly those Effect kinds legal for ordinary operation-level direct execution.

Under the pre-Outbox effect model these are:

```text
PublicationEffect
RequestEffect
ExternalEffect
```

If the Effect enum later grows, each effect kind must explicitly declare whether direct asynchronous execution is legal.

No new effect kind becomes async-capable merely because it implements the common `Effect` enum.

---

# 54. `join_all` and publication effects

A `PublicationEffect` has no synchronous result.

It may nevertheless have modeled completion.

Thus:

```text
async publish A -> HA
async publish B -> HB
join_all(HA, HB)
C
```

establishes that both publication attempts have completed before C begins.

No result bindings are produced.

This is useful when subsequent work must occur only after multiple publications complete.

---

# 55. `race` and publication effects

Result-less effects may participate in a race without a result binding.

For:

```text
race(HA, HB)
```

the continuation follows the first completed publication attempt.

No claim is made about the other publication.

This is semantically valid even if it is uncommon.

---

# 56. Async request effects

A `RequestEffect` retains its existing request-target and retry semantics.

Asynchronous execution changes only causal blocking.

For:

```text
async request A
async request B
join_all
```

the downstream target operations may overlap.

The request-result schemas remain inherited from their respective target inputs.

`join_all` may bind their results after both complete.

---

# 57. Async external effects

An `ExternalEffect` retains its existing:

```text
idempotency
result
```

contract.

Asynchronous launch provides no stronger fact about the external system.

A deduplicated external effect remains deduplicated according to its declared key.

An unspecified external effect remains unspecified.

---

# 58. `race` compatibility for requests and external effects

A result-binding race may mix different effect kinds only if they expose exactly the same logical result contract.

For example, racing:

```text
RequestEffect -> Result<X,Y>
ExternalEffect -> Result<X,Y>
```

is structurally possible if their contracts are identical.

Conseqa does not infer that they have equivalent business behavior merely from result-shape equality.

The author is responsible for declaring a meaningful architecture.

---

# 59. No implicit cancellation or rollback

No synchronization primitive in this revision performs:

```text
effect rollback
transaction rollback
external compensation
cancellation guarantee
undo
```

`race` especially must not be read as:

```text
race = execute winner only
```

All candidates are launched.

All are therefore potential side effects.

---

# 60. Bindings produced by `join_all`

A result binding produced at `join_all` enters the ordinary operation artifact/result context after that synchronization step.

Conceptually:

```text
Available(after join_all)
    =
Available(before)
∪ ResultsBoundBy(join_all)
```

Those result bindings then obey all existing:

```text
match_result
variant scope
definite availability
value-reference
replay
```

rules.

---

# 61. Binding produced by `race`

Where a race declares:

```text
bind = result.race
```

that result becomes available immediately after the race synchronization completes.

It obeys the normal result-binding contract and `match_result` rules.

Its producer is the race step, not any one statically identifiable effect launch.

The analyzer internally records the candidate handle set as its possible producers.

---

# 62. Handle bindings and ordinary bindings

Async handles introduce a new binding kind:

```text
AsyncEffectHandle
```

alongside existing operation-local semantic bindings such as:

```text
TransactionOutput
EffectIntent
EffectResult
```

A handle has one syntactic producer.

It is immutable and operation-local.

It does not become a new `ValueSource`.

Application expressions cannot inspect a handle.

Only synchronization primitives may consume it.

---

# 63. Handle identity

Every async handle ID must be unique within the operation and participate in the existing global ID uniqueness discipline.

An effect launch therefore has two independently meaningful names:

```text
effect_id
handle
```

For example:

```yaml
effect_id: effect.fetch_profile
handle: async.fetch_profile
```

The names may resemble one another for author convenience but have different semantics.

---

# 64. Handle use does not imply unique effect execution

One handle represents one execution occurrence produced by one traversal of that launch site.

On operation retry, the same logical launch site may be encountered again.

The handle declaration does not deduplicate these executions across retries.

It is an invocation-local synchronization artifact.

---

# 65. `join_all` validation

Validation SHALL require:

1. the handle list is non-empty;
2. every referenced handle exists;
3. every referenced handle is definitely available at the join site;
4. no handle appears twice within the same `join_all`;
5. every declared result binding is unique;
6. a result binding may be declared only for a result-bearing underlying effect;
7. the bound result type is inferred from the underlying effect contract and never restated.

---

# 66. `race` validation

Validation SHALL require:

1. at least two handles;
2. every referenced handle exists;
3. every handle is definitely available at the race site;
4. no handle occurs twice in one race;
5. if `bind` is absent, no result compatibility requirement is imposed;
6. if `bind` is present, every handle is result-bearing;
7. if `bind` is present, every candidate has the same logical result contract;
8. the race result binding ID is unique.

---

# 67. Async launch validation

`execute_effect_async` SHALL apply every structural validation rule already applicable to synchronous `execute_effect`, including:

```text
effect target resolution
schema compatibility
derivation validity
value-reference scope
idempotency-key reference validity
effect_id uniqueness
result-contract resolution
```

plus:

```text
handle uniqueness
effect kind permits direct async execution
```

---

# 68. Async EffectIntent validation

`execute_effect_intent_async` SHALL require:

```text
intent binding exists
intent definitely available
handle ID unique
```

and SHALL recover the underlying effect contract in order to determine result compatibility for later synchronization.

---

# 69. Result-binding validation changes

Existing:

```text
EffectResultNotBound
```

analysis SHALL be extended so that a result from an asynchronous effect is not considered bound merely because its launch occurred.

A result becomes available only when produced by:

```text
join_all
```

or:

```text
race
```

under the rules above.

This is fundamental to async soundness.

---

# 70. Program termination

Existing termination rules remain unchanged for the synchronous control structure.

Every reachable control path must still end in:

```text
return
complete
```

Outstanding asynchronous handles do not make an otherwise terminating control path unterminated.

They represent launched side-effect executions, not additional operation-control paths requiring terminals.

---

# 71. Program reachability

Ordinary program-step reachability remains defined over synchronous control.

An async effect may still be running after a terminal, but no new operation-program step executes after that terminal.

Thus:

```text
async A
complete
B
```

still makes B unreachable.

A continuing asynchronous A does not make B reachable.

---

# 72. Decision semantics

A decision cannot inspect whether an asynchronous handle has completed.

There is no:

```text
is_complete(H)
```

condition.

To make effect completion causally relevant, the program must synchronize first.

Similarly there is no:

```text
poll(H)
timeout(H)
```

in the initial revision.

---

# 73. No timeout primitive

Timeout is deliberately omitted initially.

A timeout introduces additional semantics involving:

```text
time
timer source
loser continuation
cancellation
timeout result
```

If latency or resilience analysis later requires explicit application-level deadlines, timeout should be introduced as a separate primitive rather than encoded through `race` against an implicit timer.

---

# 74. No cancellation primitive

Cancellation is deliberately omitted.

Conseqa must not infer cancellation from:

```text
race
operation terminal
branching away from a handle
failure of another effect
```

A future cancellation primitive would need to specify whether it guarantees:

```text
request not started
attempt interrupted
external side effect prevented
only local waiting abandoned
```

Those distinctions are too significant to leave implicit.

---

# 75. No async collection primitive

The initial revision introduces no collection of handles as an application value.

`join_all` and `race` receive a statically declared list of handle references.

Conseqa therefore does not need:

```text
Vec<Handle>
dynamic task creation
map_async
for_each_async
dynamic fan-out counts
```

The number and identity of modeled asynchronous effect sites remain structurally explicit.

---

# 76. Static fan-out

Conseqa can represent:

```text
async A
async B
async C
join_all(A,B,C)
```

because the fan-out structure is statically declared.

Dynamic fan-out such as:

```text
for every item in arbitrary runtime collection:
    launch request
```

remains outside the current abstract program.

This is consistent with the existing decision to defer general iteration.

---

# 77. Idempotency-key propagation

Idempotency-key propagation on an asynchronously executed effect has exactly its existing semantics.

Async execution does not alter:

```text
source key lineage
target key lineage
message identity
request identity
```

For example, two asynchronous publications each propagate keys normally into their respective topic messages.

The analyzer follows both downstream cascades.

---

# 78. Ordering requirements

Async execution must be visible to ordering verification.

For:

```text
async Effect A
async Effect B
join_all
```

textual launch order alone is insufficient to establish:

```text
effect(A) < effect(B)
```

in completion or externally observable effect order.

If an operation requirement depends on one effect completing or becoming externally visible before another, a causal synchronization edge must establish it.

Thus:

```text
execute_effect A
execute_effect B
```

and:

```text
execute_effect_async A
execute_effect_async B
join_all
```

are not interchangeable for ordering proofs.

---

# 79. `join_all` and ordering

`join_all` establishes only:

```text
all joined effects
    <
continuation
```

It does not establish ordering among the joined effects.

Therefore:

```text
async A
async B
join_all
C
```

supports:

```text
A < C
B < C
```

but not:

```text
A < B
```

or:

```text
B < A
```

---

# 80. `race` and ordering

`race` establishes:

```text
some candidate completion
    <
continuation
```

If the winner is not statically determined, the verifier must not infer that a particular candidate necessarily precedes the continuation.

For a continuation requiring A specifically:

```text
race(A,B)
C
```

does not prove:

```text
A < C
```

because B may win.

This matters for causal correctness analysis.

---

# 81. Serialization requirements

Async overlap may invalidate a serialization proof that depended on sequential execution within one invocation.

Conseqa must not assume that two asynchronously launched effects cannot overlap.

However, operation-level `SerializedBy(K)` requirements continue to concern separate logical operation invocations under their existing definition.

Async effects within one invocation are not automatically additional operation invocations.

The two domains must remain distinct.

---

# 82. Side-effect blast radius

Every asynchronously launched effect belongs to the operation's side-effect blast radius regardless of whether:

```text
it is joined
it wins a race
it loses a race
the operation terminates first
its result is ignored
```

A race loser is therefore never pruned from the effect graph merely because its result is not used.

---

# 83. Recoverability analysis

Async execution does not itself provide durable rediscovery.

For:

```text
async E
complete
```

the model says E was initiated.

It does not say that a crash during E causes the system to rediscover or retry that unfinished work.

Where durable rediscovery matters, the existing transaction/EffectIntent mechanisms or another explicitly modeled durable mechanism must provide it.

---

# 84. Visualization

A visualization SHOULD distinguish synchronous and asynchronous effect edges.

For example:

```text
Operation
   |
   +----async----> Effect A -----\
   |                              \
   +----async----> Effect B -------> JoinAll
                                  /
                                 /
                           continuation
```

A race SHOULD render as a first-completion barrier rather than an ordinary all-predecessor join.

Visualizations should make unresolved race losers visible rather than suggesting they disappear.

---

# 85. Simulation lowering

A downstream simulator may lower:

```text
execute_effect_async
```

into an effect-start event with no immediate completion dependency on the next operation step.

`join_all(H1...Hn)` creates a barrier requiring every corresponding completion event.

`race(H1...Hn)` creates a barrier enabled by the first corresponding completion event.

The simulation remains responsible for quantitative facts such as:

```text
latency distributions
network delays
resource queues
pool capacity
correlations
timeouts
failure probabilities
physical scheduling
```

Conseqa supplies only the causal structure.

---

# 86. Example — parallel fan-out

```yaml
program:
  steps:
    - kind: execute_effect_async
      handle: async.profile
      effect_id: effect.profile
      effect:
        kind: request
        ...
      values:
        kind: deterministic
        ...

    - kind: execute_effect_async
      handle: async.orders
      effect_id: effect.orders
      effect:
        kind: request
        ...
      values:
        kind: deterministic
        ...

    - kind: join_all
      handles:
        - handle: async.profile
          bind: result.profile
        - handle: async.orders
          bind: result.orders

    - kind: complete
```

Causal structure:

```text
            profile
          /         \
request --           --> join --> complete
          \         /
            orders
```

---

# 87. Example — result handling after fan-out

```text
async profile -> HP
async orders  -> HO

join_all:
    HP -> result.profile
    HO -> result.orders

match_result result.profile:
    ok:
        ...
    err:
        ...
```

The profile result cannot be matched before `join_all`.

The orders result becomes available independently at the same barrier.

---

# 88. Example — hedged request

```yaml
- kind: execute_effect_async
  handle: async.primary
  effect_id: effect.primary
  effect:
    kind: request
    ...
  values: ...

- kind: execute_effect_async
  handle: async.replica
  effect_id: effect.replica
  effect:
    kind: request
    ...
  values: ...

- kind: race
  handles:
    - async.primary
    - async.replica
  bind: result.read

- kind: match_result
  result: result.read
  ok:
    steps: [...]
  err:
    steps: [...]
```

Both requests are attempted.

The first completed result drives subsequent control.

The losing request is not known to have been cancelled.

---

# 89. Example — race followed by cleanup barrier

```text
async A -> HA
async B -> HB

race(HA, HB)
    |
    v
perform latency-sensitive continuation

join_all(HA, HB)
    |
    v
perform work requiring both attempts to have completed
```

This distinguishes:

```text
first-completion dependency
```

from:

```text
all-completion dependency
```

within one operation.

---

# 90. Example — fire-and-forget publication

```text
execute_effect_async PublishAudit -> H
return response
```

Conseqa establishes:

```text
PublishAudit initiated
before
response terminal
```

but does not establish:

```text
PublishAudit completed
before
response terminal
```

nor:

```text
PublishAudit will eventually complete
```

The publication remains in the side-effect blast radius.

---

# 91. Example — asynchronous recovered intent

```text
Transaction T:
    establish EffectIntent I

execute_effect_intent_async I -> H

perform other work

join_all:
    H

complete
```

The transaction establishes the exact intent.

The async execution initiates that captured instance.

The intervening work may overlap the effect.

The join establishes completion before the operation terminal.

---

# 92. Suggested Rust surface

Illustratively:

```rust
pub enum OperationStep {
    Transaction(Transaction),
    ExecuteEffect(ExecuteEffect),
    ExecuteEffectAsync(ExecuteEffectAsync),
    ExecuteEffectIntent(ExecuteEffectIntent),
    ExecuteEffectIntentAsync(ExecuteEffectIntentAsync),
    JoinAll(JoinAll),
    Race(Race),
    MatchResult(MatchResult),
    Branch(Branch),
    Return(Return),
    Complete,
}

pub struct ExecuteEffectAsync {
    pub handle: Id,
    pub effect_id: Id,
    pub effect: Effect,
    pub values: Derivation,
}

pub struct ExecuteEffectIntentAsync {
    pub intent: Id,
    pub handle: Id,
}

pub struct JoinAll {
    pub handles: Vec<AsyncJoin>,
}

pub struct AsyncJoin {
    pub handle: Id,
    pub bind: Option<Id>,
}

pub struct Race {
    pub handles: Vec<Id>,
    pub bind: Option<Id>,
}
```

Exact Rust organization may change to fit the existing modules.

The normative semantics take precedence over illustrative structure.

---

# 93. Analyzer state

The program analyzer conceptually gains an async-handle environment:

```text
PathContext
    TransactionOutput -> ...
    EffectIntent      -> ...
    EffectResult      -> ...
    AsyncHandle       -> underlying effect execution
```

A handle may be in one of the analysis states:

```text
available / launched
known completed
```

The DSL need not expose those states.

They are analyzer bookkeeping used to determine synchronization and result availability.

---

# 94. Definite availability extension

Conceptually:

```text
Available(after async launch H)
    =
Available(before) ∪ {H}
```

For:

```text
join_all(H1...Hn)
```

all referenced handles must already be definitely available.

Result bindings declared by the join enter availability after the join.

For:

```text
race(H1...Hn) -> R
```

R enters result availability after the race.

No individual candidate result becomes available merely because it participated in a race.

---

# 95. Path semantics revision

The statement:

> An invocation traverses one path through the program.

remains correct for synchronous program control, but SHALL no longer be interpreted as saying that every effect on that path completes sequentially.

A more precise statement is:

> An invocation traverses one acyclic synchronous control path. Asynchronous effect steps may initiate effect executions whose lifetimes overlap later control. Synchronization steps add explicit completion dependencies to that control path.

This preserves the existing path-based verifier architecture while admitting asynchronous effect lifetimes.

---

# 96. Step-order interpretation

Ordinary sequential step order establishes:

```text
control(step_i) < control(step_i+1)
```

For synchronous effect execution, completion is part of the step and therefore precedes the next step.

For asynchronous effect launch, only initiation is part of the launch step.

Its eventual completion is a separate event.

This distinction SHALL be normative.

---

# 97. Existing `execute_effect` remains unchanged

No existing synchronous effect execution semantics are weakened.

Authors who write:

```text
execute_effect
```

continue to receive the existing guarantee that the effect execution and any bound result occur before subsequent operation control.

Async behavior exists only where explicitly declared.

There is no inference that a runtime implementation may arbitrarily parallelize synchronous Conseqa steps.

---

# 98. Existing `execute_effect_intent` remains unchanged

Likewise:

```text
execute_effect_intent
```

remains synchronous.

The exact captured effect is executed and any declared result is available before following control.

Authors opt into non-blocking execution only through:

```text
execute_effect_intent_async
```

---

# 99. Compatibility

Existing Conseqa models remain semantically unchanged.

No existing:

```text
execute_effect
execute_effect_intent
```

is migrated to async execution.

No analyzer may infer asynchronous overlap from multiple adjacent synchronous effect steps.

The new semantics are entirely opt-in.

---

# 100. Required semantic-contract updates

The authoritative semantic contract should be updated in the following areas:

1. `Operation` — clarify that the causal program may contain explicit asynchronous effect lifetimes.
2. Stable execution-site IDs versus bindings — introduce async-handle bindings.
3. Effect results — state that async execution exposes results only at synchronization.
4. `EffectIntent` — add asynchronous execution as a legal execution authority.
5. `OperationBlock.steps` — add the four new step kinds.
6. `execute_effect` — explicitly contrast synchronous semantics with async launch.
7. `execute_effect_intent` — likewise.
8. Program validation — add handle availability and synchronization validation.
9. Result definite-assignment analysis — add `join_all` and `race` result producers.
10. Program paths — distinguish synchronous control paths from overlapping effect lifetimes.
11. Replay analysis — account for async launches and conservative race-result stability.
12. Idempotency analysis — enumerate async launches as ordinary reachable effect attempts.
13. Ordering analysis — distinguish launch order from effect-completion order.
14. Recoverability analysis — do not treat async launch as durable rediscovery.
15. Visualization — expose forks and synchronization barriers.
16. Simulation lowering — emit effect-start/completion dependency structure.

---

# 101. Required semantic distinctions

The implementation SHALL preserve:

| Concepts | Required distinction |
|---|---|
| `execute_effect` vs `execute_effect_async` | synchronous completion dependency vs asynchronous initiation |
| effect ID vs async handle | stable effect-site identity vs invocation-local synchronization artifact |
| launch vs completion | starting an effect does not imply it has completed |
| `join_all` vs serialization | barrier after all completions vs order/non-overlap among candidates |
| `race` vs cancellation | first-completion synchronization vs stopping losers |
| `race` vs first-success | first completion may be `Err` |
| async result vs handle | application result is unavailable until synchronization |
| L0 async vs L1 member concurrency | intra-invocation logical effect overlap vs runtime invocation capacity |
| operation terminal vs async completion | an operation may finish while launched effects remain unresolved |
| async launch vs durability | initiation provides no rediscovery guarantee |
| launch order vs effect order | textual async starts do not establish completion/external-effect ordering |

---

# 102. Non-goals for the initial revision

The revision deliberately does not introduce:

```text
general async blocks
async transactions
loops
dynamic fan-out
collections of handles as values
wait-next
completion queues
wait groups
timeouts
deadlines
cancellation
first-success
semaphores
mutexes
shared mutable operation state
task-local state
structured task scopes
scheduler priorities
CPU parallelism semantics
```

These may be reconsidered only when a concrete correctness or analysis use case requires them.

---

# 103. Acceptance criteria

The revision is complete when Conseqa can represent and analyze:

```text
async Effect A
async Effect B
join_all(A,B)
Effect C
```

and correctly infer:

```text
A and B may overlap

A completes before C
B completes before C

no A/B relative completion order is known
```

It must also represent:

```text
async Effect A
async Effect B
race(A,B)
Effect C
```

and infer:

```text
A and B are both initiated

at least one completes before C

neither particular candidate is guaranteed
to be the winner

the loser is not guaranteed cancelled
```

For result-bearing effects it must prevent result use before synchronization, bind independent results through `join_all`, and permit a compatible winner result through `race`.

The idempotency analyzer must include every launched effect in the operation's transitive side-effect cascade.

The ordering analyzer must not mistake async launch order for effect-completion order.

The replay analyzer must conservatively treat a race-bound result as unstable unless stronger future equivalence analysis proves otherwise.

The simulator must be able to lower the operation into a partial-order execution graph suitable for critical-path and latency analysis.

---

# 104. Normative summary

> `execute_effect_async` constructs and initiates the same logical effect instance that an ordinary `execute_effect` would construct, but it does not wait for that execution to complete. It binds only an operation-local asynchronous handle.

> `execute_effect_intent_async` initiates execution of the exact logical effect instance captured by an existing `EffectIntent` and similarly binds an asynchronous handle.

> An asynchronous handle is a synchronization artifact, not application data, persistent state, an effect identity, or a `ValueSource`.

> `join_all` waits for every referenced asynchronous execution to complete. It establishes completion-before-continuation edges for each joined handle, establishes no relative ordering among them, and may expose each result-bearing effect's ordinary `Result` under an independent binding.

> `race` waits for the first referenced asynchronous execution to complete. Where all candidates expose the same result contract, it may bind the winner's exact result. It means first completion, not first success, and provides no cancellation guarantee for losing executions.

> Every asynchronously launched effect remains part of the operation's side-effect blast radius regardless of whether it is subsequently joined, wins a race, loses a race, has its result ignored, or outlives the operation's synchronous terminal.

> An operation may reach `return` or `complete` with unresolved asynchronous effects. The terminal completes the declared synchronous application control; it neither joins nor cancels outstanding effects and provides no guarantee that they subsequently complete.

> Async execution is an L0 causal-program semantic. It is completely independent of L1 `ExecutionPool.member_concurrency`. L0 declares which logical effect executions may overlap; L1 and downstream simulation determine how runtime topology and resource contention affect that potential concurrency.

> The revision adds explicit fork/join structure sufficient for sincere critical-path and latency analysis without introducing general-purpose concurrent programming constructs.