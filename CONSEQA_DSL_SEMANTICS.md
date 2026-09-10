# Conseqa DSL Semantics

**Status:** Normative semantic contract for the DSL and the V1 verifiers — the single authoritative semantics document. The design drafts and revision documents that preceded it are retired; their normative content is consolidated here, and what they left open is §27.  
**Implementation namespace:** `src/spec/` (surface), `src/analyzer/` (validation and verification).

This document defines what an Conseqa declaration means, what it does **not** mean, and what a verifier may soundly infer from it. It is intentionally stricter than a field reference: the purpose is to prevent the analyzer, an LLM author, and a human reader from silently assigning different meanings to the same declaration.

---

## 1. Interpretation model

Conseqa describes a **logical architecture**, not a deployment manifest and not executable code.

### Two semantic layers

The model is hierarchical. A declaration belongs to exactly one of two layers:

| Layer | What it describes | Where it lives |
|---|---|---|
| **L0 — abstract application semantics** | The application machine: services, schemas, data models, topics as logical channels, state machines, operations, programs, transactions, effects, and correctness requirements. | The model root (§2–§23) |
| **L1 — runtime topology and realization semantics** | Selected facts about how that machine is realized: transport ordering, subscription delivery and dispatch, request routing, execution-resource topology and concurrency, storage partitioning. | `Model.runtime` (§10) |

The governing rule:

> **L0 says what application machine exists and what properties it requires. L1 says how invocations and persistent data are arranged in a particular runtime realization of that machine.**

L1 is **optional**. An L0-only model is complete, structurally valid, and analyzable; it simply has fewer facts from which its obligations can be discharged. Removing L1 from a valid model never makes it invalid — it only makes proofs that consumed runtime facts unavailable.

The analyzer reasons across both layers: an L0 obligation may well be discharged from L1 facts. Every proof therefore records its **scope** (§25.1): `l0_only` when no L1 fact was required, `runtime_dependent` when at least one was. A runtime-dependent proof holds of the declared realization and must be re-examined whenever that realization changes.

### A layer is not a correctness layer

Semantic layer and semantic category are independent. A declaration does not move to L1 merely because infrastructure implements it.

`isolation: serializable` is L0: Conseqa treats serializable transaction execution as an abstract primitive of the application machine and need not expose MVCC, SSI, or database lock managers. The same holds for explicit locks, transaction `deduplicated_by`, `RequestEffect.retry`, effect idempotency, message identity, request identity, and unique claims. Their implementation mechanisms lie below Conseqa's semantic floor.

Conversely, topic transport ordering and execution-member concurrency are L1 despite being invisible to a caller. **Correctness relevance does not determine semantic layer.**

### Semantic category

Independently of layer, a declaration belongs to one of three semantic categories:

| Category | Meaning | Examples |
|---|---|---|
| **Structural fact** | Describes what the modeled program can do or how entities relate. | operations, programs, effects, transactions, transaction outputs, result contracts, schemas, execution-pool identity |
| **Implementation guarantee / assumption** | A fact the model claims the implementation or external system provides. The verifier may rely on it, subject to implementation conformance. | topic transport ordering, delivery semantics, member assignment, member concurrency, transaction isolation, locks, effect idempotency, request/message identity |
| **Requirement / obligation** | A property the architecture says must hold. It is **not** a guarantee merely because it is declared. The verifier must prove it from facts and structure. | operation serialization, operation ordering, operation idempotency, result replay consistency, recoverability |

A structurally valid model is therefore not necessarily a safe model. Validation establishes that declarations are coherent and references are meaningful. Verification establishes whether the declared requirements follow from the declared facts and architecture.

### 1.1 `unspecified` is epistemic

Across the DSL, `unspecified` means:

> The model provides no fact from which the corresponding property may be inferred.

It does **not** mean that the property is false, and it does not mean the implementation is allowed to violate a requirement. It means the verifier must treat the fact as unknown.

For example:

- `ordering: unspecified` does not prove messages are unordered.
- `concurrency: unspecified` does not prove concurrent execution exists.
- `idempotency: unspecified` does not prove an external effect is non-idempotent.
- `isolation: unspecified` does not prove transactions are weakly isolated.

An unknown fact cannot be used as evidence for a proof.

### 1.2 Absence of a guarantee is not evidence of a violation

`unordered`, `unbounded`, and `not_deduplicated` are stronger negative declarations than `unspecified`:

- **Unordered** explicitly says no ordering guarantee is provided.
- **Unbounded** explicitly says no finite bound is declared.
- **NotDeduplicated** explicitly says duplicate executions are not deduplicated at that boundary.

Even these declarations describe guarantees, not necessarily observed runtime behavior. An unordered topic may happen to emit messages in order in one execution; the verifier simply may not rely on that.

### 1.3 Requirements are conditional on model conformance

Any proof produced by Conseqa is conditional on the real implementation satisfying the declarations used by the proof. A proof based on `serializable`, deterministic provenance, or `deduplicated_by`, for example, is invalid if the concrete implementation does not actually provide those semantics.

### 1.4 Canonical form and shorthand

Every declaration has a canonical form. That is the form serialization emits and the form tooling reads; shorthands are accepted on input and are never a second representation of the model.

A shorthand may compress a declaration only where it withholds no fact:

- a **total two-valued claim** may become a marker, as `optional` does (§4);
- a **re-spelling** of the same components may become a name, as a field path and a value source do (§4, §11);
- a **discriminant the shape already carries** may be dropped, as a selector value's does (§19).

A shorthand may never supply a value for a vocabulary carrying `unspecified`. §1.1 makes those declarations epistemic: a default would let silence be read as a fact, which is exactly what that section forbids.

Where two readings of a shorthand could collide — a schema named for a scalar, a field name containing a dot, a literal string spelling a value source — the shorthand is refused or reserved, and the canonical form states the declaration instead.

---

## 2. Model, revision, and IDs

### `Model`

`Model` is the root semantic object, in two layers:

**L0 — the abstract application machine**, directly on the root, so a model that describes only application behaviour stays concise:

- services,
- schemas,
- data models,
- topics,
- state machines,
- operations,
- and a revision.

**L1 — one runtime realization of that machine**, under `runtime` (§10). Optional: `runtime` may be absent entirely, and each of its collections may be omitted independently.

The collections describe one architecture snapshot.

### `Revision`

`revision` is an opaque numeric revision marker for the model.

The current DSL assigns no ordering, compatibility, migration, or version-negotiation semantics beyond its numeric identity. A verifier must not infer that revision `2` is semantically compatible with, derived from, or newer in any meaningful architectural sense than revision `1` unless surrounding tooling establishes that convention.

### `Id`

`Id` is a logical identifier, serialized as a string.

Conseqa uses one common ID type rather than entity-specific Rust ID types. The semantic kind of a reference is determined by context and structural validation.

IDs should be treated as stable logical names, not as runtime addresses, URLs, database keys, or deployment identifiers unless a higher layer explicitly gives them that meaning.

---

## 3. Services

### `Service`

A service is a logical ownership/grouping boundary for operations.

### `ServiceKind`

Current kinds are:

- `backend`
- `frontend`
- `worker`
- `job`

These are descriptive classifications only.

A service kind does **not** by itself imply:

- a process boundary,
- a network hop,
- a host or container,
- a trust boundary,
- a replica count,
- availability semantics,
- concurrency,
- transactional boundaries,
- or failure independence.

Those facts must come from other declarations if they matter to a proof.

---

## 4. Schemas and field paths

### `Schema::Canonical`

A canonical schema describes the logical shape of a value.

Its fields have a type and an `optional` flag.

#### `SchemaCompleteness::complete`

`complete` claims that the declaration describes the complete logical schema.

Subject to conformance, the verifier may treat a field absent from a complete schema as nonexistent.

#### `SchemaCompleteness::partial`

`partial` explicitly permits the real schema to contain undeclared fields.

The verifier may reason about declared fields, but it must **not** infer that undeclared fields do not exist.

This distinction matters when proving properties that depend on exhaustive field knowledge.

### `Schema::Fragment`

A fragment is a projection/aliasing view over another declared schema.

`source` identifies the source schema. `mapping` maps each fragment field name to a `FieldPath` in the source.

A mapping asserts semantic identity of the referenced value across the fragment boundary. It may therefore preserve value lineage even when a field is renamed.

A fragment does **not**:

- create a new independent value,
- create a new storage object,
- imply that unmapped source fields are absent,
- or establish ordering/idempotency by itself.

Fragment chains must remain acyclic and resolvable.

### `Field`

`optional: true` means the logical value may be absent. `optional: false` means the declared schema requires it.

This is a schema-shape claim, not a runtime availability/liveness claim.

### `TypeRef`

#### `scalar`

Declares one of the current primitive logical types:

`string`, `bool`, `int`, `float`, `decimal`, `uuid`, `timestamp`.

These are logical types. No storage width, precision, locale, timezone encoding, or wire representation is implied beyond what the scalar name itself requires.

#### `schema`

References another declared schema as the logical type.

#### `list`

Declares a collection of values of the nested type.

A list declaration does not itself imply uniqueness, sortedness, stable ordering across executions, bounded length, or set semantics.

#### Surface syntax

A field and its type may be declared in shorthand instead of the canonical map. The two forms mean exactly the same thing:

```yaml
fields:
  order_id: uuid                # scalar, required
  note: string?                 # scalar, optional
  customer: schema.Customer     # schema reference
  items: [schema.LineItem]      # list
  tags: "[string]?"             # optional list
```

The type grammar is `type := scalar-name | schema-id | "[" type "]"`, with an optional trailing `?` on the field as a whole.

- A name matching a scalar name is that scalar; every other name is a schema reference. A schema whose id collides with a scalar name, or ends in `?`, must be declared in the canonical map form.
- `?` states optionality of the *field*, not of a type, so it may not appear inside one. `[string?]` is an error; an optional list is written `"[string]?"`.
- A list shorthand holds exactly one element type.
- The shorthand may also be used for `ty` alone inside the canonical map, as in `ty: [schema.Tag]` with `optional: true`.
- A canonical schema with no prose may omit `description` rather than declaring an explicit null.

Compressing `optional` into a suffix is a syntactic convention, not a semantic default. Optionality is a total two-valued shape claim with no epistemic `unspecified` member, so the shorthand withholds no fact the canonical form states. Vocabularies that *do* carry `unspecified` — ordering, delivery, dispatch, idempotency, derivation — acquire no defaults, and §1.1 continues to govern them: a declaration left out of those is not a negative guarantee, so it may not be inferred from silence.

The shorthand is an authoring affordance. Serialization always emits the canonical map, so a serialized model remains a single explicit form for tooling.


### `FieldPath`

A field path identifies a nested value relative to a schema.

For example, `[customer, id]` means the `id` field nested under `customer`.

A `FieldPath` has meaning only relative to the schema of its containing declaration or value source.

#### Surface syntax

The canonical form is the sequence of components. A dotted name says the same thing, and is how a path is rendered back to the author — diagnostics name `customer.id` — so the shorthand lets a path be written the way it will be read:

```yaml
path: customer.id

path:
  - customer
  - id
```

A component containing a `.` must use the sequence form. A path with no components remains writable as an empty sequence, and remains a validation error: whether a path resolves is asked of the schema, not of the surface syntax.

---

## 5. Data models and persistent objects

### `DataModel`

A data model is a **logical transactional state boundary** containing persistent objects.

It is not necessarily one database server, one vendor product, one schema namespace, or one physical storage engine. What matters is that a transaction declaring this data model is modeled as operating against this shared transactional boundary.

### `DataObject`

A data object is a logical class of persistent object instances.

`schema` identifies the canonical state schema for an instance.

### `identity`

`identity` is the complete, non-empty logical identity of one object instance.

The vector contains the components of a **single composite identity**.

For example:

```yaml
identity:
  - [tenant_id]
  - [account_id]
```

means the object is identified by the tuple:

`(tenant_id, account_id)`

It does **not** mean that `tenant_id` and `account_id` are alternative independent keys.

Object identity is what selector precision, insert uniqueness, alias and interference analysis, locking, state-machine subject identity, and transaction reasoning rest on. The declared identity is intrinsic to the logical object model: two distinct successfully created instances cannot share the same complete identity.

### Object-history requirements are deferred

A `DataObject` declares no requirements; in particular, no object-history requirement (such as a `linearizable` obligation) exists in the active DSL.

The reason is scope, not doubt about the property. Linearizability is a meaningful correctness property even for a single logical store, but an object-history requirement becomes provable only once the model exposes the facts it rests on — replica topology, authoritative write location, read routing, quorum and leader guarantees, propagation lag, partition and failure assumptions, real-time observation boundaries — and Conseqa does not yet model any of them. Keeping the requirement would have introduced an object-history proof domain with nothing to discharge it from.

Nothing else is weakened by the removal:

- transaction isolation, explicit locks, lock ordering, object identity, selector overlap, transaction conflicts, operation serialization, and operation ordering keep their declared meanings;
- `serializable` continues to mean transaction serializability under §17 and must **not** be reinterpreted as linearizability;
- no V1 verifier emits a verdict on object linearizability, and none infers it — from serializable isolation, locks, operation serialization, or transport ordering. Those facts retain only their own semantics.

The scope rule for this iteration is:

> Conseqa models transaction and operation correctness without declaring end-to-end persistent-object history consistency requirements.

Object-history requirements are to be reconsidered, as a coherent family rather than an isolated flag, when Conseqa begins modeling distributed persistence and availability. Their exact vocabulary is not predeclared here.

---

## 6. Topics as logical channels

A topic is an L0 declaration: it defines a logical message channel — which messages it may carry, and what identifies one of them.

It carries **no grouping or ordering fact**. How a transport groups a topic's messages, and what precedence it establishes among them, are properties of the realization rather than of the logical channel: the same channel may be realized grouped or not, ordered or not, and two subscribers of it may even differ. Those facts live in L1 (§10.2), at one of two scopes.

### `Topic.messages`

`messages` is the set of schemas that may be published to the topic.

Membership means the topic is allowed to carry that schema. It does not assert that such a message is ever published.

### `Topic.message_identity`

Message identity is a **guarantee provided by the topic's producers**, declaring where the identity of one logical message lives in the payload.

#### `unspecified`

No fact relates two carried messages sharing any field values.

#### `keyed`

For each mapped message schema, the mapping gives the ordered tuple of fields holding that schema's message identity. As with the ordering key, different schemas may map differently named fields into the same identity domain; tuple positions correspond across schemas, so all mapped tuples must have the same arity.

The guarantee is one statement over the mapped population:

> Any two messages carried by the topic, each of a mapped schema, whose identity tuples are equal are the **same logical message** — hence of the same schema, with equal payloads.

Three consequences are deliberate:

1. Two publications sharing an identity are attempts at publishing one logical message. The declaration says nothing about how often that message is delivered; delivery is a realization fact (§10.3) — which already speaks of "the same logical message" being redelivered, and here gains its payload-level anchor.
2. Because equal identity implies same schema, cross-schema identity collisions are excluded. An architect must not place two schemas in one identity domain if distinct logical messages of those schemas can share the identity value.
3. The mapping may cover a subset of the carried schemas. This is a deliberate asymmetry with the transport's grouping key (§10.2), which must place every carried message in some group, while identity is meaningful knowledge per schema.

The message identity is not the transport's grouping key, and it is not object identity. On an order-events topic, `order_id` correctly orders the messages of one order and identifies the *order*; it does not identify the *message*, because `OrderCreated` and `OrderPaid` for one order share it while being different logical messages. The message identity of such a topic is an `event_id`.

Where the publishing operations are modeled, a declared message identity is a checkable claim: a publisher whose publication payload is replay-deterministic under a key propagated into the identity fields (§12, §13) conforms to it. V1 does not perform that check; the declaration is relied on exactly as external-effect idempotency is, subject to §1.3.

Declaring an identity is subject to the §26 authoring rule: declare it only if you are willing for a correctness proof to rely on "same identity value implies same payload". A producer that stamps a fresh timestamp into each publication attempt does not provide the guarantee.

### Transport order is not execution serialization

A transport ordering guarantee (§10.2) describes the order in which messages are logically observed by the subscription abstraction.

It does **not** imply:

- that two consumer invocations cannot overlap,
- that the consumer executes one message at a time,
- that effects produced by the consumer cannot overtake one another,
- or that independent producers had a meaningful business-level happens-before relationship.

To carry transport ordering through operation execution, routing and member-concurrency facts must also support it (§10.6).

### Ordered transport does not invent business order

If two independent upstream producers concurrently publish messages for the same logical key, a keyed transport may impose a sequence between them. That sequence is a real transport order, but it does not prove that either message was semantically required to precede the other.

The verifier must distinguish:

1. an order that merely exists because a transport serialized concurrent inputs, and
2. an upstream semantic/causal precedence that the architecture is required to preserve.

This distinction is central to ambiguous-ordering analysis.

---

## 7. Operations

### `Operation`

An operation is a logical unit of application behavior owned by one service.

Its declaration contains possible invocation sources, one explicit causal program, and requirements. Execution-local transactions, direct effects, transaction outputs, effect intents, and async handles are declared **at the program or transaction site that executes or establishes them**. They are not predeclared as operation-level capabilities or handles: the governing rule is that a semantic object existing because control reaches a particular execution site is declared at that site, and a separate shared declaration is kept only where the contract exists independently of any one execution occurrence — inputs, schemas, data models, topics, state machines and their transition side-effect contracts, and requirements.

The causal program may contain **explicit asynchronous effect lifetimes**: an `execute_effect_async` or `execute_effect_intent_async` step initiates an effect execution without waiting for it to complete, and `join_all` / `race` steps add explicit completion dependencies (§16). This is an L0 program semantic — intentional overlap between logical effects of one invocation — and is completely independent of L1 `ExecutionPool.member_concurrency`, which is runtime invocation capacity. Neither implies the other: a `bounded(1)` member may execute one invocation whose own program has several effects in flight, and a concurrent member may execute many invocations whose individual programs are entirely sequential.

An operation declares **no execution-concurrency fact**. Runtime concurrency is a property of the execution resource an invocation is assigned to, not of the logical unit of behaviour, and is declared exclusively by `ExecutionPool.member_concurrency` (§10.5). There is deliberately no global execution gate hidden inside `Operation`: if serialization follows from runtime execution topology, the model should expose the routing and pool facts that realize it; if it follows from L0 locks or transactions, those proof routes remain available.

`description` is documentation only and has no proof semantics.

### Multiple inputs

Each `Input` declaration is a possible source of an invocation of the operation.

A concrete invocation is associated with the input that triggered it. A `ValueRef` whose source is an input refers to the payload of that triggering logical input.

Multiple input declarations do not mean that one invocation simultaneously receives all of them.

### Stable execution-site IDs versus bindings

Two kinds of names arise from inline declarations, and they are not the same thing.

Some inline occurrences carry a **stable execution-site ID**: `Transaction.id`, `ExecuteEffect.effect_id`, `ExecuteEffectAsync.effect_id`, `EstablishEffectIntent.effect_id`. These IDs do not reference another declaration; each identifies the inline declaration itself — for keyed commit identity, value lineage, diagnostics, conformance, proof evidence, and visualization. A step's `StepLocation` (§16) is not a substitute: moving an inline transaction must not silently change its durable commit identity.

