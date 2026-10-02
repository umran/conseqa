# Conseqa's Transaction Serialization Graph

Originally a read-only investigation of the DSL 4 checker (`master`, top
commit `8637bd0`). Revised for DSL 6 — the Atomic Mutation and
Serialization Primitives revision
(`Conseqa_Atomic_Mutation_and_Serialization_Primitives_Revision_Specification_DSL_v6.md`),
which replaced the `validate_version` / `bump_version` protocol with atomic
conditional mutations — against the `atomic-mutation-dsl-6` working tree.
Line numbers are a snapshot of that tree; the code moves, so search by the
symbol named beside each one. The normative statement of every rule below
is `CONSEQA_DSL_SEMANTICS.md` §17 and §20.

## 0. The one-sentence version

Conseqa's "serialization graph" is **not** a runtime structure built while
transactions execute (there is no live database inside Conseqa). It is a
**static, whole-model analysis** performed by the analyzer over the *declared*
DSL model: for every inline transaction template, it derives every
persistent-state access, then builds (a) an undirected "conflict closure" of
templates that could ever touch overlapping state, and (b) a directed graph of
potential write-read / read-write / write-write dependencies between the
accesses of those templates. It then asks whether every dependency that lies
on a cycle of that graph is **commit-order constrained** by some declared
fact (isolation, strict locking, an atomic conditional mutation, a read-only
observation at one instant, or an ordered cursor). If so,
serializability is *proven*; if a cycle survives with an unconstrained edge,
the requirement is *unproven* and the checker reports exactly which edge and
why. This is a compile-time / model-checking technique, closest in spirit to
static conflict-serializability analysis (à la Bernstein/Goodman SSI-style
serialization graphs) rather than a runtime SSI engine like Postgres's.

"Concurrently running" is determined **conservatively and statically**: two
transaction templates are treated as potentially concurrent (and thus
relevant to each other) whenever the analyzer cannot *prove* their accessed
domains and fields are disjoint. There is no clock, lock manager, or
scheduler consulted — unknown overlap is always assumed to be an actual
conflict. Even two *concurrent executions of the same template* are modeled
as two distinct transactions that can conflict with themselves (self-loops in
the graph).

## 1. What the graph represents, and where it lives

Core module: `src/analyzer/verification/transaction_conflicts.rs` (2258
lines). Its module doc (lines 1–40) frames it as "the shared machinery of
the transaction serializability and ordering provers (§35–§52 of the DSL v4
revision)"; the DSL 6 revision (§15–§27 of its specification) changed the
evidence the machinery derives, not its shape.

Two provers consume this shared index:
- `src/analyzer/verification/transaction_serializability.rs` (705 lines) —
  discharges `SerializableBy(K)` requirements.
- `src/analyzer/verification/transaction_ordering.rs` (534 lines) —
  discharges `OrderedBy(K, P)` requirements, and internally *re-invokes* the
  serializability prover for its own closure (`transaction_ordering.rs:357`).

A visualization/UI-facing extraction layer sits on top:
- `src/viz/transaction_proofs.rs` — turns the same `ConflictIndex` into a
  `TransactionProofs` view-model (nodes, edges, pairs, cycles) "so the front
  end draws the argument rather than paraphrasing it". Wired into
  `src/viz/render.rs:14,30,44,57`.

### Nodes: `TransactionTemplate` / `TransactionRef`

`TransactionRef` (`transaction_conflicts.rs:62`) is `{operation,
transaction, location}`: one **inline transaction declaration** in the
program is one template. Concurrent executions of the same template are
analyzed as two distinct nodes (module doc, lines 37–40), which is why a
template can conflict with itself.

`TransactionTemplate<'a>` (`:357`) bundles the template's:
- `accesses: Vec<TransactionAccess>` — every persistent-state touch
  (`TransactionAccess`, `:205`)
- `locks: Vec<LockAccess>` — `Lock` steps, indexed apart from accesses
  because "a lock observes and mutates nothing" (`:230`)
- `conditional_mutations: Vec<ConditionalMutation>` — every atomic
  conditional mutation, normalized (`ConditionalMutation`, `:315`): a
  `compare_and_set`, `transition`, `advance_cursor`, `fence`, or an
  `upsert`'s identity arbitration, with its target, mechanism
  (`ConditionalMutationKind`, `:250`), comparisons (`ComparisonFact`,
  `:296` — each marked `observed` when its `expected` is exactly the same
  field of an earlier read of the same instance, resolved by `observation`
  at `:1714`), written fields, the version field it publishes, and whether
  its target pins the object's whole identity (`identified`)
- `artifacts: Vec<CommitArtifact>` — outbox admissions, effect-intent
  establishments, transaction outputs (`:334`); these are *never* treated
  as conflict accesses

There are no `validations` or `bumps` any more: DSL 6 has no version steps,
and nothing pairs a reader's check with a writer's annotation.