**Bindings** name something produced by execution: a transaction read observation, a transaction output artifact, an effect-intent artifact, an effect result observation, an async handle. Bindings are immutable, single-producer, operation-local, and scoped by the program and transaction structure (§16). There is no rebinding and no shadowing; a binding is not mutable storage and not a durability guarantee. This is not a general variable system — bindings are semantic names whose meaning is determined by the construct that introduces them.

An async launch therefore carries two independently meaningful names — `effect_id`, the stable identity of the inline effect occurrence, and `handle`, an invocation-local synchronization artifact naming one asynchronous execution occurrence. They may resemble one another for author convenience; they must never be conflated (§16).

Every stable execution-site ID and every binding ID must be unique within the operation; the IDs live in the global namespace of §2, so two sites declaring one ID collide (`DuplicateId`). One inline transaction declaration is one transaction occurrence: two locations that genuinely execute transactions declare two inline transactions with distinct IDs. If authoring reuse is later desired, it belongs to a macro/template layer expanding before semantic analysis, not to a semantic transaction-call primitive.

### The program is one explicit causal control structure

`operation.program` is the operation's single control structure: a block of steps executed in order, in which a decision — `match_result` over a bound effect result, or `branch` over an ordinary predicate — nests further blocks, and every reachable path ends at an explicit terminal: `return`, constructing a request input's declared result, or `complete`, returning nothing.

The structure is acyclic by construction. There are no loops; iteration is deliberately deferred (§27). Asynchronous effect steps introduce concurrent effect-completion edges but no loops: the operation remains a control-flow DAG plus effect-completion dependency edges, never a general cyclic concurrent state machine.

An invocation traverses one acyclic **synchronous control path** through the program: one arm at each decision, one terminal. Asynchronous effect steps may initiate effect executions whose lifetimes overlap later control; synchronization steps add explicit completion dependencies to that control path (§16). The path statement is therefore not to be read as saying that every effect on the path completes sequentially. Alternative paths exist only where a decision selects between them, and the DSL exposes what each decision rests on. There is no unexplained selection among alternative complete flows.

Control flow describes causality. It is not a durable workflow, a checkpoint, or a program counter: a retry traverses the same declared control from the first step, and what it re-encounters is judged by the transaction and effect replay rules (§16–§18).

---

## 8. Inputs

## 8.1 Request input

A request input declares a directly invoked operation input and the schema of its payload.

The current request declaration does **not** itself encode:

- transport protocol,
- caller identity,
- retry behavior,
- timeout behavior,
- synchronous network semantics,
- or whether the request originated from a user versus another service.

Outbound operation-to-operation calls are modeled separately through `RequestEffect`.

### `RequestInput.identity`

Request identity is a **guarantee provided by the request boundary**, declaring where the identity of one logical request lives in the payload.

`unspecified` provides no fact: distinct attempts may present arbitrarily different payloads under equal field values.

`keyed` declares an ordered tuple of payload fields and guarantees:

> Any two requests arriving at this input whose values at the declared identity fields are equal present equal payloads, at the granularity of the modeled schema.

Equivalently, the payload is a function of its identity fields. The canonical conforming implementations are a boundary that rejects a retry whose payload disagrees with the original request under the same identity, and a caller contract strong enough to stand in a proof. A rejected conflicting request is not an admitted invocation of the operation, so rejection preserves the guarantee.

The declaration is an implementation guarantee, not a mechanism: it fixes what the payload of a logical request is, deduplicates nothing, and does not by itself discharge any idempotency requirement.

### `RequestInput.result`

A request input declares the `Result<Ok, Err>` contract a request through it returns:

```yaml
result:
  ok: schema.CreateOrderResponse
  err:
    schema: schema.RequestRejected
    disposition: unspecified
```

`ResultType { ok, err }` names the `Ok` schema and the `Err` contract — `ErrorResultType { schema, disposition }`. The result is a tagged sum holding exactly one of `Ok(ok_payload)` or `Err(err_payload)`; mutual exclusivity is structural. Conseqa models the algebraic outcome, not any language's API around it. A bare schema id is accepted as shorthand for the `Err` contract and means `disposition: unspecified`; because `unspecified` is epistemic, no shorthand or default may silently declare `terminal` or `retryable`, and canonical serialization always emits the disposition.

The contract belongs to the input rather than to the operation. An operation may expose several request inputs, and a `RequestEffect` already targets one specific `operation + input`, from which it inherits this contract (§13.2). Subscription inputs have no synchronous result.

`Err` is a **logical** outcome, not an interrupted execution. `Err(CardDeclined)` means a synchronous interaction completed and reported a modeled failure; it is conclusive. A crash, a timeout, a lost connection, or uncertainty about whether a remote completed is not an `Err` payload — it is the idempotency and recoverability problem of §9. The two must not be conflated.

### `ErrorDisposition`

The disposition declares whether observing the contract's `Err` terminally resolves the **logical interaction** — one logical request, or one logical external execution — or conclusively ends only the observing attempt:

- `terminal` — observing this `Err` terminally resolves the logical interaction with the declared error payload.
- `retryable` — observing this `Err` conclusively ends the current attempt but does not terminally resolve the logical interaction; another attempt is **semantically admitted**. It does not say a retry occurs, is guaranteed, succeeds, returns a different result, or happens promptly — those are execution semantics Conseqa does not model here, and V1 deliberately introduces no retry policy, loop, attempt count, or backoff.
- `unspecified` — no usable fact. Nothing about terminality or retryability may be inferred (§1.1).

`Ok` is terminal by definition; no `Ok` disposition exists. A retryable `Err` remains a *logical, conclusive* outcome of its attempt — the distinction from crashes and timeouts above is untouched.

The disposition belongs to the **result contract**, not to the schema: the same error schema may be terminal in one contract and retryable in another. One disposition covers the whole declared `Err` variant; heterogeneous per-error-class dispositions inside one contract are out of scope in V1.

For a request contract, the disposition describes whether an error returned by the target semantically admits another logical request attempt. It is orthogonal to `RequestEffect.retry` (§13.2): `retry` describes whether the requesting boundary may issue repeated attempts, the disposition whether another attempt is admitted after this error — `Err retryable` with `retry: never`, and `Err terminal` with `retry: may_repeat`, are both coherent, and no automatic coupling exists. For an external contract, the disposition feeds the strengthened `deduplicated_by` terminal-result rule (§13.3).

## 8.2 Subscription input

A subscription declares invocation from a topic. That is its whole L0 content:

- the topic, and
- the selected message schemas.

It means:

> A logical message admitted through this subscription may invoke this operation.

How often the transport delivers such a message, and where those deliveries execute, are realization facts. They are declared by a **subscription runtime** (§10.3) against the `(operation, input)` pair this input already establishes — no new L0 primitive is needed to name the boundary.

### `MessageSelector::all`

Every schema carried by the topic may invoke this operation through this subscription.

### `MessageSelector::only`

Only the listed topic message schemas may invoke through this subscription.

It does not restrict what other schemas the topic itself may carry.

---

## 9. Operation requirements

Operation requirements are **proof obligations**.

Declaring one does not assert that the operation already satisfies it.

### `SerializationRequirement`

A serialization requirement keyed by a `ValueRef` means:

> Invocations with the same logical key must not execute concurrently.

Different keys may execute concurrently unless constrained elsewhere.

Serialization establishes mutual exclusion/non-overlap. It does **not** establish which same-key invocation should come first.

Thus a keyed routing domain on a serial pool member, a lock, or another mechanism may prove serialization without proving ordering.

The requirement is L0: it constrains the application machine. What discharges it is usually L1, and always in one shape:

```
semantic key equivalence
    -> routing-domain equivalence
    -> member ownership
    -> member concurrency
```

V1 accepts three routes. **Vacuous population**: the key's subscription input admits no message schemas, so the constrained population is empty by declaration — the only `l0_only` route. **Request-routed**: a `Router` serves the key's request boundary, every component of its semantic routing key carries the same logical value as the requirement key (§4), its `MemberAssignment` gives that domain one active owning member including through handoff, and the target pool declares `member_concurrency = bounded(1)`. **Subscription-routed**: the same argument on the delivery side, with `key: grouping_key` naming the effective grouping domain (§10.2) and the requirement key established to carry the grouping key for every admitted schema.

**No ordering fact participates in any of them.** Serialization is about non-overlap; a grouping domain is the whole of what a transport has to supply for it. That is the main reason grouping is declared independently of ordering — an unordered transport that still groups by key serializes, and the model can say so without claiming an order it does not provide.

Four things are deliberately not credited. A **shared pool** is a shared execution population, not a shared routing domain: two boundaries assigned to one pool, even under equal-looking keys, borrow nothing from each other. **Routing absence** yields the target population and no member-affinity fact at all. A **routing key wider than the requirement key** partitions same-key invocations across domains, so equality of the requirement key implies nothing. And `bounded(n)` with `n > 1` permits overlap wherever it appears.

### `OrderingRequirement`

An ordering requirement keyed by a `ValueRef` means:

> Same-key invocations for which a meaningful logical precedence exists must preserve that precedence through the operation's semantically relevant execution.

Ordering is stronger than merely choosing *some* serial order.

A proof must therefore establish both:

1. where the relevant precedence comes from, and
2. that the execution mechanism preserves it.

Arbitrarily serializing concurrent inputs can satisfy a serialization requirement but cannot invent a semantic precedence required by an ordering proof.

Where preserving the required order entails preventing later invocations from overtaking earlier ones, the proof must also establish the necessary execution serialization.

V1 recognizes one precedence source: the effective transport ordering (§10.2–§10.3) — `within_group`, or `global`. A request input has no precedence source at all, and a key not sourced from an input selects no population; both are unproven.

That request inputs have none is worth stating plainly: a router keyed exactly like the requirement, on a pool whose members are serial, does establish serialization — and still no ordering, because arrival order of unmodeled callers is not a logical precedence. There is nothing for the mechanism to preserve. A separate precedence source would be required.

The mechanism is the §10 composition, and it is the serialization argument plus a precedence: the requirement key is established to be the effective grouping key for every admitted schema, `key: grouping_key` routes by that same domain, `MemberAssignment` gives it one active owning member, and `member_concurrency = bounded(1)` stops a later invocation overtaking an earlier one.

Every leg is interrogated, not merely cited. The routing key is matched exhaustively, the member assignment is checked to give a domain one active owning member, and the pool's concurrency is checked to be `bounded(1)` — so a future routing key or assignment with weaker guarantees cannot be carried into a proof as though it were the one this rule was written for. Where a boundary is routed two ways, or a topic and its subscription both declare transport semantics, there is no single set of facts to reason from and the verifier refuses rather than reading whichever half it finds first.

Both precedence sources require that same grouping identity, and for the same reason: a precedence only reaches execution if same-key deliveries stay together. `within_group` needs it because its guarantee is *about* the group. `global` needs it because an order over everything is still lost the moment two same-key deliveries land on different members. So the grouping evidence is established once and cited by either — serialization proves on the grouping alone, and ordering is that argument with a precedence added.

This is why an ordering proof is strictly stronger than a serialization one over the same key, and why dispatch alone can never supply it: dispatch preserves precedence, it does not create any (§10.3.1). Dispatch additionally carries the order-preservation obligation of §10.3, so redelivery cannot invert the precedence: a failure-driven redelivery cannot be overtaken by a later message of its domain, and a duplicate of an already completed message is a repeated attempt at a logical invocation that took effect in order — what that attempt does is the idempotency requirement's obligation, not ordering's, and the proof records which requirement answers for it or that none does. Vacuously discharged: a subscription admitting no message schemas.

### Serialization versus ordering

These terms are deliberately separate:

- **serialization**: same-key invocations do not overlap;
- **ordering**: the correct same-key precedence is preserved.

A FIFO mutex may provide both if its acquisition order is proven to correspond to the required input order. A non-FIFO mutex may provide serialization without providing the required ordering. Likewise a routing domain on a serial pool member provides serialization; it provides ordering only when a transport precedence exists for the mechanism to preserve.

### `IdempotencyRequirement`

An idempotency requirement identifies a logical invocation by a composite `IdempotencyKey`.

The requirement means:

> Repeated attempts representing the same logical invocation must not cause externally distinguishable duplicate logical work beyond what the declared idempotency contract permits.

The solver must analyze the complete admitted retry path through transactions, transaction artifacts, publications, requests, and external effects.

A transaction may contribute to the proof in two distinct ways:

1. **natural replayability**, derived from the transaction's declared semantics and deterministic provenance; or
2. **explicit durable keyed commit deduplication**, declared with `DeduplicatedBy { key }` on the transaction.

These mechanisms are not interchangeable. A transaction that merely prevents a second commit is not necessarily naturally replayable, because a retry may need to reproduce transaction artifacts required by later program steps.

The requirement is not discharged merely because the operation has a field named `idempotency_key`, because a `TransactionOutput` exists, or because an `EffectIntent` exists.

V1 discharges the requirement over each **admitted path** of the program — a path ending at `complete`, or at a `return` for the triggering input (§16) — under the governing key's population (§12). Three legs must hold on every admitted path:

- **State leg.** Every transaction step must be retry-safe: a keyed commit over a stable key, or naturally replayable. There is no final-step exemption, because a duplicate delivery re-drives the whole program even after terminal completion.
- **Effect leg.** Every effect-executing step must be duplicate-safe per the §13 rules, since even a recovered intent may be executed again (§14) — and those rules follow the work an attempt causes into other operations: a request is safe only when its target collapses duplicate invocations, a publication only when every modeled consumer collapses duplicate deliveries, each through its own proven requirement.
- **Control leg.** Every decision on the path must replay (§16): the matched result replay-stable, or the branch condition deterministic over replay-stable roots, so that every attempt in the class traverses the same path. When a controlling observation may differ between attempts, a retry may do different work, and V1 has no compatibility argument for the two histories; the decision is an obstacle.

A verdict therefore covers the cascade the operation starts, and V1 computes the mutually dependent verdicts as a greatest fixpoint (below), so a cycle whose members each collapse the others' duplicates is proven and marked coinductive. Result consistency is the separate result-replay obligation below; its verdicts feed in only where a decision rests on a request effect's result. Vacuously discharged: an empty population; no admitted path, so an attempt performs no modeled work; and a triggering subscription with `at_most_once` delivery whose payload is identity-pinned by the key (§18) — same-class messages are then one logical message delivered at most once, so a class holds at most one attempt.

### `ResultReplayRequirement::replay_consistent`

The `result` member of an idempotency requirement asks a further question of the request result:

> Repeated admitted attempts in the same logical idempotency class that return a request result must return the same result variant and a replay-equivalent payload.

No privileged result artifact is involved. A request result is constructed directly at a `return` terminal from values available there (§15), so the proof is control-path replay plus ordinary provenance. V1 discharges the requirement, for each admitted path ending at a `return` for the triggering input:

1. every decision on the path must replay (§16): a class then follows one path to one terminal, which fixes the variant; and
2. the terminal's derivation must be replay-deterministic in the context at the terminal — `deterministic` over roots the §18 rules make stable, including transaction outputs by route A or route B of §17 and effect results whose targets prove their own consistency.

Premise 2 may name other operations' verdicts, since a bound request result is stable only when the target proves its result replay-consistent for the targeted input (§13.2). The checks are therefore computed as a greatest fixpoint over the replay-consistent requirements, exactly as idempotency's are: a cycle of requests whose members each pass their local checks is proven, and the proof is marked coinductive.

Vacuously discharged: a key triggered by a subscription, or a program none of whose admitted paths returns a result for the triggering input — there is nothing to stabilize.

### `result: unspecified`

No replay-stability requirement is declared for the result.

This does not waive the operation's idempotency requirement for side effects.

### Fixpoints and coinductive proofs

Request and consumer discharge make idempotency verdicts mutually dependent, and cycles — request cycles between operations, publication cycles through topics — are legal models. The same holds for result-replay verdicts through request effects. V1 computes each family's verdicts as a **greatest fixpoint**: every requirement with an admissible governing key is assumed, and whatever fails under that assumption is dropped until nothing more fails. The iteration is monotone — fewer assumptions never prove more — and terminates. A cycle whose members each pass their local checks under the mutual assumption is therefore proven, and the proof is marked *coinductive*; the least fixpoint is computed alongside, only to identify which proofs rest on such a cycle.

*Soundness, by minimal counterexample.* Suppose some member of a self-consistent set `P` were violated: some execution in which duplicate attempts at one of its logical invocations cause distinguishable duplicate work. Among all such violations across `P`, take one whose duplicate work lies at the shortest causal distance from the duplicated attempts. That work is not on the invocation's own path — the state leg and the external and publication legs are discharged by local facts that assume nothing about `P`. So it lies downstream of a request or publication, and the local check establishes that the duplicates reached the target or consumer as payload-equal inputs, falling into one class of *its* requirement, which is in `P`. The duplicate work is then caused by duplicate attempts of that invocation, at a strictly shorter causal distance — contradicting the choice of the violation. Every causal chain in a real execution is finite, so no violation exists. The argument uses the downstream requirement only for what its local check provides — a key over the input that collapses payload-equal attempts — never for its verdict, which is why assuming the verdict is harmless. The result-replay analogue substitutes "return a different result" for "cause duplicate work": a differing observation would be a violation at strictly shorter causal distance.

### `RecoverabilityRequirement`

A recoverability requirement keyed by an `IdempotencyKey` means:

> The logical invocation identified by that key must reach a valid terminal of the operation program — `return` or `complete` — after any modeled interruption.

Recoverability is a **progress** obligation. Idempotency is a **safety** obligation. They are deliberately separate requirements because neither implies the other.

An idempotency requirement constrains what repeated attempts may do. It is satisfied vacuously by never retrying at all: an invocation that crashes after its transaction commits and is never re-driven produces no duplicate work, and therefore violates nothing. Idempotency consequently says nothing about whether the remaining steps of an interrupted program ever execute.

This is exactly the gap left by §14 and §22. Consider:

```text
Transaction T   DeduplicatedBy(K)
    Transition pending -> paid
        establishes effect intent E
COMMIT

<crash>

ExecuteEffectIntent E
```

The keyed commit makes `E` *recoverable*, and the operation's idempotency requirement is satisfied. But nothing in the model yet obliges anyone to come back and execute `E`. The order is durably `paid` and the payment capture never happens. A recoverability requirement is what makes that outcome a declared violation rather than an unremarked silence.

The key identifies the retry-equivalence class in the same sense as §12: attempts sharing the key are attempts at the same logical invocation, so re-driving one of them continues that invocation rather than starting a new one.

The requirement does not name a path. An invocation takes one path through the program (§7), and a resumed attempt reaching the terminal of any admitted path discharges the obligation. Which paths remain admissible after a partial execution is open question 7 (§27); the requirement is deliberately stated so as not to prejudge it.

### `completion: resumable`

An interrupted attempt must be **able** to resume and drive the program to a terminal.

For every prefix at which the invocation may fail, the solver must establish that a continuation exists:

- each already-committed transaction resolves on re-encounter, by natural replay or by `Commit(T,K)` (§17);
- every artifact a later step consumes is replay-available by route A or route B of §17;
- no step is left in a state from which the program cannot proceed.

V1 discharges this by **same-path continuation**: for every admitted path — one ending at `complete`, or at a `return` for the triggering input — re-driving that same path from its first step must reach the path's terminal. Per admitted path:

- every transaction step needs re-encounter resolution, except one that is the final step of a path ending at `complete`, after which no failing prefix exists. A `return` is not such an exemption: constructing the result is itself a step after the transaction, so every transaction on a returning path must resolve;
- consumed artifacts — an intent the path executes, which must be established at all; a transaction output referenced by a later transaction body or an effect derivation; the outputs the terminal result is derived from — are judged by the replay rules of §17 and §18, with references inside the establishing transaction exempt by atomicity, and a commit key judged by the re-encounter analysis rather than double-counted as consumption;
- a decision is **never** an obstacle to progress. A retry not established to take the same arm follows whichever admitted path it then takes, and that path is analyzed on its own; the difference in work is idempotency's concern, not recoverability's.

This is a sufficient route and deliberately does not prejudge which other paths a resumed attempt may take (§27 question 7). A program with no path admitted for the triggering input cannot make progress for it, and the obligation is unproven — the deliberate asymmetry with idempotency, for which the same shape is vacuous.

`resumable` does **not** oblige the architecture to actually re-drive the invocation. It is the right declaration when the retry driver lies outside the model — most commonly a request input whose caller Conseqa does not model.

### `completion: guaranteed`

In addition to resumability, the architecture must guarantee that the logical invocation **is** re-driven until the program reaches a terminal.

This is a liveness obligation and additionally requires a modeled retry driver, such as:

- `delivery: at_least_once` on the triggering subscription's runtime (§10.3) — an L1 fact, so a proof taking this route is `runtime_dependent`; or
- an inbound `RequestEffect` whose `retry` is `may_repeat` — an L0 guarantee, so that route stays `l0_only`.

An inbound repeatable request may be declared among a modeled caller's effects or as a state-machine transition side effect, which is a `RequestEffect` under §22. Both driver facts re-drive the *same logical invocation*: a redelivery is another delivery of one logical message, and `may_repeat` repeats one logical request, so the re-driven attempt carries the same payload and hence the same key.

Two cautions apply.

First, the driver facts in the current DSL are duplicate-delivery facts, not bounded-liveness facts. §10.3 states that `at_least_once` "encodes no retry timing, retry count, backoff, or bounded eventual-delivery liveness guarantee." A `guaranteed` proof is therefore conditional on the delivery abstraction genuinely redelivering until the invocation succeeds, in the sense of §1.3.

Second, a request input alone supplies no driver. The caller is outside the model, so `guaranteed` on a request-only operation is normally not dischargeable unless the calling side is itself modeled as a `may_repeat` request effect.

### Idempotency versus recoverability

A recoverability requirement makes retries *expected* rather than merely *possible*. It therefore strengthens the case for also declaring an idempotency requirement, but the DSL does not couple them:

- **recoverability without idempotency** is coherent where repeating the work is harmless;
- **idempotency without recoverability** is coherent where best-effort completion is acceptable.

The checker does not couple them either, but it says when the coupling is absent: a `guaranteed` recoverability proof for an operation that declares no idempotency requirement keyed from the triggering input carries a warning, because the driver makes retries expected and nothing declares them safe.

Neither implies exactly-once external execution. Driving the program to a terminal still leaves the effect-level uncertainty described in §14: an external effect may have succeeded before a crash without that success being durably known.

---

## 10. L1 — runtime topology and realization semantics

L1 describes selected facts about how the L0 machine is realized. It hangs off `Model.runtime` and is entirely optional; absence of any L1 declaration is epistemic — no fact — never an assertion that the realization lacks the property.

```
runtime:
  topics:            <topic id>       -> TopicRuntime      { grouping?, ordering? }
  subscriptions:     <operation id>   -> <input id> -> SubscriptionRuntime
  execution_pools:   <pool id>        -> ExecutionPool
  routers:           <router id>      -> Router
  storage_layouts:   <layout id>      -> StorageLayout
```

Every L1 identifier lives in the one global namespace L0 identifiers do: a pool may not take a topic's id.

### What makes a fact worth declaring in L1

Not "some proof consumes it". L1 has two consumers, and the analyzer is only the first.

The second is the external quantitative layer (§10.9): a scenario supplies member counts, traffic rates, key-frequency distributions and service times, and evaluates the architecture for hot members, hot partitions, routing skew, shared-pool contention and queue growth. That analysis reads the same declarations the verifier does, and it reads some the verifier ignores entirely.

So a realization fact belongs in L1 when it **changes what the realization does** and is **qualitative rather than a number**. `member_assignment: round_robin` (§10.6) is the clearest case: no correctness proof consumes it, and a simulator cannot proceed without it. The numbers stay outside — that boundary is §10.5 and §10.9, and it does not move.

The corollary matters as much: correctness-inertness is not a reason to leave a fact out, and neither is proof-relevance a reason to let a *number* in.

An operation declares **no** execution concurrency of its own. There is no `OperationRuntime`, no execution-lane abstraction, and no lane concurrency anywhere. All runtime execution concurrency is declared in exactly one place: `ExecutionPool.member_concurrency` (§10.5).

### 10.1 The two things routing is, and the many things it is not

Routing has exactly two semantic components, and they are independent dimensions rather than alternative modes:

```
invocation
    |  evaluate semantic routing key
    v
routing domain
    |  member assignment
    v
execution-pool member
```

A **routing key** is a *semantic value derived from an L0 invocation*. Its only job is to define routing-domain identity:

> `routing_key(A) = routing_key(B)`  ⟹  `routing_domain(A) = routing_domain(B)`

A routing key is **not** an execution-pool member id, a worker id, a process id, a physical shard id, a storage partition id, or a host id. It stays stable as the realization changes: `account_id = 42` may be owned by member 17 today and member 31 after a rebalance, and neither the key nor the identity of its routing domain changes. `MemberAssignment` — and only it — performs the mapping onto runtime topology.

Consequently the model contains **no** `unspecified`, `unconstrained`, or `single_member` routing variant:

- **Absence expresses epistemic absence.** A router or dispatch with no `routing` block says "invocations of this boundary execute within this pool", and nothing more. The analyzer infers no member affinity of any kind — not round-robin, not random, not one member.
- **"All invocations map to one member" is an ordinary outcome**, not a category of routing. If a model ever needs a single domain for every invocation, that should come from an ordinary key expression capable of producing one constant domain.

The governing design rule: *if a case can be represented as an ordinary value of a more fundamental semantic dimension, it is not promoted into a special routing variant.*

### 10.2 Grouping and ordering

Two independent transport facts, declared at exactly one of two scopes.

**Grouping** describes the runtime equivalence domains a transport puts messages into:

```yaml
grouping:
  schema.OrderCreated: [account_id]
  schema.OrderCancelled: [account_id]
```

means

> `grouping_key(A) = grouping_key(B)`  ⟹  `runtime_group(A) = runtime_group(B)`

and nothing further. Not ordering, not serialization, not member assignment, not execution affinity, not uniqueness, not storage-partition identity. Each of those is its own declared fact.

Its presence *is* the declaration: a transport either groups by a key or it does not, so there is no `none` to spell — omitting `grouping` says the transport establishes no domain.

Different schemas may map differently named fields into one domain, so the mapping is per schema; tuple positions correspond across schemas, so every mapped tuple shares one arity, and the mapping must cover every message the topic carries — an unmapped schema would belong to no group at all.

**Ordering** describes the precedence the transport establishes, independently of how it groups:

```yaml
ordering: within_group
```

| | Meaning |
|---|---|
| omitted | No precedence is declared at this scope, and the scope stays open. |
| `none` | An explicit "this transport orders nothing" — a declaration, so it claims the scope like any other. |
| `global` | One precedence relation across every message in scope. Stronger than per-group order, and it needs no grouping key of its own. |
| `within_group` | Precedence among messages of the same declared grouping domain. Requires a grouping at the same scope, since otherwise there is no domain the guarantee could be interpreted over. |

Neither implies globally ordered *execution*. A transport precedence reaches execution only if the topology preserves it (§10.6).

#### Why they are separate

Bundling a grouping key inside a keyed-ordering variant overconstrains the model and forces unrelated consumers to depend on an ordering declaration merely to recover a domain. A realization may provide grouping without ordering, ordering without grouping, or both, and the model must be able to say which.

The serialization verifier is the sharp case: it needs only

```
same K  ->  same runtime group
```

and no transport precedence whatever. An unordered queue with consistent-hash workers is an ordinary architecture, and with the two facts separate it is stated as it is — `grouping: keyed`, `ordering: none` — instead of claiming an order the transport does not provide in order to reach the key.

#### One scope, exclusively

For a given topic, grouping and ordering are declared **either** once at the topic runtime **or** independently at each subscription runtime, and never at both.

```
topic-scoped                          subscription-scoped

TopicRuntime:  grouping, ordering     TopicRuntime:  neither
Subscription A: neither               Subscription A: grouping, ordering
Subscription B: neither               Subscription B: grouping, ordering
```

Topic-scoped is the shape of a runtime that imposes one domain on the topic as a whole — a partitioned log, canonically. Every subscription of that topic observes those semantics, and none may substitute another.

Subscription-scoped is the shape of a publication that fans out into independently configured queues or transport paths. Two subscribers of one logical channel may then group differently — one by `account_id`, another by `region_id` — where a topic-wide declaration would be false of both.

The two are not a default and an override. They are different declaration modes, and validation rejects a model that uses both for one topic. That is why omitting `ordering` and writing `ordering: none` differ: the first declares nothing and leaves the scope open, the second is an explicit negative and claims the scope. Collapsing them would let a subscription say "I have no precedence" and silently inherit the topic's — exactly the override this model exists to prevent.

A topic-scoped grouping serves every subscription of the topic, so it must place every carried message in a group. A subscription-scoped one only has to cover what its own `MessageSelector` admits: requiring more would be unsatisfiable on a heterogeneous topic where a filtered-out schema has no comparable field. There is deliberately **no inheritance, no override, and no fallback chain**, because each of those would make the effective semantics depend on implicit precedence between declarations, allow partial overrides, let grouping and ordering come from different scopes, and turn proof provenance into something a reader has to reconstruct. A model validates into one mode before analysis begins, so §10.3's resolution is a lookup and not a decision.

The two fields stay flat on their owning object rather than inside a `TransportSemantics` wrapper: the wrapper would carry no independent meaning, and the scope owner already says the facts belong together.

### 10.3 Effective semantics

For a subscription `S` of topic `T`:

```
if T declares transport semantics:
    EffectiveGrouping(S) = T.grouping
    EffectiveOrdering(S) = T.ordering
else:
    EffectiveGrouping(S) = S.grouping
    EffectiveOrdering(S) = S.ordering
```

Every proof records which scope it read, so a reader tracing a verdict knows which declaration to look at and which one changing would invalidate it.

### 10.3.1 `SubscriptionRuntime`

```
runtime.subscriptions[<operation>][<input>]:
  delivery: unspecified | at_most_once | at_least_once
  grouping: ...          # subscription-scoped mode only
  ordering: ...
  dispatch:
    pool: <execution pool id>
    routing:                       # optional
      key: grouping_key
      member_assignment: { kind: consistent_hash }
```

The referenced input must be an L0 subscription input.

#### Delivery semantics

**`unspecified`** — duplicate/loss behaviour is unknown.

**`at_most_once`** — the same logical message is delivered no more than once. Loss may still occur. This is not an exactly-once guarantee.

**`at_least_once`** — a successfully published logical message may be delivered more than once, so duplicate operation invocation must be considered possible. This is primarily a duplicate-delivery fact; it encodes no retry timing, retry count, backoff, or bounded eventual-delivery liveness guarantee.

#### `SubscriptionDispatch`

Dispatch is where transport semantics become execution topology, and it is deliberately not the same thing as grouping. `pool` names the execution population deliveries are assigned to; there is no lane abstraction and no lane-concurrency field, because how much may execute there is the pool's business (§10.5).

```
message
    |  runtime grouping / ordering semantics
    v
SubscriptionDispatch
    |  member assignment
    v
ExecutionPool member
```

`routing` is optional. Omitted, the only available fact is that deliveries execute within the pool.

#### `key: grouping_key`

Routes by the effective grouping domain, whichever scope declared it. Referencing the domain rather than restating its field mapping is what gives the analyzer grouping/routing-domain identity for free and keeps the two from drifting apart; it requires a keyed grouping to be in effect.

#### Dispatch preserves precedence, and never invents it

Where a precedence exists, dispatch must preserve it when admitting invocations to execution — including through failure-driven redelivery and ownership reassignment. A conforming runtime must not:

1. establish delivery A before B within one effective group;
2. leave A semantically incomplete;
3. admit B in a manner that permits B to overtake A contrary to the declared guarantee.

This order-preservation responsibility is what replaces the logical-lane semantics of the previous model. It is a preservation obligation only: dispatch contributes no precedence of its own, so a routing declaration alone proves no ordering.

### 10.4 `Router`

```
runtime.routers[<router>]:
  boundary: { operation: <operation>, input: <input> }
  pool: <execution pool id>
  routing:                              # optional
    key: [ <field path>, ... ]          # non-empty
    member_assignment: { kind: consistent_hash }
```

A router describes how invocations entering through one L0 **request** input are assigned into a pool. It does not choose which operation executes — the L0 request boundary already determines that. It answers exactly one question:

> Which member of the target execution pool owns the semantic routing domain of this request invocation?

The key is evaluated against the request input's schema. Two invocations of one router belong to the same routing domain exactly when their evaluated key tuples are equal.

One request boundary has at most one router.

#### A router without routing

```yaml
routers:
  router.health:
    boundary: { operation: op.health, input: input.request }
    pool: pool.web
```

means "requests through this boundary execute within `pool.web`", and provides no member-affinity fact. This is the sole representation of unspecified member routing.

### 10.5 `ExecutionPool`

```
runtime.execution_pools[<pool>]:
  member_concurrency: unspecified | unbounded | bounded{ value }
```

A pool identifies **a logical population of interchangeable runtime members capable of executing the operation invocations assigned to that pool**. It establishes two things: runtime population identity, and the qualitative execution concurrency of each member.

#### Pool identity

If two execution paths target the same pool, they share one logical runtime execution population — a request router and a subscription dispatch alike. This fact is meaningful even though Conseqa never declares pool cardinality.

If they target different pools, they target distinct logical populations. That implies **nothing** about physical hosts, processes, deployments, availability zones, or failure domains; those remain outside L1.

#### `member_concurrency`

**`bounded(n)`** — at most `n` operation invocations assigned to one member of the pool may simultaneously be active. The bound applies across *all* invocations assigned to that member, irrespective of operation or ingress mechanism.

**`bounded(1)`** is the important serialization case: one pool member executes at most one invocation at a time.

**`unbounded`** — no finite member-level execution bound may be assumed.

**`unspecified`** — no usable fact about simultaneous execution on one member.

Unlike routing, member concurrency has genuine semantic value in distinguishing an unknown resource from an explicitly unconstrained one, so it keeps both negative states.

#### Cardinality is external

A pool carries no member count, replica count, CPU, memory, autoscaling rule, host count, or container count. An external simulation scenario may instantiate `DataWorkers.members = 16` or `= 128` against the same Conseqa architecture, along with traffic rates, key-frequency distributions, service-time distributions, capacity, queueing, and latency. Those values never become Conseqa semantics.

### 10.6 `MemberAssignment`

```
member_assignment: { kind: consistent_hash }
```

A member assignment describes how a routing domain is assigned to a member of an execution pool. The name is deliberate: the semantic relation is `semantic routing domain -> runtime execution-pool member`, and assignment accommodates ownership and handoff, which "placement" does not.

#### `consistent_hash`

> Equal routing domains are owned by the same execution-pool member during a stable ownership epoch.

Different routing domains may be assigned to the same member. Conseqa prescribes no hash function, virtual-node count, membership-discovery mechanism, or choice between Ketama and rendezvous hashing. The declaration specifies semantic assignment behaviour, not implementation mechanics.

#### `round_robin`

> Each invocation goes to the next member in rotation, irrespective of routing domain.

No correctness proof consumes it. It earns its place anyway, for two reasons.

The first is that it is a **primary input to the external analysis** L1 exists to feed (§10.9). Consistent-hash and round-robin over one pool, at one cardinality, under one workload, behave completely differently: hashing a skewed key distribution concentrates load on the members owning the hot domains, while rotation spreads load evenly and destroys locality. Hot members and routing skew are exactly what a simulator is asked to find, and it cannot find them without knowing which assignment is in force. A model that could not distinguish the two would be handing that analysis a coin flip.

The second is diagnostic. Omitting the routing block says *nothing is known* about member affinity; `round_robin` says *affinity is known not to exist*. A serialization or ordering requirement over such a boundary is then refused with a reason — "rotation puts same-key invocations on different members" — rather than for want of a declaration nobody has made. An author reading the first is told to go and find out; reading the second, to change the architecture.

A routing key declared alongside it still names domains, and those domains keep their own identity. This assignment simply does not respect them.

This is the "explicit negative routing guarantee" the initial model deferred (§27), admitted now that there is a use for the distinction. It is emphatically *not* the `unconstrained` routing variant that model declined: routing keys still have exactly two components, and absence still means absence. What changed is that the *assignment* dimension gained a second ordinary value.

#### Safe ownership transfer is normative

Any member assignment used to establish keyed serialization **must preserve exclusive ownership through reassignment**. If routing domain `K` moves from member A to member B, a conforming runtime must not permit A and B to execute `K` in a manner that violates the declared one-owner semantics.

Draining, leases, generation fencing, coordinated handoff, and partition-ownership protocols are conforming mechanisms. Conseqa models the resulting guarantee, not the mechanism. A rebalance that silently invalidates *one routing domain → one current owning member* while the runtime still claims conformance is a non-conforming implementation, not a modeling gap.

### 10.7 `StorageLayout`

```
runtime.storage_layouts[<layout>]:
  object: { data_model: <data model>, object: <object> }
  partition_key: [ <field path>, ... ]     # non-empty
```

A storage layout describes physical partition identity for one L0 data object. V1 admits at most one primary layout per object.

**Object identity and storage identity are distinct.** Object identity answers "which logical object instance is this?"; the layout answers "into which physical partition is that instance mapped?". The following is entirely valid, and the analyzer never substitutes one for the other:

```
DataObject identity:   message_id
StorageLayout key:     (channel_id, bucket)
```

**A partition key is not a routing key.** They may reference the same underlying fields without becoming the same concept: a router key defines an execution-affinity domain, a partition key defines physical storage partition identity. They may also deliberately differ —

```
Router routing key:       channel_id
StorageLayout key:        (channel_id, bucket)
```

— meaning all work for one channel shares an execution-affinity domain while the channel's data stays distributed across partitions. Conseqa never infers equivalence between routing-domain identity and storage-partition identity merely because their key expressions coincide.

A layout implies nothing about database vendor, node count, replication factor, replica placement, consistency level, partition capacity, latency, or availability.

### 10.8 `Service` has no execution-topology meaning

A service is an L0 grouping. No inference may be made that same service implies same pool, same process, same deployment, same runtime member, or that different services imply execution isolation.