The whole-model index is `ConflictIndex<'a>` (`:368`), built once per model
by `ConflictIndex::build` (`:380`), which walks every operation's program
(`model.operations`), finds every inline `transaction` step via
`operation.program.transactions()`, and turns each into a
`TransactionTemplate` via `template_of` (`:473`).

### Access modes and footprints (`AccessMode`, `AccessFields`)

`AccessMode` (`:91`) has twelve modes, each classified by `.reads()` /
`.writes()`: `Read`, `Update`, `CompareRead` (the fields a guarded
mutation's comparisons read), `CompareWrite` (the fields a compare-and-set
mutates), `Insert`, `UpsertReadWrite` (an upsert's arbitration and either
branch — the target selector, every field), `Delete`, `TransitionRead`,
`TransitionWrite`, `CursorReadWrite`, `FenceReadWrite`, and
`VersionPublish`. `template_of` is a match over every `TransactionStep`
variant that derives the access(es) it implies — e.g. a `Transition` step
(`:677`) produces a `TransitionRead`, a `CompareRead` for any comparisons
it carries, a `TransitionWrite` of the state-machine's governed state field,
a `VersionPublish` when the object is versioned, and one
`ConditionalMutation`; it can also emit `CommitArtifact::OutboxWrite` for
transition-scoped effects (`:716`).

`VersionPublish` is synthetic. No step names the version field, but every
committed mutation of a live versioned instance publishes a newer token, so
`template_of`'s `publication` closure (`:512`) adds a write of the version
field to every `update`, `compare_and_set`, `transition`, `advance_cursor`,
and `fence` of a versioned object. Inserts, deletes, and upserts already
touch every field and need none.

`AccessFields` (`:173`) is `All | Only(BTreeSet<FieldPath>) | Unknown`,
where an undeclared field footprint (an `update` or `compare_and_set` naming
no fields) is `Unknown` and is "treated as potentially conflicting with
everything".

### Edges: what a conflict/dependency edge means

There are two distinct graphs built here, and it's important not to conflate
them:

1. **The undirected "conflict closure" graph** (§41 of the DSL v4
   revision; `closure()` at `transaction_conflicts.rs:915`,
   `templates_conflict` at `:903`). An edge exists between two templates
   when *any* access pair between them is a `conflict()` (`:882`): at least
   one access writes, and neither the selector domains nor the field
   footprints are provably disjoint. This graph is used only to compute the
   **closure** — the connected component containing the root transaction —
   via a frontier walk. This is deliberately transitive: "a serialization
   cycle can pass through a transaction that never touches the root's state
   directly" (module doc, lines 21–25).

2. **The directed "potential serialization-dependency graph"** (§43,
   `dependencies()` at `:937`), built *only* over the members of a closure
   once it's known. For every ordered pair of accesses `(a, b)` across all
   closure members (including a template against itself) that conflict, it
   emits:
   - `WriteRead` if `a` writes and `b` reads — "the target reads what the
     source wrote"
   - `ReadWriteAntiDependency` if `a` reads and `b` writes — the write-skew
     anti-dependency
   - `WriteWrite` if both write

   Each edge (`DependencyEvidence`, `:2208`) records `kind`, `source`,
   `source_step`, `source_mode`, `target`, `target_step`, `target_mode`,
   `object`, `selector_overlap`, `field_overlap`, `evidence`
   (`CommitOrderEvidence`, `:2038`), an optional `fence`, and any `gaps`
   (`DependencyGap`, `:2133`).

## 2. Detecting "concurrently running" transactions

There is **no dynamic/runtime concurrency detection** in Conseqa — no
scheduler, timestamps, or lock manager observed at execution time. Concurrency
is decided purely from the declared model, conservatively:

- **Which templates could ever be concurrent with each other**: implicitly,
  *any two inline transaction templates in the whole model* are candidates —
  `ConflictIndex::build` indexes every transaction of every operation, and
  `closure()` walks outward from the root through every template whose
  accesses could conflict, with no notion of "these two operations can
  never run at the same wall-clock time." The verification module doc is
  explicit about this scope: the transaction families "never rest on L1"
  (runtime/placement facts) "because no placement, transport, or capacity
  fact survives redelivery, worker replacement, or reordering after failure"
  (`src/analyzer/verification/mod.rs:48-53`). Concretely: `apply_payment`
  and `cancel_order` are two *different* operations, driven by different
  message types, dispatched to different logical work — yet because both
  touch `object.order` they are unconditionally treated as potentially
  concurrent and put in one closure (worked example below).
  See also `tests/transaction_requirements.rs`
  (`no_transaction_verdict_depends_on_the_runtime_topology`,
  `a_serial_pool_and_affinity_prove_nothing_about_a_transaction`), which
  assert this directly: changing L1 (worker pool serialization, routing
  affinity) never changes a transaction verdict.