Same-service operations may target different pools. Different-service operations may share one pool. Until `Service` acquires stronger normative semantics, it remains independent of runtime placement.

### 10.9 The external analysis boundary

L1 describes semantic topology, qualitatively. Everything countable stays outside it, supplied by a scenario evaluated *against* a Conseqa architecture:

```text
pool member counts          service-time distributions
traffic and message rates   storage node counts
key-frequency distributions replication factors
capacity, queueing, latency failure probabilities
```

Given those, a simulator can evaluate hot execution members, hot storage partitions, routing skew, shared-pool contention, pool scaling, alternative routing keys, alternative partition keys, queue growth, latency, and request amplification — none of which become Conseqa semantics.

The division works because the declarations are qualitative and the scenario is quantitative. One architecture:

```text
Router GetMessages:  routing key = channel_id, member_assignment = consistent_hash
                     pool = MessageReads
ExecutionPool MessageReads:  member_concurrency = bounded(32)
StorageLayout Message:       partition_key = (channel_id, bucket)
```

can be run against `MessageReads.members = 16` or `= 128`, against a uniform `channel_id` or a Zipf one, without a word of it changing. And when the scenario moves `channel_id` domain X from member 17 to member 31, the routing domain keeps its identity — that is §10.1's point, and it is what lets the same declarations serve both consumers.

This is also why L1 carries facts no proof reads. A member assignment is inert to the verifier and decisive to the simulator: under a skewed key distribution, consistent-hash concentrates load on the members owning the hot domains while round-robin spreads it and gives up locality. Same pool, same cardinality, different answer.

---

## 11. Value references
### `ValueRef`

A value reference consists of:

- a `ValueSource`,
- and a `FieldPath` relative to that source's schema.

It identifies a logical value and is the main mechanism for linking keys, predicates, and deterministic provenance across the model.

#### Surface syntax

A value source may be written as a `kind:id` string instead of the tagged map, which with a dotted path puts a reference on two lines:

```yaml
- source: input:input.create_order.request
  path: idempotency_key

- source:
    kind: input
    id: input.create_order.request
  path:
    - idempotency_key
```

The kind is always written. It is never inferred from the id: the seven sources index seven separate namespaces, and inferring would let the meaning of a reference depend on what happens to resolve, silently choosing for an id declared in two of them.

Neither shorthand withholds a fact the canonical form states — both are re-spellings with the canonical form still accepted — so §1.1 is not engaged. Serialization emits the canonical form, keeping one explicit shape for tooling.

### Reference scope

A value reference is evaluated by some set of invocations, and may only name a source those invocations can actually observe.

The evaluating invocations are determined by where the reference is declared:

- a reference declared within an operation — in its requirements or anywhere in its program, transaction bodies and inline effect contracts included — is evaluated by invocations of **that operation**;
- a reference declared on a state-machine transition side effect is evaluated by invocations of **whichever operation applies that transition**.

From that scope:

- `input`, `transaction_output`, `effect_result_ok`, and `effect_result_err` must name declarations of an admitted operation. An input, a transaction-output binding, and a result binding each belong to exactly one operation: another operation's input is never the "current invocation's input payload", and another operation's output or bound result is never available to this invocation.
- `effect` must name an inline effect occurrence of an admitted operation, or a transition side effect of a transition an admitted operation applies.
- `state_machine_subject` is unrestricted. State machines are global, and any operation may address the persistent objects they govern.
- `transaction_read` is restricted further, to the transaction execution that produces it. See §18.
- `transaction_output`, `effect_result_ok`, and `effect_result_err` are restricted further still, to program points where control flow definitely provides them. See §16.

This is a structural coherence rule, not a replay-stability claim. A reference being observable says nothing about whether its value is stable across retries.

### `ValueSource::input`

References a field in the current invocation's input payload.

An input reference is not automatically replay-stable merely because two attempts share an idempotency key. Replay stability must follow from the V1 rules of §18: the governing key's own components, a declared request or message identity pinned by that key, artifact recovery or reconstruction, or deterministic derivation over such roots.

### `ValueSource::effect`

References a field in the payload of a `PublicationEffect` or `RequestEffect`.

For an operation-owned effect, the ID resolves to the inline occurrence that declares it: an `ExecuteEffect.effect_id` or an `EstablishEffectIntent.effect_id`. For a transition-owned effect it resolves, as before, to the state-machine transition side-effect declaration. This preserves the idempotency-key-propagation and value-lineage semantics without an operation effect registry.

Declaring such a reference establishes value lineage only if the surrounding declaration states how the value is propagated. It does not mean the effect has already executed.

An external effect has no inspectable payload schema in the current DSL and therefore cannot provide ordinary field-path value references. Its declared *result*, if any, is reached through a result binding instead (below).

### `ValueSource::transaction_output`

References a field of a transaction-output binding (§15): a typed value a transaction of the same operation exported into the operation's control. The ID resolves to the `establish_transaction_output` binder that produces it, whose declared `schema` the path resolves against.

Availability is a matter of control flow. The reference is valid only at a program point where the output is **definitely available**: established or recovered by a transaction on every path reaching that point (§16). Within the establishing transaction itself, a later step may reference the output by step order. How the value reaches a retry — reconstruction by a naturally replayable establishing transaction, or recovery from an explicitly keyed commit — is the §17 question, and the source kind does not itself imply independent durable storage.

### `ValueSource::effect_result_ok` and `ValueSource::effect_result_err`

Reference a field of the `Ok` or `Err` payload of a bound effect result (§13), where the id names the `bind` of an `execute_effect` or `execute_effect_intent` step. The path resolves against the effect contract's `ok` or `err` schema respectively.

These are **operation-local observations** of the current attempt. They are not transaction artifacts, and they are not inherently durable. Each is available only inside the arm of a `match_result` on that binding that selects its variant: `effect_result_ok` in the `ok` arm, `effect_result_err` in the `err` arm. Neither survives the join after the match (§16).

### `ValueSource::state_machine_subject`

References a field on the persistent object governed by the identified state-machine subject.

The path is interpreted against that subject object's schema. Mutable subject state is not automatically replay-stable.

### `ValueSource::transaction_read`

References a field observed by a `Read` earlier in the same transaction execution, through the read's `bind`.

Transaction-read bindings are transaction-local provenance sources. They are not durable cross-transaction artifacts and are not available to later transactions or program steps merely because the surrounding program continues. Information observed inside a transaction reaches later control only by being exported through a transaction output (§15).

V1 permits them in the semantic model but does not use a provenance chain that reaches a transaction read to prove natural transaction replayability. See §18.

---

## 12. Idempotency keys and propagation

### `IdempotencyKey`

An idempotency key is an ordered tuple of `ValueRef` components.

Two attempts have the same declared idempotency identity when all components are equal in the declared component order.

A composite key is one logical key, not a set of independent alternative keys.

The current DSL assigns no special semantic meaning to an empty component list; authors should not rely on one unless a future contract explicitly defines it.

### Governing keys and the attempt population

When an idempotency, recoverability, or result-replay obligation is verified, its key is the **governing key** of the analysis. V1 analysis proceeds only when every component of a governing key is sourced from **one** input of the operation — the *triggering input* of the analysis. A component sourced from mutable persistent state, or from an artifact the invocation itself produces, cannot define a pre-execution equivalence class, and the obligation is `Unknown`.

The attempt population of such an obligation is the set of invocations triggered by that input. An invocation triggered by a different input has no value for the key, belongs to no equivalence class, and is not constrained by the obligation — the same population reading that applies to serialization and ordering keys, for the §7 reason that a concrete invocation is associated with the input that triggered it.

An empty governing key places every attempt in one class; no component roots exist and no identity can be pinned by it (§18), so essentially nothing is replay-stable relative to it.

`DeduplicatedBy` keys on transactions are not governing keys and are exempt from the single-input restriction; their fitness for artifact recovery is judged by the §18 rules.

### `IdempotencyKeyPropagation`

A propagation declares that the target values carry the same logical idempotency identity as the source values.

This is a **lineage assertion**.

It can bridge renamed fields or different message/request schemas.

Propagation does **not** itself deduplicate anything. It allows the verifier to trace the same logical key across an effect boundary.

V1 reads it on the consumer's side: for a governing key whose population rests on a topic's keyed message identity, each modeled producer of an admitted schema either declares a propagation whose targets cover the identity fields — the identity then carries the producer's key, and when that key is one of the producer's own idempotency requirements, distinct logical invocations of the producer publish distinct messages — or declares none, in which case the identity rests on the topic declaration alone. Both facts are recorded next to the consumer's verdict; neither changes it.

---

## 13. Effects

Effects describe work outside the operation's immediate transaction state.

### Effect contract versus effect instance

An effect declaration is a contract describing what kind of logical work occurs. Depending on effect kind, this includes information such as the destination or target, the schema, retry semantics, idempotency-key propagation, and external idempotency guarantees.

An operation-owned contract lives inline at its one execution or establishment site (§7); a transition-owned contract lives on the state-machine transition, shared by every operation applying it (§22). Either way the contract does not define how the values of a particular effect instance are computed. An effect instance is constructed at an execution or establishment site, and each such site declares the provenance of the values used to construct it:

- a direct `execute_effect` or `execute_effect_async` program step declares the contract and `values` (§16);
- an explicit `establish_effect_intent` transaction step declares the contract and `values` (§14);
- a `transition` transaction step declares `effect_intents`, one intent binding and one derivation per side effect of the applied transition (§22).

`execute_effect_intent` and `execute_effect_intent_async` consume an already-established effect instance and therefore declare no derivation: the instance's values were fixed at establishment (§14). Async is an execution mode, not a new kind of effect: there is no `AsyncEffect` contract type, and an asynchronous launch uses exactly the same `Effect`, `Derivation`, and `effect_id` semantics as its synchronous counterpart.

A contract's own value references — an external deduplication key, propagation components — are evaluated at the site's actual context: a direct effect's in the operation context immediately before the `execute_effect` step; an explicitly established intent's in the enclosing transaction context at the `establish_effect_intent` step, where they may use preceding transaction reads and outputs under the usual rules; a transition-owned effect's in the applying transaction context at the `transition` step (§22).

### Effect results

A synchronous effect may yield a first-class `Result<Ok, Err>` (§8.1). Which effects do is fixed by the contract:

- a **publication** has no synchronous result and cannot bind one (§13.1);
- a **request** inherits the result contract of the request input it targets, and never redeclares it (§13.2);
- an **external** effect may declare `result: { ok, err }`, or declare none (§13.3).

A synchronous execution site — `execute_effect` or `execute_effect_intent` — may **bind** the result under an operation-unique binding (`bind: result.charge_payment.card`). The result type is inferred from the contract, never restated at the site. A result-bearing effect may be executed without a binding when the result is deliberately ignored; an effect with no synchronous result must not declare one (validation: `EffectHasNoResult`). The binding is an attempt-local observation, not a transaction artifact, and its variant payloads are reached through `effect_result_ok` / `effect_result_err` inside a `match_result` on it (§11, §16).

An **asynchronous** launch binds no result: at launch time the synchronous result need not exist yet, so `start effect A; use result(A) before A completed` is not representable. An asynchronously executed effect's result becomes available only through a synchronization step — a `join_all` entry's `bind`, or a `race`'s `bind` — under exactly the contract it would have had synchronously (§16).

The returned result is a separate semantic object from the outgoing effect payload. Stable outgoing values do not by themselves prove a stable returned result; effect-result replay is judged on its own (§18).

## 13.1 Publication effect

A publication effect declares publication of one schema to one topic.

When executed, the resulting logical message participates in the topic's declared delivery and ordering semantics. A publication has no synchronous result: an execution site may not bind one.

`idempotency_key_propagation` describes which values in the published payload preserve an upstream idempotency identity.

A publication declaration does **not** by itself imply:

- exactly-once publication,
- atomic publication with a database transaction,
- deduplication,
- eventual delivery,
- or that the effect executes at all.

Those properties require additional structure/facts.

### Duplicate publication

For an upstream idempotency requirement, a duplicate execution of a publication effect is safe exactly when:

1. the topic declares a keyed message identity mapping the published schema (§6) and the published instance is class-fixed — replay-deterministic for a direct execution, or an intent replay-available by route A or B of §17 — so that every attempt publishes the **same logical message**; and
2. every modeled consumer of that message collapses duplicate deliveries of it. A consumer is an operation subscribing to the topic with a message selection admitting the schema; it collapses duplicates either through an idempotency requirement keyed from that subscription that is itself proven, or by receiving the subscription with `at_most_once` delivery, under which one logical message is delivered no more than once however often it is published.

Condition 1 makes the duplicate no new logical work *at the topic*: at most it raises delivery multiplicity. Condition 2 makes it no new logical work anywhere the model can see. Delivery multiplicity is a degree of freedom the topic contract admits, but the work a redelivery causes in a consumer is still work the upstream attempt caused, and the requirement's "must not cause" is transitive: an operation whose retries double a downstream card charge is not idempotent, however faithfully it republishes one message. A consumer the model does not contain is outside the proof, which is conditional on the model's closed world of consumers (§1.3). Producer and consumer verdicts are mutually dependent; V1 computes them together with request discharge (§13.2) as a greatest fixpoint: a cycle of requirements that each collapse the others' duplicates is proven — a publication cycle through topics no less than a request cycle — by the minimal-counterexample argument of §9, and such proofs are marked coinductive.

`idempotency_key_propagation` plays no role in this discharge: a class-fixed instance already makes every duplicate payload-equal, so a consumer's key evaluates equally across them whichever fields it reads. Propagation remains lineage for the consumer's analysis (§12) and deduplicates nothing on the publishing side.

## 13.2 Request effect

A request effect invokes a specific request input of another operation with the declared schema.

`target.operation` and `target.input` identify the destination.

### Retry semantics

#### `never`

The modeled request mechanism does not intentionally repeat the logical request.

This is a sender-side retry fact. It should not be inflated into a general exactly-once guarantee for every lower-level failure mode unless implementation conformance provides that stronger semantics.

#### `may_repeat`

The logical request may be attempted more than once.

Downstream duplicate invocation must therefore be considered possible.

#### `unspecified`

No retry fact is available.

`idempotency_key_propagation` links the outbound request's key fields to upstream logical identity.

### Result lineage

A request effect's synchronous result is `target.input`'s declared `result` (§8.1): executing the effect yields exactly the `Result<Ok, Err>` the targeted input returns. The schemas are not redeclared on the effect; an execution site binding the result observes that type.

Whether repeated executions observe the *same* result is a separate question with one V1 answer: the bound result is replay-stable exactly when the request instance is class-fixed, the effect's schema is the targeted input's schema, and the target operation declares an idempotency requirement keyed from the targeted input with `result: replay_consistent` that is itself proven (§9, §18). Payload-equal duplicates then fall into one class of that requirement, which returns the same variant and a replay-equivalent payload to each of them. Idempotency-key propagation remains lineage only; it proves neither duplicate safety nor result consistency.

### Duplicate request

A duplicate execution of a request effect invokes the target again, and nothing admits invocation multiplicity by default — the asymmetry with duplicate publication is deliberate: a request identity on the target input fixes payload consistency, but only a mechanism collapses invocations. The duplicate is safe exactly when the instance is class-fixed, the effect's schema is the targeted input's schema, and the target operation declares an idempotency requirement, keyed from the targeted input, that is itself proven: payload-equal duplicates then fall into one class of that requirement, which collapses them to the work of a single logical invocation. V1 computes these mutually dependent verdicts as a greatest fixpoint: a cycle of requirements that each collapse the others' duplicates is proven, by the minimal-counterexample argument of §9, and such proofs are marked coinductive.

## 13.3 External effect

An external effect marks a boundary beyond which Conseqa does not inspect implementation structure.

`name` is descriptive.

Because the checker cannot analyze the external implementation, its idempotency behavior is supplied as an explicit assumption.

### `IdempotencyGuarantee::unspecified`

No deduplication fact is available.

### `not_deduplicated`

Repeated execution is not deduplicated at this external boundary.

A retry/duplicate path reaching such an effect is therefore potentially observably unsafe for an upstream idempotency requirement.

### `deduplicated_by`

The external boundary guarantees deduplication for executions sharing the declared idempotency key.

The guarantee is scoped to equality of that logical key. It does not imply ordering, transactionality, or deduplication across different keys.

For an upstream idempotency requirement, a duplicate execution is safe when every component of the declared key is replay-stable relative to the governing key (§18): all attempts then execute under one key, and the boundary collapses them. No instance condition is needed, since the guarantee is scoped to key equality alone.

For a **result-bearing** external effect, the guarantee additionally fixes the interaction's terminal result. Equal evaluated keys identify one logical external execution; individual attempts are concrete executions made while that logical execution has not yet terminally resolved. Beyond suppressing duplicate logical work, `deduplicated_by`:

- does not let a retryable error outcome establish the terminal logical result;
- fixes the logical execution's terminal result once it reaches one — after the first terminal `Ok` or terminal `Err`, every subsequent same-key execution observes the same variant and a replay-equivalent payload.

Duplicate-work collapse and terminal-result stability are two consequences of the same declaration; no separate external result-replay guarantee exists. A boundary that performs the work once but answers a duplicate with a distinct response — `Ok(original)` then `Err(AlreadyProcessed)` — does **not** conform unless the modeled boundary abstracts the duplicate response back into the original logical result; expose the distinct duplicate response as the modeled result and the boundary must not be declared `deduplicated_by`. For a resultless effect (`result: null`), the meaning is unchanged: same-key logical work is deduplicated, and there is no result-replay component. As every implementation guarantee, conformance is an obligation on the boundary (§1.3); the checker consumes the declaration and cannot inspect the boundary.

### `ExternalEffect.result`

Because Conseqa cannot inspect beyond the boundary, an external effect may declare the synchronous result the boundary returns:

```yaml
result:
  ok: schema.ChargeAccepted
  err:
    schema: schema.ChargeDeclined
    disposition: terminal
```

Absent (`result: null`), no synchronous result is modeled and an execution site may not bind one.

The declaration carries the result's shape and the error's disposition, and combines with the effect's idempotency guarantee. For a result-bearing `ExternalEffect`, `deduplicated_by { key }` identifies one logical external interaction for equal evaluated keys and, in addition to suppressing duplicate logical work, fixes the interaction's terminal logical `Result`: after the first terminal `Ok` or terminal `Err`, every subsequent same-key execution observes the same variant and a replay-equivalent payload. Retryable `Err` outcomes are attempt-level, nonterminal outcomes and do not establish the logical interaction's terminal result; an `Err` with unspecified disposition provides no usable terminality fact.

Relative to a governing key, a bound external result is therefore replay-stable — per observed variant — when the effect declares `deduplicated_by`, every component of its key is replay-stable, and the observed variant is terminal: `Ok` by definition, `Err` under a declared `terminal` disposition (§16, §18). A retryable or unspecified `Err`, a boundary declared `not_deduplicated` or with an unspecified guarantee, or an unstable key leaves the observation unusable as a replay-stable root, and a decision resting on it is not established to replay — an honest gap the checker reports rather than a fact it assumes. `not_deduplicated` does not say repeated executions return *different* results; only that the guarantee is unavailable.

---

## 14. Effect intents