- **Which specific accesses "conflict" and thus create an edge**: this is
  where the actual concurrency-relevance test lives, `conflict()`
  (`transaction_conflicts.rs:882-899`):
  1. At least one of the two accesses must be a write (`.mode.writes()`) —
     two reads never conflict.
  2. `selector_overlap()` (`:829-877`) must not prove `Disjoint`. This
     checks, in order: different object ids → disjoint; pinned-literal
     predicates naming incompatible values on the same field → disjoint;
     both predicates `All` → overlapping; complete declared identity fields
     pinned by *equal* literals on both sides → overlapping
     (single-instance match); otherwise → `Unknown`, which the conflict
     test treats as *not* disjoint, i.e. as a conflict ("Unknown overlap is
     never treated as disjoint"). Notably, "two executions evaluate their
     references independently, so equal references prove nothing across
     them" — a shared symbolic input reference (e.g. both reading
     `input:order_id`) does *not* prove the two executions target the same
     row; only literal/identity matches do.
  3. `field_overlap()` (`:1859`) must not prove `Disjoint`: `Unknown`
     footprint on either side → `Overlapping` (unknown treated as
     conflicting); `All` on either side → `Overlapping`; otherwise
     field-path prefix matching between the two `Only` sets.

  So "concurrency relevance" reduces to: *could these two accesses, run by
  any two executions anywhere in the modeled system, touch the same
  persistent state in a way that isn't provably impossible?* If yes, an edge
  exists and the pair is folded into the closure regardless of how far apart
  the two operations are in the program.

This conservative design is stated as a first-class principle: "Everything
here is conservative (§70): an unknown selector overlap is potentially
overlapping, an unknown field footprint potentially conflicting, an
unspecified isolation no isolation, and a guard that does not identify one
instance, or does not compare what the conflict touches, no credit at all"
(module doc, `transaction_conflicts.rs:33-37`).

## 3. How edges get added — the exact trigger conditions