### The intent binding

An effect intent is a **logical transaction artifact** describing an intended effect execution: a captured logical effect instance awaiting execution.

There is no standalone intent declaration. An intent exists because a transaction site establishes it, and the site introduces the operation-local **binding** under which the artifact is available to later control:

- an explicit `establish_effect_intent` transaction step (below); or
- a `transition` transaction step, which binds one intent per side effect of the applied transition (§22).

An effect intent is not inherently synonymous with a durable database record. It implies no invisible independent executor or independent rediscovery mechanism: execution happens only through an `execute_effect_intent` program step, and retry availability only through the §17 routes.

### `EstablishEffectIntent`

An `establish_effect_intent` transaction step declares an effect contract, constructs one concrete logical effect instance from `values`, and atomically establishes that captured instance as the `EffectIntent` artifact named by `bind`:

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

The step simultaneously:

1. declares the effect contract, identified by `effect_id` — the stable identity of the captured inline effect occurrence (§7), not a lookup into any registry;
2. constructs the concrete logical effect instance according to `values`, the instance's provenance declaration;
3. establishes that exact instance as the artifact bound by `bind`.

The intent binding is not the effect declaration: `bind` names the artifact, `effect_id` names the captured logical effect site. The contract's own value references and the instance derivation are evaluated in the enclosing transaction context at this step, so they may use preceding transaction reads and outputs where the usual rules permit them (§13, §18).

If the intent is deterministically derived from replay-stable provenance and the establishing transaction is naturally replayable, a retry may reconstruct the same logical intent without requiring the intent payload itself to have been durably materialized.

If the establishing transaction is explicitly `DeduplicatedBy { key }`, the exact intent produced by the first successful logical commit is retained with that commit and recovered when the transaction step is encountered again under the same key.

### `ExecuteEffectIntent`

A program step executing an intent performs or attempts the work represented by the logical intent available to the current invocation. It consumes the **definitely available** binding (§16) and executes the exact captured instance.

`ExecuteEffectIntent` is the modeled synchronous execution authority for the intent; `ExecuteEffectIntentAsync` (§16) is the additional asynchronous one, initiating the same exact captured instance without waiting for completion. Intent establishment alone does not execute the underlying effect, and the async authority alters no intent durability, replay, or recovery semantics.

The effect instance was already constructed when the intent was established, so `ExecuteEffectIntent` declares no derivation and must never recompute or replace the intent's values.

It may bind the effect's synchronous result under `bind`, exactly as a direct execution does (§13): a request intent yields its target input's result, an intent of an external effect yields the declared one, an intent of a publication yields none and may not bind. Binding a result adds no outgoing-value derivation; the instance stays the one the transaction captured.

Reconstructing or recovering the same intent does **not** prove that repeating the external effect is safe, and executing it implies no exactly-once effect execution. A crash after an external effect succeeds but before completion is durably known may still lead to another effect attempt. Effect-level idempotency/retry semantics must handle that uncertainty.

---

## 15. Transaction outputs and request results

### The transaction-output binding

A transaction output is a logical transaction artifact shaped by a declared schema: a typed value a transaction deliberately exports into the enclosing operation's control.

It represents **data** — a reservation id, a selected account, remaining stock, a routing decision, authorization metadata, a normalized version of the input. Later control may use it for branching, effect construction, another transaction, or terminal result construction. Its single meaning is:

> This transaction deliberately exposes this typed logical value to the enclosing operation.

There is no standalone output declaration: the `establish_transaction_output` binder declares in one place the artifact's binding, its schema, its producer transaction and step, and its derivation.

A transaction output does **not** imply an operation result, success or failure, effect execution, idempotency, database storage, or memoization. It is semantically separate from transaction idempotency: establishing an output does not, by itself, prevent the enclosing transaction from executing or committing again. It is not inherently synonymous with a durable database record; its logical availability after retry may come from deterministic reconstruction or from durable retention by an explicitly keyed transaction commit (below).

A transaction output is intentionally generic and is not a `Result`. Outputs remain schema-shaped; `Result<Ok, Err>` is reserved for request results and synchronous effect results (§8.1, §13). If a future architecture requires result-typed outputs, the type model is to be extended deliberately rather than by coupling that concern into the output binder.

### `EstablishTransactionOutput`

The transaction step `establish_transaction_output` declares and establishes an output within the surrounding transaction execution:

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

For `EstablishTransactionOutput(bind, schema, D)`, the transaction:

1. constructs a value shaped by `schema`;
2. declares its provenance through `D`, evaluated in the transaction context at that step — it may reference operation-level values, artifacts available on entry, outputs established earlier in the same transaction, and preceding `transaction_read` bindings under the §18 rules;
3. establishes the artifact atomically with the transaction commit;
4. makes `bind` available to subsequent operation control after successful execution or commit recovery.

### Transaction-output replay

The two routes of §17 apply unchanged. If the transaction is naturally replayable and `D` is replay-deterministic, a retry reconstructs the same `O` without independent durable storage (route A). If the transaction is `DeduplicatedBy { key }`, the exact output produced by the first successful commit is retained with `Commit(T,K)`; a retry resolves the commit, restores `O`, and does not recompute `D` (route B). The same recovery rule applies to `EffectIntent`:

```text
Commit(T,K)
    |
    +-- TransactionOutput O
    |
    +-- EffectIntent I
```

### Transaction encapsulation

`transaction_read` remains transaction-local (§11, §18). If information observed or computed inside a transaction must influence later operation control, it must be explicitly exported through a transaction output:

```text
transaction-local observation
        |
        | establish_transaction_output
        v
operation-visible value
```

Effect intents cross the same boundary for a different reason: they carry captured executable work (§23).

### Request results: `return` and `complete`

There is no response declaration. A request invocation terminates directly with its input's declared result, constructed at the terminal from the values available there:

```yaml
- kind: return
  request: input.create_order.request
  outcome:
    kind: ok
    values:
      kind: deterministic
      from:
        - source: transaction_output:output.create_order
          path: order_id
```

`return` names an operation-owned request input `R` with `R.result = Result<OkSchema, ErrSchema>`; `outcome: { kind: ok, values }` constructs an `OkSchema` payload from `values`, and `outcome: { kind: err, values }` an `ErrSchema` payload. A subscription input cannot be a `return` target. Unknown provenance is declared as `values: { kind: unspecified }`, never omitted.

`complete` terminates an execution that returns nothing, as is natural for a subscription-driven operation.

Terminal result replay consistency is proven from path and variant stability plus the ordinary provenance of the terminal derivation (§9, §16); no privileged result artifact intervenes between a transaction and the result it informs.

---

## 16. The operation program and transaction artifacts

### `OperationBlock.steps`

The program is an `OperationBlock`: a sequence of steps executed in order. A decision step nests further blocks. The step kinds are:

- `transaction`
- `execute_effect`
- `execute_effect_async`
- `execute_effect_intent`
- `execute_effect_intent_async`
- `join_all`
- `race`
- `match_result`
- `branch`
- `return`
- `complete`

No explicit recovery step exists for a transaction output or an effect intent; recovery is what re-encountering a keyed transaction does (§17). No loop or general task primitive is introduced by the asynchronous steps; asynchronous execution is permitted for effects and effect intents only — never for transactions, blocks, branches, or arbitrary program fragments — so the program language acquires no general shared-state concurrency semantics. The analyzer may lower the block structure to a control-flow graph, and the asynchronous steps additionally to a partial-order execution graph of effect-start and effect-completion events; the DSL declares the structure.

### `transaction`

Declares and executes one atomic transaction at that point in the operation program, or resolves its prior keyed commit. The step **is** the transaction: it carries the stable logical `id` together with the data-model boundary, isolation guarantee, idempotency guarantee, and ordered body (§17). The ID identifies the inline transaction for keyed commit recovery, conformance, proof evidence, and diagnostics; it is not a reference to another declaration.

For an ordinary transaction, reaching the step means executing the transaction body.

For a transaction explicitly `DeduplicatedBy { key }`, if the same logical commit already exists, the step resolves that prior commit instead of committing the body again and restores the artifacts retained by that commit. The durable identity is conceptually `Commit(operation, id, K)` — which is why the ID, not the step's location, carries it: moving the step must not silently change durable commit identity.

### `execute_effect`

Declares one logical effect contract and one concrete execution site.

For `ExecuteEffect(effect_id, E, D)`, reaching the step:

1. constructs one logical instance of the inline contract `E`;
2. obtains its values according to derivation `D`;
3. executes that effect instance;
4. optionally binds the effect's synchronous result under `bind` (§13).

`effect_id` identifies the inline effect occurrence itself; it is not a lookup into an operation-level effect registry, and it must be unique within the operation (§7). The distinction stands: `effect` is the logical contract, `values` the provenance of the concrete instance constructed here.

`values` declares the provenance of the complete logical effect instance, for every effect kind, using the same `Derivation` vocabulary as transaction-level provenance declarations (§18). Unknown provenance must be declared explicitly as `unspecified` rather than omitted.

Because the step occurs at program level rather than inside a transaction, the derivation — and the inline contract's own value references (§13) — are evaluated in the operation-level value context immediately before the step. Neither may reference `transaction_read` bindings, which are local to a transaction execution (§18).

For natural replay idempotency, the analyzer must prove `D` replay-deterministic: `deterministic` plus replay-stable provenance roots (§18) establishes that a retry constructs the same logical effect instance. Effect payload stability, duplicate-execution safety, and effect-result stability remain three separate proof obligations. Validation checks only that the derivation's references and field paths are structurally coherent; replay stability is solver responsibility.

A direct effect execution is not automatically durable or retry-safe. The verifier must use the effect's retry/deduplication environment and the invocation's possible failure/retry paths.

A transition side effect is never executed directly: it is established as an intent by the transition and executed through `execute_effect_intent` (§22).

Ordinary sequential step order establishes `control(step_i) < control(step_i+1)`. For a synchronous effect execution, **completion is part of the step** and therefore precedes the next step: `execute_effect A` followed by `execute_effect B` establishes `complete(A) < start(B)`. This is normative: authors who write `execute_effect` keep the existing guarantee that the execution and any bound result occur before subsequent control, and no analyzer may infer asynchronous overlap from adjacent synchronous steps. Async behavior exists only where explicitly declared through `execute_effect_async`.

### `execute_effect_async`

Declares one logical effect contract and one asynchronous execution site.

For `ExecuteEffectAsync(H, effect_id, E, D)`, reaching the step:

1. declares the inline effect contract `E`;
2. constructs one concrete logical effect instance according to `D`;
3. initiates execution of that instance;
4. establishes asynchronous handle `H`;
5. permits control to proceed without waiting for the execution to complete.

Contract and instance-construction semantics are otherwise identical to `execute_effect`: `effect_id` has exactly its existing meaning, `D` has exactly the existing `Derivation` semantics and is evaluated when the launch is reached, so the effect instance is **fully determined at launch** — later control or observations cannot change the launched payload, and synchronization never reevaluates `D`.

For `execute_effect_async A` followed by step `B`, the program establishes only `start(A) < start(B)` — **not** `complete(A) < start(B)` — so A may overlap B. No completion relationship exists until a synchronization step declares one. Conseqa asserts nothing about the scheduling mechanism: no thread, task, future, event-loop, CPU-parallelism, or OS-scheduling semantics — only that the causal program does not require A to complete before subsequent control proceeds. L0 declares logical overlap eligibility; whether concrete runtime resources allow physical parallelism is an L1 and simulation question.

The step binds no result (§13): `bind` at an async launch is invalid by construction, and the launched effect's result surfaces only at a synchronization step. Initially, direct asynchronous execution is legal for exactly the effect kinds legal for ordinary direct execution — publication, request, external; a future effect kind must explicitly declare whether direct asynchronous execution is legal rather than inheriting it from the `Effect` enum.

An asynchronously launched effect participates in idempotency analysis as soon as its launch site is reachable, exactly as a synchronous execution would: not waiting for completion removes nothing from the operation's side-effect blast radius, and on operation retry the launch site may initiate the effect again, judged by the same instance-stability and downstream idempotency rules. Async launch is not a durable checkpoint, and none of the async primitives implies deduplication, exactly-once execution, retry suppression, replay stability, or effect-result consistency.

### `execute_effect_intent`

Executes the referenced logical effect intent currently available to the invocation, and optionally binds its result (§14). The step is synchronous: the exact captured effect is executed and any declared result is available before following control. Authors opt into non-blocking execution only through `execute_effect_intent_async`.

The intent may have been produced by an earlier transaction in this invocation, reconstructed by naturally replaying that transaction, or recovered from an explicitly keyed transaction commit.

### `execute_effect_intent_async`

Initiates execution of the exact logical effect instance captured by a definitely available `EffectIntent` binding, establishes an asynchronous handle, and permits control to continue without waiting for completion.

There is no derivation: the intent's instance was fixed when the intent was established. The step is an additional execution authority for the intent (§14) and alters no `EffectIntent` durability, replay, or recovery semantics — natural reconstruction and keyed-commit recovery follow the existing rules, recovery of the intent does not make repeated async execution safe, and the operation terminal does not establish that the initiated effect completed.

### Async handles

An asynchronous launch introduces an operation-local **handle** identifying that particular in-flight effect execution occurrence, consumable only by synchronization steps.

The handle is a semantic synchronization artifact, not application data: it has no schema, cannot be persisted, cannot be used as an idempotency key, cannot be returned from an operation, cannot be placed in a transaction, and does not identify the logical effect itself — `effect_id` does that (§7). It is not a `ValueSource`; application expressions cannot inspect it, and no decision can ask whether it has completed — there is no `is_complete(H)`, `poll(H)`, or `timeout(H)`.

Handles follow the normal operation-local definite-availability discipline (`AsyncHandleNotAvailable`): a handle launched inside one arm of a decision is not available at a synchronization only some paths reach it from. As with existing bindings there are no forward references, no rebinding, no shadowing, and no implicit merge. Every handle ID is unique within the operation and participates in the global ID uniqueness discipline.

One handle represents one execution occurrence produced by one traversal of its launch site. On operation retry the same launch site may be encountered again; the handle declaration does not deduplicate those executions across retries — it is an invocation-local synchronization artifact.

### `join_all`

A synchronization barrier over asynchronous executions:

```yaml
- kind: join_all
  handles:
    - handle: async.profile
      bind: result.profile
    - handle: async.orders
      bind: result.orders
```

`join_all(H1..Hn)` does not complete until every referenced execution has completed: it establishes `complete(Hi) < continuation` for each joined handle, and **no relative completion order among them** — `async A; async B; join_all; C` supports `A < C` and `B < C` but neither `A < B` nor `B < A`. The barrier does not serialize its members: it implies no ordering, no non-overlap, and no shared execution resource among them. Textual launch order alone establishes no completion or externally observable effect order; where an operation requirement depends on one effect completing before another, a causal synchronization edge must establish it, so sequential `execute_effect A; execute_effect B` and `async A; async B; join_all` are **not interchangeable for ordering proofs**.

Each entry may bind its effect's ordinary `Result` when the underlying effect is result-bearing; a result-less effect (a publication, an external effect declaring no result) must not declare one (`EffectHasNoResult`), and a result-bearing effect may be joined without a binding when its result is deliberately ignored. The result type is inferred from the underlying contract and never restated. `join_all` constructs no aggregate value: each bound result is an independent binding under the ordinary result-binding rules — no tuple algebra, collection result type, or combined error model exists — and subsequent control inspects them through ordinary `match_result` steps in whatever causal order it declares.

An `Err` result is still a completed effect interaction: `join_all` does not short-circuit because one candidate returned `Err`; it keeps waiting for the rest, after which each result may be inspected. A fail-fast primitive would require separate semantics and is not implied.

A `join_all` over one handle is valid and acts as the minimal await; an empty `join_all` is invalid (`EmptyJoinAll`). Joining a publication or other result-less effect is meaningful — it establishes that the attempt completed — it simply binds nothing.

A result binding produced at `join_all` has the same logical result the effect would have exposed synchronously. Its replay stability therefore rests on the same facts — instance stability, target result-replay guarantees, external deduplication, error disposition (§18) — and the presence of other effects in the same barrier does not alter it.

### `race`

A first-completion barrier over asynchronous executions:

```yaml
- kind: race
  handles:
    - async.primary
    - async.replica
  bind: result.read
```

`race(H1..Hn)` completes when at least one referenced execution completes: for the winning completion `W` it establishes `complete(W) < continuation`, and **nothing about any other candidate**. A race requires at least two handles (`RaceRequiresTwoHandles`); the winning handle's identity is not exposed as an application value — no handle comparison, handle matching, dynamic task identity, or winner-dispatch control enters the expression language.

`race` means **first completion, not first success**: if A completes first with `Err` and B later completes with `Ok`, a result-binding race observes A's `Err`. A `first_ok` primitive would require distinct semantics and does not exist.

`race` provides **no cancellation** — this is normative. If A wins, Conseqa SHALL NOT infer that B was cancelled, aborted, did not execute, cannot later complete, or cannot produce side effects. Every candidate was initiated and remains part of the operation's side-effect cascade; a concrete implementation may attempt to cancel losing work, but Conseqa provides no cancellation guarantee, and a downstream load model must still account for every candidate's work even though the response critical path may approximate the first completion. A race loser is never pruned from the effect graph merely because its result is unused.

Where `bind` is present, every raced handle must refer to a result-bearing effect and all candidates must expose **the same logical result contract** (`RaceResultContractMismatch`); the binding carries the exact result of whichever candidate completes first. Different effect kinds may be raced under a binding only when their contracts are identical — result-shape equality does not make their business behavior equivalent; the author is responsible for a meaningful architecture. A `race` without `bind` imposes no compatibility requirement and may synchronize result-less or heterogeneous effects: it means only "continue after the first referenced execution completes".

If the winner is not statically determined, no particular candidate is established to precede the continuation: `race(A,B); C` does not prove `A < C`, because B may win.

`race` does not consume or invalidate its handles: a later `join_all` may still wait on them — a race followed by a join distinguishes a first-completion dependency (the latency-sensitive continuation) from an all-completion dependency (work requiring both attempts). If a candidate already completed, joining it again requires no new effect execution; synchronization never re-executes an effect.

A race-bound result introduces scheduling nondeterminism — which candidate completes first — so even when each candidate independently has replay-stable results, retries need not select the same winner. The verifier conservatively treats a race-bound result as **not established to be replay-stable**; a `match_result` or other replay-sensitive decision resting on it becomes an explicit replay obstacle. A future verifier may prove race-result stability by establishing that every possible winner yields replay-equivalent observable results; V1 does not attempt that equivalence proof, which is preferable to silently assuming deterministic scheduling.

### Asynchronous effects and terminals

An operation terminal does not implicitly join asynchronous executions. `async A; complete` is structurally meaningful: it establishes `start(A) < complete(operation control)` but not `complete(A) < complete(operation control)` — deliberate fire-and-forget, with no additional primitive. The terminal completes the declared synchronous control path only; with unresolved handles outstanding, Conseqa SHALL NOT infer that those effects completed, succeeded, failed, were cancelled, or will eventually complete, and no durability or eventual-completion guarantee follows from launch alone. Where durable rediscovery matters, the transaction/`EffectIntent` mechanisms must provide it — async execution is not durable rediscovery.

For a request operation, `race(A,B) -> R; return R` makes the request result causally dependent on the race winner only; the loser may remain in flight beyond the terminal, so request latency and the lifetime cost of all initiated effects are distinct quantities — a major reason asynchronous execution is represented explicitly. Where a source-driven input uses `acknowledge_on_success`, successful completion — and therefore acknowledgement — may occur while launched effects remain unresolved; source acknowledgement is never silently turned into an implicit async join.

Program reachability remains defined over synchronous control: no program step executes after a terminal, so `async A; complete; B` still makes B unreachable — a continuing asynchronous A does not make B reachable. Outstanding handles do not make an otherwise terminating path unterminated: they are launched side-effect executions, not additional control paths requiring terminals.

Async overlap may invalidate a serialization argument that depended on sequential execution within one invocation — two asynchronously launched effects cannot be assumed not to overlap. Operation-level `SerializedBy(K)` requirements, however, continue to concern separate logical operation invocations under their existing definition (§9): async effects within one invocation are not additional operation invocations, and the two domains remain distinct.

### `match_result`

Destructures a bound result into two arms:

```yaml
- kind: match_result
  result: result.charge_payment.card
  ok:
    steps: [...]
  err:
    steps: [...]
```

`result = Ok(v)` executes the `ok` block, `result = Err(e)` the `err` block. The match is exhaustive and mutually exclusive by construction; both arms are declared, though either may be empty.

Inside `ok`, `effect_result_ok:<result>` is available and the `err` payload is not; inside `err`, the reverse. **Variant payloads are arm-local**: neither survives the join after the match, even when the other arm terminates. Data that must be generally available after a match is exported through a transaction artifact instead — a transaction inside the arm establishing a transaction output — or the control is structured so the consumer sits inside the arm.

Success and failure of a synchronous interaction are expressed by `match_result` over a `Result`, not by a `branch` comparing a conventional status field. The two primitives are not overloaded to do each other's job.

### `branch`

An ordinary control decision over modeled values:

```yaml
- kind: branch
  condition:
    kind: eq
    value:
      source: input:input.checkout
      path: region
    equals: CA
  then:
    steps: [...]
  otherwise:
    steps: [...]
```

The `then` block executes when the condition holds. `otherwise` is optional; absent, the branch falls through to the following step when the condition does not hold.

`Condition` is deliberately small and structurally exposes every value the decision depends on, so replay analysis can judge a decision without an expression language:

- `eq { value, equals }` — equality of a value reference against `equals`, which accepts the selector-value surface of §19: a map is another value reference, a plain scalar is a literal;
- `and { conditions }` — every nested condition holds;
- `not { condition }` — the nested condition does not hold;
- `unspecified` — the model provides no fact about how the decision is made.

`eq`, `and`, and `not` are **deterministic functions of their references**: given equal values for every root, the decision takes the same arm. `unspecified` declares no fact and is never deterministic; a condition containing it anywhere is not. §1.1 governs it as it governs every other `unspecified`.

### `return` and `complete`

The terminals (§15). `return` constructs the named request input's declared result from `outcome`; `complete` returns nothing. Each ends its block: an invocation reaching it has finished.

### Program validation

Validation establishes that the program is structurally coherent. It performs no replay proof. The rules:

1. **Termination.** Every reachable path ends at a `return` or `complete` (`ProgramNotTerminated`). A block whose last step is a decision terminates only if every arm of that decision terminates; a `branch` without `otherwise` never does.
2. **Reachability.** No step follows a terminal — or a decision whose every arm terminates — in its block (`UnreachableProgramStep`, reported for the first dead step of a block).
3. **Definite artifact availability.** A transaction artifact — transaction output or effect intent — may be consumed only at a program point where a transaction on **every** path reaching that point establishes or recovers it (`TransactionArtifactNotAvailable`). Consumers are: an `execute_effect_intent` of the intent; a `transaction_output` reference in an effect derivation, a branch condition, a `return` outcome, another transaction's commit key or body, or an effect contract's own roots at the site where they are evaluated (§13) — an external deduplication key, propagation components. Inside one transaction, a reference to an output that transaction establishes is satisfied by step order.
4. **Definite result assignment.** A result binding may be matched or referenced only where a step on every path reaching the point has bound it (`EffectResultNotBound`). The binding steps are the synchronous effect executions and the synchronization barriers: an asynchronous launch binds no result, so a result from an asynchronously executed effect is **not** considered bound merely because its launch occurred — it becomes available only where a `join_all` or `race` produces it. This is fundamental to async soundness.
5. **Variant scope.** `effect_result_ok:<r>` is legal only inside the `ok` arm of a `match_result` on `r`, `effect_result_err:<r>` only inside its `err` arm (`EffectResultVariantOutOfScope`). Field paths resolve against the variant's schema.
6. **Result-binding contracts.** A binding is declared only by a site observing a result-bearing effect (`EffectHasNoResult`): a request, whose contract resolves through its target input; an external effect declaring `result`; never a publication. For a `join_all` entry, the underlying effect is the joined handle's; for a `race`, every candidate must be result-bearing and all candidates must expose the same logical result contract (`RaceResultContractMismatch`) — absent `bind`, no compatibility requirement is imposed.
7. **Return target.** `return.request` names an operation-owned **request** input (`InvalidInputKind` for a subscription). The outcome's derivation roots must be definitely available under rules 3–5.
8. **Identity.** Every inline `Transaction.id`, inline `effect_id`, binding ID, and async handle ID is unique (`DuplicateId`, §7); an `execute_effect_intent` or `execute_effect_intent_async` names an intent binding produced by this operation's program; every value reference respects §11 scope.
9. **Definite handle availability.** A synchronization step waits only on handles bound by an async launch on every path reaching it (`AsyncHandleNotAvailable`); a handle is consumed by nothing else.
10. **`join_all` shape.** The handle list is non-empty (`EmptyJoinAll`); every referenced handle exists and is operation-owned; no handle appears twice in one `join_all` (`DuplicateSynchronizationHandle`); every declared result binding is unique; a binding is declared only for a result-bearing underlying effect, its type inferred from the contract and never restated.
11. **`race` shape.** At least two handles (`RaceRequiresTwoHandles`); every referenced handle exists and is operation-owned; no handle occurs twice in one race (`DuplicateSynchronizationHandle`); the result binding, when present, is unique and subject to rule 6's compatibility requirement.

An `execute_effect_async` additionally applies every structural rule already applicable to synchronous `execute_effect` — effect target resolution, schema compatibility, derivation validity, value-reference scope, idempotency-key reference validity, `effect_id` uniqueness, result-contract resolution — plus handle uniqueness and the requirement that the effect kind permits direct async execution. An `execute_effect_intent_async` requires an existing, definitely available intent binding and a unique handle, and resolves the underlying effect contract to determine result compatibility for later synchronization.

Rules 3–5 and 9 are a **forward definite-availability analysis** over the block structure, with producers discovered inline:

```text
before Read(bind=r): r unavailable
after Read(bind=r):  r available inside the same transaction
transaction exit:    r unavailable

Available(entry)               = {}
Available(after T)             = Available(before T) ∪ Artifacts(T)
Available(after E→r)           = Available(before) ∪ {r}
Available(after async launch H) = Available(before) ∪ {H}
Available(after join_all)      = Available(before) ∪ ResultsBoundBy(join_all)
Available(after race → R)      = Available(before) ∪ {R}
Available(join)                = ∩ Available(each predecessor that falls through)
```

No individual candidate result becomes available merely because it participated in a race; only the race's own binding does.

`Artifacts(T)` is the transaction's explicitly bound outputs, its explicitly bound intents, and the transition intents it binds (§22). A predecessor arm that terminates imposes no constraint on the join. Variant selection is not joined at all: an arm selects its variant for its own extent only.

Bindings exist only after their producer — there are **no forward references**. Using an output before its binder's transaction, executing an intent before the transaction that binds it, or matching a result before the step that binds it is a use-before-bind / not-definitely-available error; the validator never resolves an operation-wide declaration and hopes control eventually produces it. A producer existing somewhere in the operation does not make its binding globally available: a binding may be consumed only where every falling-through path to the consumer has produced it, subject to the stronger binding-kind scopes — transaction reads never leave their transaction, and result variant payloads remain local to their `match_result` arm.

A binding has one syntactic producer and is never merged with a differently produced value from another arm: this model has no phi or merge construct. If two falling-through arms must produce different values for a later common consumer, keep the consumer inside each arm or restructure the program.

Operation requirements live outside the causal execution body, so program-produced bindings are not in scope inside `Operation.requirements`: idempotency and recoverability governing keys define the invocation equivalence class from the triggering boundary, never from values the invocation later produces (§12).

### Paths and path admission

An invocation traverses one **synchronous control path** through the program: the linear sequence of its steps, the arm taken at each decision, and the terminal reached. Asynchronous effect steps on the path initiate executions whose lifetimes overlap later control, and synchronization steps add explicit completion dependencies; the path remains the acyclic control skeleton those lifetimes hang off. Verification analyzes the program path by path — a path is a linear sequence of steps plus the decisions that selected it — so the forward replay pass of §18 applies to each path unchanged, and what a decision rests on is judged where it is taken. For simulation, a path with async steps lowers to a partial-order graph: each launch is an effect-start event with no immediate completion dependency on the next step, `join_all` a barrier requiring every corresponding completion event, `race` a barrier enabled by the first; Conseqa supplies the causal structure, the simulator the quantitative facts.

A path is **admitted for input `i`** iff its terminal is `complete`, or `return` for `i`. A path returning another request input's result is not one an invocation of `i` completes. Admission is terminal-based; the DSL adds no explicit entry or path-admission concept associating a triggering input with a control entry. That association is open question 10 (§27) and is deliberately not resolved by inventing one: an operation with several request inputs distinguishes their paths by the `return` each takes, and a subscription-triggered invocation is admitted to every path ending at `complete`.

A path that falls off the end of its block with no terminal is rejected by validation (rule 1). Verification, which is not promised a valid model, admits such a path conservatively so its work is still analyzed, and recoverability records it as an obstacle.

### Artifact context per path

Along a path, the invocation carries an abstract artifact context:

```text
PathContext
    TransactionOutput O -> logical value, with its replay route
    EffectIntent E      -> logical effect intent, with its replay route
    result r            -> bound result, with its replay judgment
    AsyncHandle H       -> launched effect execution, with the judgment
                           its result would carry if joined
```

This context is semantic bookkeeping, not a DSL workflow construct. The DSL exposes no handle analysis state; available/launched versus known-completed is analyzer bookkeeping used to determine synchronization and result availability.

Artifact availability may arise from:

1. production earlier on the current path;
2. deterministic reconstruction during natural transaction replay; or
3. recovery from a prior `Commit(T,K)` for an explicitly deduplicated transaction.

Transaction-read results are excluded: they remain local to the transaction execution that produced them. Result bindings enter the context at the step that binds them — a synchronous execution, or the `join_all` / `race` barrier that produces them — with their replay judgment (§18); they are observations, not artifacts, and no route reconstructs or recovers them. A result bound at a `join_all` carries the judgment computed at its launch, where the instance and any external deduplication key were evaluated; a result bound by `race` is conservatively judged unstable, its candidate set recorded as the binding's possible producers — the binding's producer is the race step, not any one statically identifiable launch.

### Decision replay

A retry traverses declared control. Whether it takes the same arm at a decision is a fact the checker establishes or records as a gap. Relative to a governing key (§12), a decision **replays** — every attempt in a class takes the same arm — when:

- for a `branch`: the condition is deterministic (not `unspecified` anywhere) **and** every root it observes is replay-stable under §18. The same roots then yield the same predicate value; or
- for a `match_result`: the matched result is replay-stable under §18, so the variant is fixed across the class.

Otherwise the checker reports the decision as **not established to replay**, naming the gap: the condition is `unspecified`; a condition root is unstable; the result is not bound before the decision on this path; or the result is unstable in the taken arm's variant — its instance not class-fixed, its request's schema not the target's, its target declaring no replay-consistent requirement for the input or one that is unproven, an external boundary that is `not_deduplicated`, carries no deduplication fact, deduplicates by an unstable key, or whose observed `Err` is retryable or of unspecified disposition (§13.3), or a result bound by `race`, whose winner is scheduling nondeterminism (§16). Instability is not proven; a different arm on retry may be legitimate. What that means for each obligation is stated in §9: an obstacle for idempotency and result replay, never for recoverability.

### Step locations

Program steps carry no ids. Diagnostics, proofs, and obstacles name a step by its **location**: one hop per nesting level, each the one-based position in its block and, for every level but the last, the arm entered beneath it. `3.ok.1` is the first step of the `ok` arm of the third top-level step; `2` is the second top-level step. A path is named by the arms it takes, as `ok(result.charge_payment.card) › then(step 3)`, or as "the program" when it has no decisions. An obstacle at a step is reported once per site however many paths share the prefix reaching it, since those paths reach the step with the same context.

---

## 17. Transactions, replayability, and explicit idempotency

### `Transaction`

A transaction is one atomic commit/abort unit, declared inline at the program step that executes it (§16).

`id` is the transaction's stable logical identity: unique within the operation, carried by the inline declaration itself, and the identity under which a keyed commit is durably recognized. Its object accesses are interpreted against its declared `data_model`. Its steps are logically ordered as written.

Atomicity does not imply serializability, and serializability is a statement about committed transactions only — it is not to be read as any stronger object-history property (§5).

Framework transaction artifacts established by the transaction — transaction outputs and effect intents (§23) — participate in the same logical atomic boundary as application-state mutations.

### `data_model: <id>`

The transaction operates against the identified logical transactional state boundary.

Object reads/writes/locks/inserts/deletes/transitions must refer to objects belonging to that data model.

### `data_model: null`

Permitted when the transaction performs no application `DataObject` access and only produces or consumes framework transaction artifacts.

It must not be used to imply atomic application-object access with no declared transactional boundary.

### Transaction idempotency guarantee

A transaction should expose an `IdempotencyGuarantee` independently of any transaction-output or effect-intent declaration.

#### `unspecified`

No explicit keyed transaction-commit deduplication fact is available.

The analyzer may still prove **natural replayability** from the transaction's declared semantics.

#### `not_deduplicated`

The architecture explicitly declares that the execution environment provides no keyed transaction-commit deduplication for this transaction.

The analyzer may still prove natural replayability.

#### `deduplicated_by`

For transaction declaration `T` and evaluated key `K`, the execution environment guarantees a durable logical commit identity:

```text
Commit(T,K)
```

At most one logical execution of `T(K)` may successfully commit.

On the first successful execution, application state, `Commit(T,K)`, and the exact transaction artifacts produced by that execution commit atomically.

If `Commit(T,K)` already exists on a later encounter:

- the transaction body is not committed again;
- the prior logical commit is resolved;
- artifacts retained by that commit are restored to the invocation artifact context.

Concurrent attempts with the same `(T,K)` must not both successfully commit.

This is a concrete implementation/conformance obligation, not a claim that arbitrary transaction code is mathematically idempotent.

### Natural replayability

Natural replayability is derived, not declared with a boolean.

A transaction is naturally replayable only when the verifier can establish that another execution for the same logical invocation can safely reproduce the same logical transaction outcome and any artifacts required by later program steps.

This is stronger than merely showing that a second commit cannot happen.

A one-shot guard that makes a second attempt abort may establish at-most-once commit behavior while still preventing the program from reconstructing artifacts after a crash. Such a guard therefore does not, by itself, prove natural replayability.

V1 may use deterministic target/value provenance and mutation semantics where sufficient. If required facts are absent, natural replayability is `Unknown`.

### Artifact replay after a transaction

For an artifact required after a crash, V1 accepts either:

```text
A. reconstruction
   establishing transaction naturally replayable
   +
   artifact derivation replay-deterministic

OR

B. recovery
   establishing transaction DeduplicatedBy(K)
   +
   artifact retained by Commit(T,K)
```

Otherwise the artifact's retry availability/consistency is not proven.

Route A's derivation roots and route B's commit-key components are judged by the replay-stability rules of §18. In particular, a commit key over roots that are not replay-stable earns no recovery route: attempts may evaluate different keys, address different commits, and each commit the body once.

### Isolation

The solver should use the following abstract semantics.

#### `unspecified`

No isolation fact may be assumed.

#### `read_committed`

Reads do not observe uncommitted writes from other transactions.

The verifier must still consider anomalies permitted by read-committed execution, including non-repeatable reads and concurrent read/modify/write races unless prevented by stronger facts such as locks, atomic mutation semantics, uniqueness, or serialization.

Read committed is not serializable.

#### `snapshot`

A transaction reads from one consistent committed snapshot for ordinary reads.

Snapshot isolation does not in general imply serializability; write skew and predicate-level anomalies must remain possible unless ruled out by additional facts.

#### `serializable`

Committed transactions admit an equivalent serial execution order.

Serializable does **not** by itself imply real-time precedence. It is a transaction-level fact and must not be promoted into an object-history guarantee (§5); no V1 verifier draws such an inference.

Serializable execution also does not imply that a transaction is replayable across separate invocation attempts.

### Transaction step order

The declared step sequence represents logical program order inside the transaction.

This is especially important for lock-order/deadlock analysis, transaction-read provenance, state transitions, and reasoning about when transaction artifacts are established relative to application state.

---

## 18. Deterministic derivation and transaction reads

### `Derivation`

The DSL declares opaque value computation through a small provenance vocabulary:

```rust
pub enum Derivation {
    Unspecified,
    Deterministic { from: Vec<ValueRef> },
}
```

`Deterministic { from }` means:

> The produced values are a deterministic function solely of the declared source values.

It does **not** assert that those source values are replay-stable.

The verifier separately determines replay stability of provenance roots.

Therefore:

```text
deterministic derivation
        +
replay-stable provenance
        ↓
replay-deterministic produced value
```

### Replay-stable provenance roots (V1)

Replay stability is judged relative to an operation and a governing key
`K` (§12):

> A `ValueRef` is **replay-stable** iff in every admitted execution,
> any two attempts in the same `K`-class that evaluate it obtain equal
> logical values.

The quantification is over evaluations: an attempt that crashes before
evaluating a reference imposes nothing, and evaluating an artifact
reference presupposes the artifact is available in the attempt's
context (§16).

A path `p` is *pinned by `K` in schema `S`* when some component of `K`
has a path canonically equal to `p` within `S` — equality of canonical
value paths after fragment expansion, since a fragment mapping asserts
semantic identity of the referenced value (§4).

The V1 rules. Stability is definitional, declared, or derived — never
assumed:

1. **Key components.** Every component of `K` is replay-stable: class
   membership requires their equality (§12).
2. **Literals.** A literal is replay-stable; this matters for selector
   provenance (§19).
3. **Identified triggering payload.** Let `i` be the triggering input.
   - `i` is a request declaring a keyed identity (§8.1) whose every
     field is pinned by `K`: every field of `i`'s payload is
     replay-stable.
   - `i` is a subscription whose topic declares a keyed message
     identity (§6), every admitted schema is mapped, and for each
     identity position a **single** component of `K` pins that
     position's field in **every** admitted schema: every field of
     `i`'s payload is replay-stable.

   Same-class attempts are then presentations of one logical stimulus.
   The per-position single-component clause is what carries key
   equality across schemas: with different components pinning a
   position in different schemas, class equality would relate each
   component to itself across attempts but never relate one message's
   identity to the other's.