Once the conflict closure is known, `dependencies()`
(`transaction_conflicts.rs:937`) creates one `DependencyEvidence` per
conflicting `(source access, target access)` ordered pair, for **every**
kind that applies (an access that both reads and writes — a cursor advance,
a fence, an upsert — can produce several edges against one other access).
Then `dependency()` (`:988-1170`) classifies *why* the commit order is (or
isn't) fixed — the "commit-order evidence" computation:

- **Ordered-cursor short-circuit** (`:1010-1020`): if both accesses are
  `CursorReadWrite` on the same field under the *same* declared rule
  (`Successor` or `MonotonicAfter`), the edge is `OrderedCursor` evidence
  regardless of kind — "an older accepted position cannot commit after a
  newer one".
- **`WriteRead`** (`:1023-1043`): commit-ordered (`IntrinsicCommittedRead`)
  if the target's isolation is not `Unspecified` — the target reads only
  committed writes. Otherwise, if the target's read is covered by its own
  guarded mutation of the instance (`protection`, `:1176` — the read is part
  of that mutation's statement, or follows it on the same identified
  instance) and the source holds its write until it terminates
  (`holds_write`, `:1248` — declared isolation, or its own guarded
  mutation), the edge is `AtomicConditionalMutation` evidence. Otherwise
  `None`, with an `IsolationUnspecified` gap.
- **`WriteWrite`** (`:1045-1082`): commit-ordered (`AtomicWriteOrder`) if
  *both* sides declare isolation ≠ `Unspecified`. Otherwise, if the
  source's write is covered by its guarded mutation, or the target's is and
  the source declares isolation, `AtomicConditionalMutation`. Otherwise
  `None`, with `IsolationUnspecified` gaps for whichever side lacks
  isolation.
- **`ReadWriteAntiDependency`** (`:1084-1138`, the write-skew edge), reader
  access `a`, writer access `b`, tried in order:
  1. `covering_lock()` (`:1403`) on both sides — a *strict lock* protects
     the edge only if the reader holds a shared-or-exclusive lock and the
     writer holds an exclusive lock, **both acquired at an earlier step than
     the access they protect** (a lock acquired after the access "protects
     no earlier observation"): `StrictLock`.
  2. `observation_guard()` (`:1269`): `a` is covered by the reader's own
     guarded mutation (`GuardCoverage::Atomic` / `HeldProtection`), or a
     *later* guarded mutation of the same identified instance in the
     reader's transaction compares what was read — the object's version
     against the version a read at or before `a` observed
     (`ObservedVersion`), or fields against the values `a` itself observed,
     covering every conflicting region of the two footprints
     (`ObservedState`; the coverage check is `uncovered`, `:1778`). The
     version route is refused when `b` is an insert or upsert and some
     template deletes the object (`deleter`, `:454`): a re-inserted
     instance may repeat an observed token.
  3. `serializes_at_its_read()` (`:1218`): the reader writes nothing,
     declares isolation, and observes at one instant — every read one read
     step of one identified instance under `read_committed` or
     `serializable`, or any reads under `snapshot` — so its anti-dependencies
     are ordered by its own serialization point: `ReadOnlyObservation`.
     Several reads under `serializable` do not qualify: the engine may hold
     read locks rather than read one snapshot, and a writer of unspecified
     isolation need not respect them.
  4. *Locked reader*: the reader holds a covering lock from before `a`, its
     selector identifies one instance, and `b` is covered by the writer's
     guarded mutation, which must acquire the write protection the reader's
     lock withholds: `AtomicConditionalMutation` with
     `GuardCoverage::LockedReader`.
  5. Otherwise `None`, with the lock gaps (`LockCoverageMissing` /
     `LockAcquiredAfterProtectedAccess`, per side) and one guard gap:
     `ObservedStateGuardMissing`, `ObservedStateGuardDoesNotCoverConflict
     { fields }`, `ObservedStateNotIdentified` (the read selects a set or a
     range — only a lock or a serializable closure can cover it), or
     `ObservedVersionMayRepeat { deleted_by }`.
- **Fences are never evidence** — a `fence` pair is recorded separately as
  `dependency.fence` (`:1002-1008`) because "equal fencing tokens serialize
  nothing... a fence constrains no dependency by itself". A fence is a
  `ConditionalMutation`, but `ConditionalMutationKind::holds_protection`
  (`:267`) is false for it: on an equal token it writes nothing and holds no
  write protection, so neither its fencing condition nor a comparison it
  carries is ever credited by `protection` or `observation_guard`.

`protection()` and `observation_guard()` are the two predicates behind the
optimistic route, and they consult only the reader's (or the protected
side's) own template: a guarded mutation counts only when its target pins
the object's whole identity and is structurally equal to the access's
selector, and a comparison counts as an observation only when `observation`
resolved its `expected` to `transaction_read:<bind>.<the same field>` of a
preceding read of that selector whose field selection covers the field.
There is no writer-side lookup: the version route rests on the object's
intrinsic publication, which the `VersionPublish` access of every
conflicting mutation already represents.

If, after all this, `evidence == CommitOrderEvidence::None`, extra gaps are
appended when the *reason* the edge exists at all is an unresolved unknown
rather than a proven overlap: `TransactionConflictUnknownSelectorOverlap` /
`TransactionConflictUnknownFieldOverlap` (`:1141-1153`).

## 4. From graph to proof: the actual mechanism

Two provers use `ConflictIndex`, both funneling through
`transaction_serializability::prove()` (`transaction_serializability.rs:208-286`):

1. `closure(root)` — the connected component (§41).
2. **Fast path** (§42, `:222-240`): if every closure member declares
   `isolation: serializable`, the requirement is proven immediately as
   `SerializableIsolationClosure` — no dependency graph is even built. "One
   serializable transaction among weaker conflicting ones is not enough" —
   this path requires *all* members serializable (`:232`
   `weaker.is_empty()`).
3. **Graph path** (§43–§52, `:242-251`): otherwise, `dependencies(&closure)`
   builds the directed graph (section 3 above), then
   `unconstrained_cycles(&closure, &dependencies)`
   (`transaction_conflicts.rs:1463-1516`) is the acyclicity check:
   - Builds a plain edge list `(usize, usize)` over closure-local indices
     from every `DependencyEvidence`.
   - Runs **Tarjan's strongly-connected-components algorithm**
     (`strongly_connected_components`, `:1883` — classic index/lowlink/
     stack implementation) over that edge list.
   - A component is "cyclic" if it has more than one node, or is a single
     node with a self-loop edge — this is exactly how a template's
     self-conflicting concurrent executions produce a cycle.
   - For each cyclic SCC, it collects the member `DependencyEvidence`s whose
     evidence is `CommitOrderEvidence::None`. **If none are unconstrained,
     the SCC is dropped entirely** — this is the crux insight stated in the
     doc comment: "An SCC every one of whose dependencies is commit-order
     constrained is not returned: its apparent cycle would imply a cycle in
     strict commit order and cannot occur in a committed history"
     (`:1457-1462`, and the prover module doc
     `transaction_serializability.rs:16-28`). This is the mathematical core
     of the whole technique: it isn't naive "any cycle = non-serializable";
     it's "a cycle all of whose edges are proven to respect a strict commit
     order is a logical contradiction, so only a cycle with at least one
     *unconstrained* edge is a real witness of possible non-serializability."
   - If `unconstrained_cycles` returns empty, the requirement is **proven**
     as `ConflictGraph { root, key, closure, dependencies }` — i.e. the
     "certificate" for a proven graph-route verdict is literally the full
     dependency list with, for each edge, its commit-order evidence
     (`transaction_serializability.rs:245-251`).
   - If not empty, the requirement is **unproven**, and the returned
     `TransactionSerializabilityObstacle`s name the concrete unconstrained
     cycle (as a chain `a → b → a`, `chain()` at `:422`) plus, per
     unconstrained edge, either
     `TransactionSerializabilityUnprotectedReadWriteDependency` (anti-dep) or
     `TransactionSerializabilityUnconstrainedDependency` (wr/ww)
     (`:253-283`).

The verdict type is `TransactionSerializabilityVerdict::Proven { proof,
scope } | Unproven { obstacles }` (`transaction_serializability.rs:75-84`).
`scope` is always `ProofScope::L0Only` (`:111-113`, `mod.rs:144-147`): the
whole argument rests on declared L0 (application/program) facts, never on L1
(runtime/placement) facts — reiterated by the property tests in
`tests/transaction_requirements.rs`.

**Ordering proofs** (`transaction_ordering.rs`) build on top of this. An
`OrderedBy(K, P)` obligation needs two legs (module doc
`transaction_ordering.rs:1-33`):
1. The transaction's conflict closure must be serializable — literally calls
   `transaction_serializability::prove` again with the ordering requirement's
   key (`:357`), whether or not a `SerializableBy` was separately
   declared.
2. The transaction must persist its declared `position` through a
   state-level commit guard whose *accepted* values order the commits: an
   `advance_cursor` step whose `incoming` is canonically the position, or a
   `fence` step whose `token` is the position. `prove()` (`:228`) scans the
   transaction's steps for the first such guard whose incoming value matches
   (via `ConflictIndex::same_value`, canonical value-path equality,
   `transaction_conflicts.rs:1526`), checks the guard's selector is
   *identified* by the requirement key (`key_identifies_domain`, `:1612` —
   every identity field of the guarded object must be pinned by either a
   literal or the key itself), and checks no *other* write can touch that
   managed field outside the protocol (`uncontrolled_managed_writers`,
   `:1654` — an `update` or `compare_and_set` naming the field, an `upsert`
   whose `update_fields` name it, a cursor advance under a *different* rule,
   or a fence of a cursor's field, defeats the proof). If both legs succeed,
   the proof is `Cursor{...}` or `Fence{...}` carrying the nested
   `TransactionSerializabilityProof` (`transaction_ordering.rs:89-116`).
   A `compare_and_set` is never an ordering route: a version detects
   interference, it does not order commits by a position, and comparisons a
   cursor or fence carries change nothing about the order it establishes.

Neither prover ever touches runtime/L1 declarations (worker pools, transport
ordering, member assignment) as evidence — explicitly called out as a
non-route in §57 of the DSL v4 revision ("L1 is not an ordering proof
route") and enforced by the two "runtime topology proves nothing" tests noted
above.

## 5. Cycle detection / conflict resolution / abort-retry logic

There is **no runtime abort/retry logic** here — Conseqa doesn't execute
transactions, it statically checks whether a *hypothetical* implementation
satisfying the declared facts would be serializable. So:

- **Cycle detection** = Tarjan's SCC algorithm described above
  (`strongly_connected_components`, invoked from `unconstrained_cycles`).
  This is the entire "conflict resolution" mechanism: it doesn't resolve
  anything, it *reports* the unresolved cycle as a checker obstacle (a
  `Diagnostic`, severity `Unknown` — meaning "not proven", never
  "violated": `transaction_serializability.rs:312`, and the module-level
  rule stated at `mod.rs:36-42`: "A requirement that cannot be proven is
  unproven, never 'violated'").
- What in a real database would be "abort and retry" is represented in the
  DSL as the concurrency primitives *themselves* being declared correctly:
  an atomic conditional mutation — a `compare_and_set`, or a `transition` or
  `advance_cursor` carrying a comparison — which the implementation is
  expected to lower to one conditional statement that rejects the
  transaction when its comparison fails (`UPDATE … SET …, version = version
  + 1 WHERE id = ? AND version = ?`, rejecting on zero rows; this is the
  semantic contract the analyzer trusts, not something it simulates), and
  strict two-phase-style locking via `Lock` steps. The analyzer's job is
  only to confirm the *model* declares these guards in a way that would
  constrain a real implementation's commit order; it never simulates an
  abort. A rejection is a program outcome — the transaction step's
  `rejected` arm — not an engine retry.
  A related, explicitly out-of-scope concern is flagged in the fixture
  itself: `tx.transfer_stock` locks two rows as separate steps "with no
  acquisition order between them, so a concurrent transfer in the opposite
  direction may deadlock" — and the comment states plainly: "The DSL cannot
  yet say otherwise and no checker yet looks: serializability and deadlock
  freedom are separate properties" (`tests/fixtures/flash_checkout.yaml:802-807`).
  This is a genuine, acknowledged **gap**: Conseqa proves serializability but
  does **not** prove deadlock freedom, even though the lock-ordering shape
  that causes deadlocks is visible in the very model it analyzes.

## 6. Worked example, traced through real code

Fixture: `tests/fixtures/flash_checkout.yaml` (`dsl: 6`) — a flash-sale
checkout DSL model with `operation.create_order`,
`operation.reserve_inventory`, `operation.charge_payment`,
`operation.cancel_order`, `operation.apply_payment`,
`operation.transfer_stock`. Its golden report,
`tests/fixtures/flash_checkout.report.json` (report format 9), records 11
obligations: 6 proven, 5 unknown. Three view-model tests exercise exactly
this model:

```
$ cargo test --lib transaction_proofs
test viz::transaction_proofs::tests::apply_payment_ordering_shows_its_cursor_over_the_closure ... ok
test viz::transaction_proofs::tests::apply_payment_is_drawn_as_a_constrained_graph ... ok
test viz::transaction_proofs::tests::reserve_inventory_is_drawn_with_its_unconstrained_cycle ... ok
```

And `tests/transaction_requirements.rs::flash_checkout_proves_apply_payment_and_leaves_reserve_inventory_unproven`
exercises the same paths with direct assertions on the `ConflictIndex`
output.

### 6a. A proven closure: `tx.apply_payment`

`tx.apply_payment` (fixture lines 674-763) declares:
- `requirements.serializability[0].key = input.apply_payment.captured.order_id`
- `requirements.ordering[0]`: same key, `position =
  input.apply_payment.captured.sequence`

Its body is three steps: read `object.order` (binding
`read.apply_payment.order`, fields `order_id`, `version`,
`last_applied_sequence`) → `advance_cursor` on `last_applied_sequence`
(rule `successor`, incoming = `sequence`) carrying `compare: [version =
read.apply_payment.order.version]` → `transition` (`order.mark_paid`, which
is both a `TransitionRead` and `TransitionWrite` of `status`, plus
establishes an effect intent). There is no bump: the cursor advance and the
transition each publish the order's version intrinsically
(`VersionPublish`).

Running the actual checker (`transaction_serializability::check` /
`transaction_ordering::check`) on this model — captured in the golden
report — the closure for this requirement is:

```
{tx.apply_payment, tx.cancel_order, tx.create_order.new}
```

i.e. `tx.apply_payment` is folded together with `tx.cancel_order` (fixture
lines 576-647: it reads the order's `order_id` and `version`, then applies
`transition.order.cancel` carrying `compare: [version =
read.cancel_order.order.version]`) and with `tx.create_order.new`'s
`insert` step (fixture line 262, which the analyzer conservatively treats as
able to touch *any* instance of `object.order`, per the comment at
`transaction_conflicts.rs:617-620`: "an insert creates a complete logical
instance whose identity is fixed by its derivation, which the analysis
cannot read: it may touch any instance of the object"). Crucially,
`tx.reserve_inventory` — which touches a *different* object
(`object.stock`) — is **not** in this closure (asserted directly in
`flash_checkout_proves_apply_payment_and_leaves_reserve_inventory_unproven`).

Report evidence (obligation
`oblig.operation.apply_payment.tx.apply_payment.transaction_serializability.0`,
`"status": "proven"`, `"scope": "l0_only"`) lists 57 dependency sentences
(report steps are one-based), e.g.:

> "the read-write anti-dependency from `tx.apply_payment` (step 1) to
> `tx.cancel_order` (step 2) on `object.order` is commit-ordered by
> `tx.apply_payment`'s cursor advance of `object.order` at step 2: it
> conditions its mutation on the version `read.apply_payment.order`
> observed (`version`). Every committed mutation of the versioned instance
> publishes a newer token, so a stale observation cannot participate in a
> successful commit"

> "the read-write anti-dependency from `tx.cancel_order` (step 2) to
> `tx.apply_payment` (step 3) on `object.order` is commit-ordered by
> `tx.cancel_order`'s transition of `object.order` at step 2: the access is
> part of that atomic conditional mutation, whose write protection is held
> to commit"

> "the write-read from `tx.apply_payment` (step 2) to `tx.apply_payment`
> (step 2) on `object.order` is commit-ordered by the cursor
> `object.order.last_applied_sequence` (successor): an older accepted
> position cannot commit after a newer one"

The anti-dependencies into `tx.create_order.new`'s insert are covered by
the same observed-version guard: no transaction in the model deletes an
order, so an inserted instance cannot repeat a token some read observed. The
list closes with:

> "no cyclic conflict component contains an unconstrained dependency: an
> apparent cycle would imply a cycle in strict commit order and cannot occur
> in a committed history"

That final sentence is the direct textual trace of the "drop the SCC if
fully constrained" logic in `unconstrained_cycles`.

The ordering obligation (`transaction_ordering.0`, also `"status": "proven"`)
re-embeds *the entire serializability argument above* as its first
assumption group ("transaction state history is serializable:" followed by
the same lines), then closes with the cursor route (headline text generated
at `transaction_proofs.rs:275-290`, e.g. "Proven: the cursor on
object.order.last_applied_sequence applies exactly the next position within
each input.apply_payment.captured.order_id, over a serializable closure.").
The comparison the cursor carries does not enter the ordering argument.

`transaction_proofs.rs` test `apply_payment_is_drawn_as_a_constrained_graph`
(`:823`) further confirms via the view-model: `apply.route ==
"conflict_graph"`, `apply.cycles.is_empty()`, all edges `.constrained`, at
least one edge labelled `observed version guard` (the view-model's label for
`GuardCoverage::ObservedVersion`; `atomic` / `held_protection` read
`conditional mutation`, `observed_state` reads `observed field guard`), the
`apply_payment → cancel_order` arrow carrying that label, and one arrow
(`PairView`) a `self_loop` — the self-conflict between two hypothetical
concurrent executions of `tx.apply_payment` itself, constrained by its own
observed-version guard and the write protection it holds.

### 6b. An unproven closure with a real cycle: `tx.reserve_inventory`

`tx.reserve_inventory` (fixture lines 351-~440) reads `object.stock`
(`on_hand`, `reserved`) then `update`s it (`reserved`), under
`isolation: read_committed`, with **no lock and no guarded mutation** — the
fixture's own comment calls this out as "the write-skew shape. The
serializability obligation is deliberately left unproven" (fixture lines
358-362). `object.stock` declares no version, and the update is
unconditional, so nothing conditions the mutation on what was read.
`tx.transfer_stock` (fixture lines 794-~930) also touches `object.stock`
(same object, unpinned warehouse/sku so selector overlap is `Unknown` →
treated as conflicting), also under `read_committed`.

Running the checker: closure = `{tx.reserve_inventory, tx.transfer_stock}`.
Since not all members are `serializable`-isolation, the graph route is tried.
`unconstrained_cycles` finds a genuine SCC:
`tx.reserve_inventory → tx.transfer_stock → tx.reserve_inventory` (both
directions of write-read/anti-dependency exist because both mutate the same
possibly-overlapping stock rows without any commit-order-fixing mechanism).

Report evidence (obligation
`oblig.operation.reserve_inventory.tx.reserve_inventory.transaction_serializability.0`,
`"status": "unknown"`, `"remedy": "application"`):

> "A cyclic conflict component through `tx.reserve_inventory` →
> `tx.transfer_stock` → `tx.reserve_inventory` contains a dependency no
> declared fact commit-orders..."

> "Read-write anti-dependency: `tx.reserve_inventory` may read
> `object.stock` (step 1) before `tx.reserve_inventory` writes it (step 2),
> and neither strict locking nor an atomic conditional mutation constrains
> the commit order — the anti-dependency behind write skew. Lock coverage is
> missing on the reader side... Lock coverage is missing on the writer
> side... `tx.reserve_inventory` reads this `object.stock` instance at step
> 1 and later commits a decision that can conflict with another writer, but
> it does not lock the observation, run in a serializable closure, or
> condition a mutation of the instance on the observed state... The
> selected `object.stock` domains could not be proven disjoint..."

This is `tx.reserve_inventory`'s **self-loop** edge — the analyzer treats
two concurrent executions of the *same* template (e.g. two flash-sale
reservations racing for the same SKU) as a self-conflict, exactly per the
module doc's stated design.

`flash_checkout_proves_apply_payment_and_leaves_reserve_inventory_unproven`
asserts this precisely: `DependencyGap::LockCoverageMissing` and
`DependencyGap::ObservedStateGuardMissing` both present, and the reported
cycle contains `tx.transfer_stock`.
`transaction_proofs.rs::reserve_inventory_is_drawn_with_its_unconstrained_cycle`
(`:875`) confirms the view-model side: `reserve.proven == false`,
`reserve.route == None`, at least one node `in_cycle`, and the self-loop pair
carries gap `"no observed-state guard"`.

Real fixes exist: a strict lock before the read (6c); serializable
isolation across the closure; or turning the `update` into a
`compare_and_set` that compares the observed `on_hand` and `reserved` — an
observed-state guard on an unversioned object. The remedy catalogue's
observed-guard route (`RemedyKind::ObservedGuard`,
`src/harness/executors/remedies.rs`) would make exactly that edit, but a
compare-and-set rejects, and what a rejection does is the author's
decision: it converts an `update` only where the transaction already
declares a `rejected` arm. `tx.reserve_inventory` declares none, so the
route escalates rather than invent one.

### 6c. How locking flips the verdict (regression-style trace)

`tests/transaction_requirements.rs`
(`a_strict_exclusive_lock_before_the_read_constrains_the_write_skew_edge`,
`:703`) mutates the fixture in-memory — inserting an exclusive `Lock` step
*before* the read in `tx.reserve_inventory` — and re-runs
`serializability()`. Because `tx.transfer_stock` already locks both rows
exclusively before touching them (fixture lines 808-845), every edge in the
closure now has `StrictLock` evidence, and the verdict flips from
`Unproven` to `Proven` via the `ConflictGraph` route. The companion tests
(`a_lock_acquired_after_the_access_covers_nothing`,
`a_shared_lock_covers_the_reader_and_not_the_writer`) show the boundary
conditions of the lock-timing rule: a lock inserted *after* the protected
access yields `LockAcquiredAfterProtectedAccess`, and a `Shared` (not
`Exclusive`) lock covers the reader side but leaves
`LockCoverageMissing { side: Writer }`.

## 7. Relevant tests

- `tests/transaction_requirements.rs` (2387 lines) — the primary
  integration-test suite for this whole subsystem. Beyond the ones already
  cited:
  - `serializable_isolation_across_the_closure_proves` /
    `one_weaker_transaction_in_the_closure_defeats_the_isolation_route`: the
    §42 fast path, and that one non-serializable closure member is enough to
    force the graph route.
  - The atomic-conditional-mutation tests:
    `an_observed_field_compare_and_set_constrains_the_anti_dependency` and
    `an_observed_version_compare_and_set_needs_no_writer_annotation` (the
    two observation routes; the second with no writer-side step at all),
    `an_expected_value_from_outside_the_read_is_no_observation` (an input
    or literal comparison is no evidence),
    `a_transition_carrying_the_observed_version_constrains_its_read`,
    `removing_the_observed_version_guard_leaves_the_anti_dependency_unconstrained`,
    `an_insertion_after_a_deletion_may_repeat_an_observed_version`,
    `an_upsert_guards_no_earlier_read`,
    `upserts_of_one_identity_conflict_and_of_disjoint_identities_do_not`,
    `a_read_only_observation_needs_a_real_mechanism` /
    `a_read_only_observation_at_one_instant_serializes_at_its_read`,
    `a_locked_read_orders_a_guarded_writer`, and
    `a_fence_alone_is_not_commit_order_evidence`.
  - The validation tests for the new vocabulary:
    `an_application_mutation_may_not_assign_the_version_field`,
    `a_mutation_of_a_versioned_instance_publishes_its_version_without_a_step`,
    `a_compare_and_set_identifies_one_instance_and_compares_something`,
    `a_compare_and_set_rejects_and_needs_a_rejected_arm`,
    `an_upsert_identifies_one_instance_and_leaves_its_identity_alone`.
  - The cursor/fence ordering-route tests (`ordering_needs_a_cursor_or_fence`
    onward): missing cursor/fence, position mismatch, key-domain mismatch,
    uncontrolled managed-field writers, "a managed field has one role" (a
    field can't be both a cursor and something else).
  - `no_transaction_verdict_depends_on_the_runtime_topology`
    and `a_serial_pool_and_affinity_prove_nothing_about_a_transaction`:
    proves the L0/L1 independence claim by mutating the fixture's `runtime:`
    section (pool concurrency, routing) and asserting the transaction
    verdicts are byte-identical.
- `src/viz/transaction_proofs.rs` — three unit tests
  (`apply_payment_is_drawn_as_a_constrained_graph`,
  `reserve_inventory_is_drawn_with_its_unconstrained_cycle`,
  `apply_payment_ordering_shows_its_cursor_over_the_closure`) verifying the
  view-model faithfully reflects the underlying graph/cycle/proof structure,
  used above as the worked example.
- `tests/fixtures/flash_checkout.report.json` — a golden, checked-in,
  full-report snapshot of running the checker over `flash_checkout.yaml`;
  the authoritative source for the exact prose sentences the prover emits
  for both the proven and unproven cases (quoted above).
- `tests/report.rs`, `tests/verification.rs` — broader report/verification
  pipeline tests; `tests/verification.rs` covers the other requirement
  families (idempotency, recoverability, result-replay) that interact with
  but are distinct from the transaction graph.

## 8. Summary answers to the captain's intent

**How does the graph aid in generating serialization and ordering proofs?**
It doesn't just "aid" — it *is* the proof, in certificate form. A proven
`SerializableBy` verdict's `ConflictGraph` variant literally carries the full
list of `DependencyEvidence` with each edge's commit-order justification
(`transaction_serializability.rs:100-105`); a reader (or the JSON report, or
the viz layer) can walk that list and verify by hand that (a) it's exactly
the conflict closure, (b) every potential dependency is listed, and (c) every
dependency on a cycle names a specific declared fact (isolation / lock /
atomic conditional mutation with the guard coverage that credits it /
read-only observation / ordered cursor) that forces its commit order — so no
cycle can actually occur in a real committed history. Ordering proofs layer a
cursor/fence commit-guard argument on top of the *same* serializability
certificate for the ordering key's closure.

**How are "concurrently running" transactions identified?** Statically and
conservatively, with zero runtime input: any two inline transaction
templates in the entire model (including two executions of the same
template) are treated as potentially concurrent for the purposes of the
graph whenever the analyzer cannot *prove* their selected object domains and
field footprints are disjoint — unknown overlap always counts as a conflict.
This is a deliberate worst-case assumption, explicitly justified because no
runtime/placement fact (worker pools, routing, transport ordering) is
trustworthy evidence that two transactions can't actually race (they can,
after redelivery, worker replacement, or failure-driven reordering).

## Recommendation

No bug was found in the original DSL 4 investigation, and the DSL 6 revision
removed its one structural awkwardness — a proof that had to pair a reader's
`ValidateVersion` with a writer's `BumpVersion`, which no single storage
operation implements — in favour of guards a database actually performs. The
one substantive gap worth flagging to the team (self-documented in the
fixture, section 5 above) is that **deadlock freedom is out of scope** — the
analyzer can prove a serializable committed history while the same
lock-acquisition-order shape it just certified could deadlock in practice
(`tx.transfer_stock`'s two unconditionally-ordered exclusive locks). This is
called out in-repo as a known, deliberate limitation ("A model-wide deadlock
checker remains earmarked (§71...)", `src/analyzer/verification/mod.rs:25-29`).