4. **Recovered artifacts.** For a transaction `T` declared
   `DeduplicatedBy { key }` whose key components are all replay-stable,
   the contents of every artifact established by `T` — every
   transaction output and effect intent — are replay-stable: all
   attempts in a class address the same `Commit(T,K)`, which durably
   retains the exact artifacts of the single successful execution
   (§17 route B). A `transaction_output` reference to such an output
   is therefore stable at every point where the output is available.
5. **Reconstructed artifacts.** For a naturally replayable transaction,
   an artifact — transaction output or effect intent — whose
   establishment derivation is replay-deterministic is replay-stable
   (§17 route A), and a `transaction_output` reference to it is
   stable wherever it is available.
6. **Effect results.** A reference through `effect_result_ok` or
   `effect_result_err` to a bound result `r` is judged per variant.
   For a **request**, both variants are stable at once iff the
   instance is class-fixed — a direct execution with a
   replay-deterministic derivation, or an intent replay-available by
   route A or B — the schema is the targeted input's schema, and the
   target operation proves `result: replay_consistent` for that input
   (§9, §13.2): the class then sends one logical request into one
   class of the target, and receives one variant and a
   replay-equivalent payload back. For an **external** effect, a
   variant is stable iff the effect declares `deduplicated_by` over a
   key whose components are all replay-stable — equal keys identify
   one logical external interaction whose terminal result the
   guarantee fixes (§13.3) — and the referenced variant is terminal:
   `Ok` by definition, `Err` under a declared `terminal` disposition.
   A retryable or unspecified `Err` gains nothing from the guarantee;
   no instance condition applies, since result identity follows the
   key exactly as the work collapse does. A publication has no
   result. The judgments are made at the step that binds the result;
   a `match_result` rests on the judgment of the variant of the arm
   it takes (§16).
7. **Congruence.** A value produced by `Deterministic { from }` with
   every root replay-stable is replay-deterministic, and its uses
   inherit stability.
8. **Everything else is `Unknown`**: unidentified non-key input fields,
   fields of a non-triggering input, `state_machine_subject` state
   (always, in V1), `effect` payload roots, external effect results
   outside rule 6 — an undeduplicated or unstably keyed boundary, a
   retryable or unspecified `Err` — an artifact available by neither
   route, an artifact or result not in the path context at the point
   of reference, and `transaction_read` results, which additionally
   poison any natural-replay provenance closure that reaches them.

The `Unknown` cases are epistemic (§1.1): no rule establishes
stability; instability is not proven.

Why rule 3 requires a declaration rather than following from the key:
with `K = [input.idempotency_key]` and no declared identity, attempts
`{idempotency_key: k, amount: 100}` and `{idempotency_key: k, amount:
200}` are both admitted and share a class. A write derived
`deterministic_from(input.amount)` is deterministic yet produces
different values across the class. Only a boundary fact excludes the
conflicting attempt.

These judgments — stability, replay determinism, natural
replayability, artifact replay availability, result stability, and
decision replay — form one simultaneous induction, computed **per
path** of the program (§16) in a single forward pass in path order
and, within a transaction, step order: every rule consumes only roots
or facts established at earlier steps of that path, and
transaction-read dependence, the only backward-looking observation,
is excluded outright. Two paths sharing a prefix reach the prefix's
steps with the same context and reach the same judgments about them.

### Transaction read bindings

A `Read` binds a transaction-local observation under `bind` so later steps in the same transaction can reference fields from it through `ValueSource::TransactionRead`.

A transaction-read binding is an observation of transaction state, not a replay-stability guarantee. It exists only after the read, is visible only inside the same transaction execution, and never becomes a transaction artifact.

Validation requires that a transaction-read source:

- refers to a read in the same transaction;
- refers only to fields selected by that read; and
- is used only after that read in transaction program order.

### V1 read-dependent replay rule

V1 is deliberately conservative:

> If the provenance closure of a persistent mutation target, mutation value, or transaction artifact reaches a `TransactionRead`, V1 does not use that path to prove natural transaction replayability.

The result is `Unknown`, not necessarily `Violated`.

Determinism of the computation is insufficient. The value observed by the read may differ on retry even when no other process modified it.

In particular, a transaction can read a field and then deterministically write a function of that value back to the same object:

```text
Read A.counter -> r
Write A.counter = f(r.counter)
```

For deterministic `f(x) = x + 1`, the first execution may observe `5` and commit `6`, while the retry observes `6` and commits `7`. The computation is deterministic but the transaction is not naturally replayable.

A future solver may attempt a stronger invariance proof. Such a proof must account for both:

1. mutations by other admitted executions between attempts; and
2. the establishing transaction's own effect on the state it later re-reads.

For a read observation function `R` and transaction state transformation `T`, absence of external writers is insufficient; the solver may need to establish an invariant corresponding to `R(S) = R(T(S))` over the relevant admitted states, together with any required interleaving guarantees.

---

## 19. Object selectors and predicates

### `ObjectSelector`

A selector identifies which instances of one declared `DataObject` a transaction step addresses.

The selector is a logical predicate, not a claim about a particular database query plan or index.

### `SelectorPredicate::all`

Selects every modeled instance of the object satisfying no narrower condition.

This is a broad selector and may imply many concrete object accesses.

### `eq`

Requires the selected object's field to equal either:

- a `ValueRef`, or
- a literal.

The equality is a logical predicate over modeled values.

Because the selector explicitly exposes its literals and `ValueRef`s, selector provenance should be derived structurally rather than asserted with a separate `deterministic` flag.

#### Surface syntax

A selector value may be written as itself: a map is a value reference, and a plain scalar is a literal, typed as YAML types it.

```yaml
value:
  source: input:input.transfer_stock.request
  path: sku

value: pending
```

Inferring this discriminant is safe where inferring a `ValueSource`'s kind is not. Telling a map from a scalar resolves nothing, whereas the seven value sources are all ids and differ only in which namespace they name. The distinction this section relies on — a selector exposing its literals and references structurally — survives the shorthand rather than being defaulted by it.

A string literal opening with a value source kind is refused in shorthand. `value: input:input.transfer_stock.request` is almost certainly a reference that lost its path, and reading it as the text it spells would quietly turn a provenance-bearing comparison into a comparison with a constant. A string that genuinely spells one is declared in the canonical form.

A string that YAML would read as a bool or an int is quoted, as it is anywhere else.

### `and`

All nested predicates must hold.

The list is conjunctive. It does not define short-circuit evaluation order or physical query evaluation order.

### Selector precision and object identity

A selector constraining every field of a `DataObject.identity` to one logical value identifies at most one logical object instance.

A selector constraining only part of a composite identity may match multiple logical instances.

A verifier must not treat partial identity coverage as single-object selection.

---

## 20. Read, write, insert, and delete steps

### `Read`

Reads the selected object instances.

`fields` describes the read set visible to conflict analysis.

The step binds the transaction-local observation under `bind` (§18) so later steps in the same transaction can use it as deterministic provenance.

#### `FieldSelection::all`

Reads all fields represented by the declared object schema.

If that schema is partial, the verifier must not silently treat this as proof that undeclared real-world fields do not exist; it means all fields represented by the model.

#### `only`

Reads only the listed field paths for the modeled semantics.

### `Write`

Mutates the listed fields of the selected object instances.

The step declares the provenance of the values written through `Derivation` (§18).

A deterministic derivation describes value computation, not replayability by itself. Natural replay analysis must additionally establish replay stability of the selected target and all derivation roots (§18).

A write whose derivation is `Unspecified` normally leaves natural replayability `Unknown` when that mutation matters to the proof.

### `Insert`

Creates a new instance of the declared object type.

The step declares inserted-value provenance through `Derivation` but does **not** redeclare object identity.

`DataObject.identity` already defines the strict non-empty logical identity of every object instance. Two distinct successful inserts cannot create two logical instances with the same complete identity; no separate unique-claim primitive exists.

Whether retrying a conflicting insert can participate in a natural replayability proof depends on duplicate-identity insert outcome semantics that are deliberately undefined — open question 4 (§27). Until they are defined, V1 must not infer transaction replayability merely from object identity uniqueness, and a transaction containing an `Insert` is never proven naturally replayable.

### `Delete`

Deletes the instances selected by the object selector.

Deletion replay behavior depends on what the model guarantees when the selected instance is already absent. Unless sufficient semantics establish a reproducible outcome, the verifier must not silently treat deletion as naturally replayable merely because applying deletion twice leaves no object.

---

## 21. Locks

### `Lock`

A lock is an explicit synchronization guarantee inside a transaction.

For verification, a conforming `Lock` step means:

1. the logical lock is acquired at that point in transaction program order;
2. it protects the object instances selected by `target`;
3. it is held until the surrounding transaction terminates.

Without hold-to-transaction-end semantics, the current DSL would not provide enough information for its intended serialization and deadlock reasoning.

### `shared`

Shared locks are mutually compatible with other shared locks on the same logical target, but conflict with exclusive locks.

### `exclusive`

An exclusive lock conflicts with both shared and exclusive locks on the same logical target.

### `LockOrder::unspecified`

No acquisition-order fact is provided for multiple concrete locks arising from the selector.

### `LockOrder::by`

Locks selected as part of this lock step are acquired according to the ordered list of `OrderingTerm`s.

Each term contains a field path and ascending/descending direction.

This is an acquisition-order fact, not an ordering requirement on business events.

A lock-order declaration may be used for deadlock reasoning only when competing transactions can be shown to use compatible order domains.

### Separate lock steps

Program order between separate `Lock` steps is itself relevant to the lock-order graph.

A `by` order within one selector does not automatically reconcile contradictory order between two separately declared lock steps.

The current DSL therefore cannot declare a deadlock-safe acquisition of several specific instances of one object: a selector admits no disjunction, so one lock step cannot name them, and no fact orders separate steps. The locking facts the DSL lacks are open question 8 (§27), and the model-wide deadlock checker that would consume them is question 9; no V1 verifier reasons about locks.

---

## 22. State machines

### `StateMachine`

A state machine defines the legal states and legal transitions of a persistent object field.

### `StateMachineSubject::object`

`object` identifies the persistent object class.

`state` identifies the field in the object's canonical schema that stores the logical machine state.

### `states`

The set of legal logical states.

### `initial`

The initial state for a newly created logical machine instance.

It does not imply that every existing persistent record is currently in that state.

### `Transition.from`

The set of states from which this transition is legal.

### `Transition.to`

The destination state.

### `TransactionStep::transition`

Selects a concrete persistent machine instance and applies the named transition.

The transition's `from` condition and update to `to` are interpreted as one logical state transition within the surrounding transaction.

The state machine declares legality, not concurrency safety. Two individually legal transitions can still race. The verifier must use isolation, locks, serialization, ordering, or other facts to prove that concurrent execution cannot produce an illegal history.

### Transition transaction replay

A transaction containing any `Transition` is **not naturally replayable** under the current replay proof rules.

A successful transition changes the state against which its own precondition was evaluated, so re-executing the transaction cannot generally be assumed to reproduce the same transaction outcome or transaction artifacts. A transition that rejects a second application may provide an at-most-once state-mutation gate, but that is not natural replayability: suppressing the second mutation reproduces neither the outputs nor the intents of the original execution, and the gate must not be promoted into replayability.

This limitation is a **proof fact, not a structural validity rule**. A transition-containing transaction may declare any `IdempotencyGuarantee` permitted for other transactions — `unspecified`, `not_deduplicated`, or `deduplicated_by { key }` — and none of them is a validation concern; `unspecified` and `not_deduplicated` keep their §17 distinction. `DeduplicatedBy { key }` provides the durable recovery route: after the first successful logical commit, a later same-key encounter resolves the prior `Commit(T,K)` rather than reapplying the transition, and restores the exact retained transaction artifacts.

Where an idempotency, result-replay, or recoverability obligation depends on replay or recovery of a transition-containing transaction, the analyzer proves a sufficient route from the actual declarations: it assumes neither natural replayability nor durable keyed recovery, and the recovery route holds only when `deduplicated_by` is declared over a key the §18 rules make stable. Failing to establish a route leaves the relevant requirement unproven; it does not make the transaction invalid, and the diagnostics report the missing proof facts rather than prescribing `deduplicated_by` as the one legal architecture. A transition transaction over which nothing declares a retry obligation needs no key at all. A retry may also legitimately observe the durably transitioned state and take a different admitted path; what that divergence does is judged by decision replay (§16) and idempotency, not by structural validation. No requirement or guarantee is ever synthesized merely because a transaction contains a transition.

### Transition side effects

A transition may declare publication or request side effects associated with taking that transition. This is intentionally different from operation-owned effects: a state machine exists independently of any one operation and may be applied by several, so its side-effect contracts remain shared model-level declarations while operation-owned effects are execution-local and inline (§7).

For replay semantics, these side effects are treated as **implicitly established effect-intent transaction artifacts** when the transition successfully commits. They are not direct external executions inside the application-state transaction.

Therefore transition side effects commit logically with the transition as intents, enter the invocation artifact context, and are subject to the same retention/recovery rules as explicitly established intents.

An implicitly established intent needs an operation-local binding so a later `ExecuteEffectIntent` step can name it. The application site supplies it directly: the `transition` step's `effect_intents` map binds one intent per side effect (below). No separate operation-level intent declaration exists, and no uniqueness or establishability check on such declarations is needed — the application site names the artifact.

A transition side effect must **not** be established explicitly, and must not be executed by a direct `ExecuteEffect` step: its ID belongs to the state machine's declaration, so an inline site claiming it is a `DuplicateId`. Establishment is the transition's, and execution is `ExecuteEffectIntent`'s.

In particular, consider:

```text
Transaction T
    Transition pending -> paid
        establishes effect intent E
COMMIT

ExecuteEffectIntent E
```

If the invocation crashes after `T` commits but before `ExecuteEffectIntent E`, natural replay cannot be relied on to reproduce `E`, because V1 will not replay the transition transaction naturally. `DeduplicatedBy { key }` ensures that retrying `T` resolves the prior commit and restores `E`, allowing the program to continue along the same path. Without it no declared fact makes `E` available to the resumption, and an obligation that needs `E` there stays unproven.

This still does not imply exactly-once external execution. Effect-level idempotency/retry analysis remains necessary.

### Transition effect intents

The state-machine transition owns the effect contract; the applying `transition` transaction step owns the concrete instance provenance and the artifact binding. `StateTransition.effect_intents` maps each side effect declared by the applied transition — the map key is the side-effect ID — to a `TransitionEffectIntent`: the operation-local `bind` under which the intent artifact is established, and the `Derivation` used to construct that side effect's instance when the transition is applied.

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

The mapping must be exact:

```text
transition.side_effects.keys()
==
state_transition.effect_intents.keys()
```

Each entry supplies exactly one concrete derivation and one operation-local intent binding. Missing entries, extra entries, and entries keyed by another transition's effects are all structural validation errors (`TransitionEffectIntentsMismatch`). A transition without side effects declares an empty map. Unknown provenance must be declared explicitly as `unspecified`; the validator does not synthesize missing entries, preserving the distinction between provenance intentionally unspecified and a provenance declaration accidentally missing.

Each derivation is evaluated in the enclosing transaction context at the point of the `transition` step, as are the side-effect contracts' own roots (§13). It may therefore reference valid operation-level values, transaction outputs available at that point (§16), and preceding `transaction_read` bindings, subject to the usual read-before-use and field-selection rules (§18). It is not evaluated in a static state-machine-transition context, because its values belong to a concrete transaction application of the transition.

A successful transition transaction logically performs the following atomically:

1. evaluate the state-transition guard;
2. apply the state transition;
3. construct each transition side-effect instance using its corresponding derivation;
4. establish each bound effect-intent artifact;
5. commit the transition state and established artifacts together.

When the transaction declares `DeduplicatedBy { key }`, these derivations are evaluated only during the first successful keyed execution: a retry with the same transaction idempotency identity resolves `Commit(T,K)` and recovers the exact original artifacts without evaluating the derivations again, which is what lets transition effect values depend on transaction-local reads even though those reads may not be replay-stable. Without keyed deduplication no such recovery fact exists: the intents are established on first success like any other artifact, but nothing makes them replay-available, and obligations that need them settle unproven (§18).

---

## 23. Framework transaction artifacts versus application data

`TransactionOutput` and `EffectIntent` are the two principal framework-level **logical transaction artifacts**. Neither is an inherently durable primitive.

They have deliberately different jobs:

- a **`TransactionOutput`** is a typed logical value deliberately exported from a transaction into subsequent operation control. It represents **data**.
- an **`EffectIntent`** is a captured logical effect instance intended for later execution. It represents **pending logical work**.

The rule follows: do not use an `EffectIntent` merely to transport arbitrary transaction data, and do not use a `TransactionOutput` to represent work awaiting execution. Both may be established by the same transaction, both enter the path's artifact context under the same definite-availability rule (§16), and both may be retained by `Commit(T,K)`; they are not interchangeable.

They may participate atomically in a transaction without belonging to the application `DataModel` namespace.

A transaction containing only framework artifact-establishment operations may therefore have `data_model: null`.

Once a transaction reads, writes, locks, inserts, deletes, or transitions an application `DataObject`, its application transactional boundary must be declared.

Artifact durability depends on the replay mechanism:

- a naturally replayable transaction may reconstruct replay-deterministic artifacts;
- a transaction `DeduplicatedBy { key }` must durably retain the exact artifacts of its successful `Commit(T,K)` because its body is not committed again on replay.

This framework retention must not be interpreted as a hidden global transaction spanning arbitrary application data models.

---

## 24. Crucial distinctions for the proof solver

The solver must preserve these distinctions:

| Concepts | Why they are not interchangeable |
|---|---|
| **Requirement vs guarantee** | A declared requirement still needs proof. |
| **Validation vs verification** | A coherent model can still describe an unsafe architecture. |
| **Unspecified vs negative guarantee** | Unknown is not the same as explicitly unordered/unbounded/non-deduplicated. |
| **Semantic layer vs semantic category** | L0 versus L1 says which layer a fact belongs to; structural/guarantee/requirement says what kind of claim it makes. Correctness relevance decides neither. |
| **Transport ordering vs execution ordering** | Ordered delivery can still lead to concurrent/overtaking execution. |
| **Ordering vs serialization** | Serialization prevents overlap; ordering preserves the correct precedence. |
| **Transport order vs semantic order** | A broker can serialize concurrent producers without establishing a business-level happens-before relation. |
| **Routing domain vs pool member** | A routing key names a semantic domain; `MemberAssignment` maps it onto a member. The domain keeps its identity across rebalances. |
| **Routing domain vs storage partition** | Equal key expressions do not make execution affinity and physical partitioning the same concept. |
| **Shared pool vs shared routing domain** | One execution population is not one ownership domain; two boundaries in one pool borrow no affinity from each other. |
| **Routing absence vs unconstrained routing** | No routing block is no fact — not a declaration that routing is arbitrary. |
| **Grouping vs ordering** | A transport may group without ordering, order without grouping, or both. Serialization needs only the first. |
| **Grouping vs dispatch** | Grouping is a transport equivalence domain; dispatch maps it onto execution topology. Dispatch preserves precedence and never creates it. |
| **Topic scope vs subscription scope** | Exclusive declaration modes, not a default and an override. Neither inherits from the other. |
| **Serializability vs linearizability** | Serializable histories need not respect real-time precedence. The DSL currently declares no object-history requirement (§5); the distinction is kept so that `serializable` is never promoted into one. |
| **Atomic transaction vs external side effect** | Local atomic commit does not imply an external publication/request is atomic with it. |
| **Idempotency lineage vs deduplication** | Propagating a key lets the analyzer trace identity; only a guarantee/mechanism actually deduplicates. |
| **Deterministic derivation vs replay stability** | The same sources produce the same value, but those source values may differ on retry. |
| **Natural replayability vs at-most-once commit** | Preventing a second commit does not guarantee that a retry can reconstruct the original outcome or artifacts. |
| **Natural replay vs keyed recovery** | Natural replay recomputes the same logical outcome; keyed transaction idempotency resolves a prior durable commit without committing the body again. |
| **Artifact availability vs intrinsic durability** | An artifact may be reconstructed naturally or recovered from a keyed commit; its declaration alone does not imply durable storage. |
| **Transaction read determinism vs read invariance** | A deterministic computation from a read can still change on retry because the observed state may have changed, including due to the transaction itself. |
| **Object identity vs ordering key** | They may coincide, but neither declaration automatically implies the other. |
| **State-machine legality vs replayability** | A legal transition graph does not imply that a transition-containing transaction can be naturally replayed after commit. |
| **Effect-intent recovery vs exactly once** | Recovering the same intent does not establish whether the external effect already occurred or whether another attempt is safe. |
| **Idempotency vs recoverability** | Idempotency bounds what retries may do and is satisfied by never retrying; recoverability obliges the program to actually reach a terminal. |
| **Resumable vs guaranteed completion** | Being able to resume is a property of the path's artifacts; being re-driven requires a modeled retry driver. |
| **Unstable decision: safety vs progress** | A decision not established to replay is an obstacle for idempotency and result replay (a retry may do different work or return a different result) and never for recoverability (whichever admitted path the retry takes is analyzed on its own). |
| **`Err` vs interrupted execution** | `Err` is a conclusive logical outcome a synchronous interaction returned; a crash, timeout, or lost connection is an idempotency/recoverability question and is not an `Err` payload. |
| **`TransactionOutput` vs `EffectIntent`** | An output exports data; an intent captures pending work. Both are artifacts of the same commit, and neither may stand in for the other. |
| **`match_result` vs `branch`** | A control-flow decision on a `Result` destructures a mutually exclusive typed outcome; a `branch` evaluates an ordinary predicate. Success/failure is never encoded as a status-field comparison. |
| **Effect payload replay vs effect result replay** | A class-fixed outgoing instance proves every attempt asks the same question; whether the same answer comes back is a separate fact — a target's proven result consistency, or a deduplicated external boundary's fixed terminal result (never its retryable or unspecified errors). |
| **Transaction output vs request result** | An output is a schema-shaped artifact a transaction exports; a request result is a `Result<Ok, Err>` constructed at a `return` terminal. A result may be derived from an output, but no privileged artifact stands between them. |
| **Duplicate-delivery fact vs liveness** | `at_least_once` and `may_repeat` say a retry may happen, not that retries continue until success. |
| **Ordering key vs message identity** | The ordering key sequences messages; the message identity identifies one logical message. They may coincide; neither implies the other. |
| **Object identity vs message identity** | `order_id` identifies the order, not the message about the order. |
| **Key equality vs payload equality** | Class membership equates the governing key's components only; payload equality needs a declared stimulus identity pinned by that key. |
| **Stimulus identity vs deduplication** | An identity fixes what the payload of a logical request or message is; only a mechanism limits how often work happens. |
| **`execute_effect` vs `execute_effect_async`** | Synchronous completion dependency versus asynchronous initiation: only the first establishes `complete(A) < start(next)`. |
| **Effect ID vs async handle** | Stable effect-site identity versus invocation-local synchronization artifact. |
| **Launch vs completion** | Starting an effect does not imply it has completed; only synchronization establishes completion edges. |
| **`join_all` vs serialization** | A barrier after all completions establishes no order and no non-overlap among the joined candidates. |
| **`race` vs cancellation** | First-completion synchronization says nothing about stopping, preventing, or undoing the losers. |
| **`race` vs first-success** | The first completion may be an `Err`; a result-binding race observes it. |
| **Async result vs handle** | The application result is unavailable until synchronization; the handle is never application data. |
| **L0 async vs L1 member concurrency** | Intra-invocation logical effect overlap versus runtime invocation capacity; neither implies the other. |
| **Operation terminal vs async completion** | An operation may finish while launched effects remain unresolved; the terminal neither joins nor cancels them. |
| **Async launch vs durability** | Initiation provides no rediscovery guarantee; durable rediscovery needs transactions or effect intents. |
| **Launch order vs effect order** | Textual async starts establish no completion or externally observable effect ordering. |

---

## 25. What a successful Conseqa proof means

A successful proof should be read as:

> Given the declared architecture facts, given the semantic contract in this document, and assuming the concrete implementation conforms to the declarations used by the proof, the specified requirement follows for all executions admitted by the model.

It should **not** be read as:

> The implementation is universally correct.

Conseqa proves selected application-level properties over a declared abstraction. Its strength comes from making the abstraction explicit and forcing correctness arguments to state which facts they depend on.

### 25.1 Proof scope

Every successful proof records the semantic layers its argument consumed:

| Scope | Meaning |
|---|---|
| `l0_only` | No explicit L1 fact was required. |
| `runtime_dependent` | At least one L1 fact was necessary. |

The scope records a **dependency**, nothing more. A `runtime_dependent` proof is not weaker in kind; it is conditional on the runtime realization the model declares, and must be re-examined when that realization changes — which is exactly what makes the record useful. Changing a pool's member concurrency, or dropping a router's routing block, invalidates the proofs that cited them, and the scope is how a reader finds them.

`l0_only` does **not** mean implementation-free. A proof resting on `isolation: serializable` is L0-only, and still assumes the concrete database implements serializable execution. Scope identifies dependency on semantic layers, not the absence of conformance assumptions.

Alongside the scope, a proof carries the declarations it consumed, so a report can say precisely why a verdict holds:

```
Requirement:  SerializedBy(account_id)
Verdict:      Proven
Scope:        runtime_dependent
Evidence:
    Router update_account_router
        semantic routing key = account_id
        member_assignment    = consistent_hash
    ExecutionPool account_workers
        member_concurrency   = bounded(1)
```

### 25.2 Removing L1

Deleting the runtime model from a valid model leaves it valid. Requirements discharged from runtime facts become **unproven** — never violated, and never a structural error. That asymmetry is the point of the layering: L0 stands alone, and L1 is what a particular realization adds.

---

## 26. Authoring rule of thumb

When declaring a fact, ask:

> Would I be willing for the verifier to rely on this statement in a correctness proof?

If not, use `unspecified` or omit the stronger claim.

When declaring a requirement, ask:

> What observable property would make the architecture wrong if it failed?

Keep that requirement separate from the mechanism expected to satisfy it. The solver's job is to connect the two.

When deciding which layer a declaration belongs to, ask:

> Is this a property of the application machine, or of one particular way of running it?

Serializable isolation, explicit locks, message identity, and `retry: may_repeat` describe the machine, however much infrastructure implements them — they are L0. Transport ordering, delivery, routing, execution pools, and storage partitioning describe one realization — they are L1. Do not move a declaration to L1 because it feels operational, or keep it in L0 because a proof depends on it. Correctness relevance decides neither.

When declaring transport semantics, ask which of the two facts you actually have. Grouping and ordering are separate on purpose: a transport that groups by a key without ordering within it is an ordinary thing, and saying so earns a serialization proof without claiming an order that does not exist. Declaring `within_group` to reach a grouping key would be exactly the false statement §26 warns against.

When declaring runtime topology, declare only what the architecture genuinely provides. Inventing a pool or a member assignment to make a proof pass is the same error as declaring a guarantee the implementation does not offer — and here the temptation is sharper, because `member_concurrency = bounded(1)` discharges obligations so readily. If the architecture does not constrain execution that way, leave the requirement unproven.

---

## 27. Open questions and deferred semantics

What the DSL deliberately does not yet decide. Every entry is scoped so that resolving it can only extend what can be stated and proven — never invalidate a V1 verdict. The questions keep the numbering under which diagnostics, code comments, and fixture notes cite them; resolved questions are recorded in a line so the numbering stays meaningful.

1. **Artifact-level keys** — *Resolved.* Transaction artifacts carry no independent logical identity or key: a `TransactionOutput` or `EffectIntent` is identified by its declaration, and its retry availability comes only from the §17 routes of its establishing transaction.

2. **Artifact derivation granularity** — *Resolved.* The `Derivation` at an establishment or execution site describes the complete constructed instance (§13, §14); no finer per-field lineage surface exists.

3. **Replay-stable provenance roots** — *Resolved.* The rules are §18 in their entirety: stability is definitional (governing-key components), declared (a request or message identity pinned by the governing key), or derived (keyed-commit recovery, natural-replay reconstruction, effect-result stability, congruence); everything else is `Unknown`.

4. **Insert failure semantics** — *Open.* The outcome of attempting to insert an already-existing `DataObject.identity` — and how it affects the enclosing transaction and path admissibility — is normatively undefined. Until it is defined, a transaction containing an `Insert` is never proven naturally replayable (§20), which is `Unknown`, not a violation.

5. **Transition natural replay** — *Open.* V1 never replays a transition-containing transaction naturally, whatever its declared guarantee (§22); the durable recovery route through `deduplicated_by` is the only proven route. A later solver may investigate whether restricted transition patterns admit a sound natural-replay argument — for instance, a transition whose re-encounter against the durably transitioned state is provably outcome-equivalent — but no such inference is permitted in V1.

6. **Effect execution completion state** — *Open.* Intent reconstruction and recovery are kept separate from durable tracking of whether the underlying effect has executed or completed. V1 records the §14 uncertainty — a recovered intent may re-execute, and an effect may have succeeded before a crash without that success being durably known — and models no execution-state artifact. What minimum completion-state vocabulary a later revision needs, and whether it is a transaction artifact, a boundary guarantee, or both, is undecided.

7. **Alternative-path continuation** — *Open; V1 stance adopted.* Recoverability is proven by same-path continuation (§9): for every admitted path, re-driving that path from its first step reaches its terminal, and a decision is never an obstacle to progress. This is a sufficient route that neither uses nor forbids continuation along a *different* admitted path after a partial execution; which other paths a resumed attempt may legitimately follow, and what preconditions make one applicable, remains unresolved. A resolution may add routes; it cannot invalidate same-path proofs.

8. **Locking expressivity** — *Open; earmarked for implementation.* A `Lock` is one selector, a mode, and an acquisition order within that selector (§21). That surface cannot state the facts a deadlock argument needs, and the gaps are these:

   - *Instance sets.* `SelectorPredicate` admits only `all`, `eq`, and `and` — no disjunction, no set membership — so one lock step cannot name several specific instances of an object. `tx.transfer_stock` in `tests/fixtures/flash_checkout.yaml` locks its source and destination `stock` rows as two steps for exactly this reason. Needs `or`/`in` over a field (with §19's provenance rules extended to them), or a lock step with several targets.
   - *Cross-step and cross-object order.* `LockOrder::by` orders acquisition only inside one step; between steps the only fact is program order, which is data-relative for a transfer (source before destination), and nothing orders locks on different objects ("`order` before `stock`"). Needs a transaction-level lock order — a sequence of objects, each with a field order — or a data-model-level ordering convention that transactions cite.
   - *Order-domain compatibility.* §21 permits deadlock reasoning only between "compatible order domains" without defining them. Needs a definition: same object, one declared order a common prefix of the other, same directions; or one declared total order per object.
   - *Preconditions and distinctness.* Selectors reference input values, so whether two of them address one instance or two depends on the inputs, and nothing can state `source_warehouse_id ≠ destination_warehouse_id`. Needs a decision on input preconditions; without them the degenerate same-instance case of a transfer is unstatable and its two writes conflict.
   - *Upgrades.* A `shared` then `exclusive` lock on one target within a transaction is the classic deadlock (two holders both upgrading); the DSL allows writing it and gives it no semantics. Needs one: conversion or a second lock.
   - *Implicit locks.* Engines take row locks on `Write`/`Delete` and gap locks on `Insert` under the stronger isolation levels; the DSL models only explicit `Lock` steps. Needs either implicit-lock facts per isolation level (a `Write` acquires `exclusive` on its selector at its program point) or a stated assumption that only declared locks count — which makes every proof conditional on the engine's conformance to that assumption.
   - *Predicate versus instance locks.* Whether a lock on `all` or on a partial identity covers instances inserted later (a predicate lock) or only current ones is unspecified; serialization and deadlock reasoning both depend on it.
   - *Wait policy.* No lock-wait timeout, `nowait`, or `skip locked` fact; these decide whether a circular wait deadlocks or aborts. Absent one, a checker must treat every cycle as a deadlock.

   No V1 proof credits a lock — the serialization checker deliberately declines the lock route — so each of these can only add what can be stated and proven, never invalidate a verdict.

9. **Model-wide deadlock checker** — *Open; earmarked for implementation; depends on 8 to be useful.* A model-wide analysis, not a per-operation requirement: locks live in transactions, and a deadlock is a property of every transaction the model admits concurrently on one data model. The analysis, per data model:

   1. *Collect* every explicit lock step of every transaction in every operation — object, selector, mode, declared order, program position — and, once question 8 settles it, the implicit locks its isolation level implies.
   2. *Abstract* each lock to a class: the object and the shape of its selector (full identity, partial identity, `all`), with selector values carried as canonical paths, so that two classes are *disjoint* only when provably so (distinct literals, or identities pinned to different canonical values) and otherwise *may overlap*. Two classes *conflict* when they may overlap and are not both `shared`.
   3. *Order* the classes: within a transaction, program order between steps and the `by` order within a step give a per-transaction acquisition order over conflicting classes; a multi-instance step with `order: unspecified` contributes no order among its own instances.
   4. *Admit* concurrency: two transactions can overlap unless a declared fact says otherwise — a proven serialization requirement for same-key invocations, or `member_concurrency = bounded(1)` on a pool both boundaries are assigned to. The serialization verdicts already compute most of this.
   5. *Decide.* The union of the admitted transactions' acquisition orders over conflicting classes is acyclic: **proven**, citing the global order it found. A cycle whose every edge is a declared fact, whose transactions are admitted concurrently, and whose classes may overlap: **disproven**, with a counterexample trace — "T1 holds A, requests B; T2 holds B, requests A" — the checker's first disproven verdict, consistent with §1.2 because it is built from declarations, not from their absence. Anything else — an `unspecified` order on a multi-instance class, an unspecified concurrency bound, overlap that cannot be decided — is **unknown**, with the lock steps it hinges on as evidence.

   To settle alongside: whether deadlock freedom is declared (a data-model requirement, keeping the rule that requirements are obligations) or standing; a `data_model` subject kind for the report; the rendering of a disproven obligation with its trace; and the wait-policy assumption of question 8. Until question 8 lands, the analysis is implementable but would return unknown for nearly every real model, the transfer pattern included — which is still the honest answer.

10. **Input-specific path admission** — *Open; V1 stance adopted.* An operation may declare several inputs, and its one program does not say which input an invocation entered through. V1 relates a path to an input only through its terminal (§16): a path is admitted for triggering input `i` iff it ends at `complete` or at `return` for `i`. This is sufficient but weaker than the model knows: a `complete`-terminated path is admitted for every input, including a request input whose invocations then return nothing; a program with two request inputs cannot state that a step is reachable only through one of them, so both populations are analyzed over it; and a `return` for another input excludes a path without saying what an `i`-invocation does instead. An explicit entry concept — a per-input entry block, an `entry` step naming the inputs that may reach the steps it dominates, or a validation rule that every request input has at least one `return` — would let validation reject a program that returns nothing for a request input, let each population analyze only the steps it can reach, and give path admission a declared rather than inferred basis. Any resolution refines admission and so can only remove paths from an analysis, never add work to a proven one.

11. **External effect result replay** — *Resolved.* For a result-bearing external effect, `deduplicated_by` fixes the interaction's terminal result (§13.3), and `ResultType.err` declares an `ErrorDisposition` (§8.1); a terminal external result over a class-fixed key is a replay-stable root (§18 rule 6). Still open from that resolution: heterogeneous per-error-class dispositions inside one result contract, and any retry-execution vocabulary that would consume `retryable`.

### Deferred surfaces

- **Loops.** The program is acyclic (§7). Iteration requires semantics for iteration identity, repeated transaction artifacts, repeated effect-result bindings, effect multiplicity, termination, ordering between iterations, recovery from partial iteration progress, and bounded analyzer state; none are defined. The expressivity the program model was built to solve is branching and intermediate observations, not iteration.
- **Object-history requirements.** No object-level history requirement (such as linearizability) exists; §5 states the scope rule and what its absence does not weaken. To be reconsidered as a coherent family when Conseqa models distributed persistence and availability.
- **Process completion.** Recoverability's `guaranteed` obliges one operation to reach its terminal. That a multi-operation process — a saga across the trigger graph — reaches its end state is a distinct liveness property with no declaration; it needs new surface (the trigger graph is its natural consumer), not a stronger reading of `guaranteed`.
- **Retry execution.** `ErrorDisposition::retryable` states that another attempt is semantically admitted (§8.1); nothing models the mechanism that performs one — no retry policy, loop, attempt count, backoff, or timeout. A retry-execution revision may consume the disposition.
- **Performance overlay.** The correctness vocabulary deliberately exposes distinctions a future probabilistic layer could consume — terminal versus retryable outcomes, attempt populations, member concurrency, member assignment, routing and partition keys — but no performance semantics exist in the model. L1 is qualitative by design: pool cardinality, traffic rates, key-frequency distributions, service-time distributions, storage-node counts, replication factors, capacity, queueing, and latency belong to an external simulation scenario evaluated *against* a Conseqa architecture, never inside it.
- **Explicit negative routing** — *partly resolved.* There is still no `unconstrained` routing variant: absence of a routing block expresses that no member-affinity fact exists, and routing keeps exactly two components. The distinction between *unknown* and *known arbitrary* member behaviour is now carried where it belongs, on the assignment: `member_assignment: round_robin` (§10.6), admitted for the external analysis that needs it rather than for any proof. Still open is whether a routing *key* ever needs a comparable negative.
- **Global execution gates.** Removing operation-level concurrency leaves no way to say "no two invocations of X overlap globally", independent of topology. If a genuine architectural need appears, it should be an explicit primitive — never hidden inside `Operation`, where it was detached from the execution topology that realizes it.
- **Beyond the pool.** L1's execution abstraction stops at a population of interchangeable members. Physical database nodes, replica topology, consensus protocols, database lock-manager internals, hosts, containers, process ids, CPU, memory, availability zones, network links, queue capacities, and autoscaling policies are all outside it.
