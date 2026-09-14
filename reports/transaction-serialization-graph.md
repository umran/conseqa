# Conseqa's Transaction Serialization Graph

Read-only investigation. No files in the conseqa worktree were modified. All
line numbers below were checked directly against the worktree at
`/Users/umran/.treehouse/conseqa-2e3c3b/1/conseqa` (detached HEAD on
`master`, top commit `8637bd0`).

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
fact (isolation, locking, version validation, or an ordered cursor). If so,
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

Core module: `src/analyzer/verification/transaction_conflicts.rs` (1656
lines). Its module doc (lines 1–37) explicitly frames it as "the shared
machinery of the transaction serializability and ordering provers
(§35–§52 of the DSL v4 revision)". The corresponding spec sections live in
`Conseqa_Transaction_Consistency_and_Ordering_Revision__DSL_v4.md:1294-1940`.

Two provers consume this shared index:
- `src/analyzer/verification/transaction_serializability.rs` (589 lines) —
  discharges `SerializableBy(K)` requirements.
- `src/analyzer/verification/transaction_ordering.rs` (526 lines) —
  discharges `OrderedBy(K, P)` requirements, and internally *re-invokes* the
  serializability prover for its own closure (`transaction_ordering.rs:349-362`).

A visualization/UI-facing extraction layer sits on top:
- `src/viz/transaction_proofs.rs` (902 lines) — turns the same
  `ConflictIndex` into a `TransactionProofs` view-model (nodes, edges, pairs,
  cycles) "so the front end draws the argument rather than paraphrasing it"
  (`transaction_proofs.rs:1-19`). Wired into `src/viz/render.rs:14,30,44,57`.

### Nodes: `TransactionTemplate` / `TransactionRef`

`transaction_conflicts.rs:59-63` — a `TransactionRef` is `{operation,
transaction, location}`: one **inline transaction declaration** in the
program is one template. Concurrent executions of the same template are
analyzed as two distinct nodes (module doc, `transaction_conflicts.rs:35-37`
and `:56`), which is why a template can conflict with itself.

`transaction_conflicts.rs:244-255` — `TransactionTemplate<'a>` bundles the
template's:
- `accesses: Vec<TransactionAccess>` — every persistent-state touch
  (`:165-183`)
- `locks: Vec<LockAccess>` — `Lock` steps, indexed apart from accesses
  because "a lock observes and mutates nothing" (`:185-195`)
- `validations: Vec<VersionValidation>` — `ValidateVersion` steps, with
  whether the expected version is a preceding *observed* read of that same
  instance (`:206-215`, resolved by `observes_version` at `:1258-1291`)
- `bumps: Vec<(usize, ObjectSelector)>` — `BumpVersion` steps (`:251-252`)
- `artifacts: Vec<CommitArtifact>` — outbox admissions, effect-intent
  establishments, transaction outputs (`:217-240`); these are *never*
  treated as conflict accesses (`:14-15`, `:1401` in the spec)

The whole-model index is `ConflictIndex<'a>` (`:258-268`), built once per
model by `ConflictIndex::build` (`:270-310`), which walks every operation's
program (`model.operations`), finds every inline `transaction` step via
`operation.program.transactions()`, and turns each into a `TransactionTemplate`
via `template_of` (`:336-599`).

### Access modes and footprints (`AccessMode`, `AccessFields`)

`transaction_conflicts.rs:86-127` defines the ten access modes (`Read`,
`Write`, `Insert`, `Delete`, `TransitionRead`, `TransitionWrite`,
`VersionValidate`, `VersionBump`, `CursorReadWrite`, `FenceReadWrite`), each
classified by `.reads()` / `.writes()`. `template_of` (`:365-576`) is a match
over every `TransactionStep` variant that derives the access(es) it implies —
e.g. a `Transition` step produces *both* a `TransitionRead` and a
`TransitionWrite` of the state-machine's governed state field (`:439-465`),
and can also emit `CommitArtifact::OutboxWrite` for transition-scoped effects
(`:466-485`). `AccessFields` (`:147-160`) is `All | Only(BTreeSet<FieldPath>) |
Unknown`, where an undeclared field footprint (an ordinary `write` naming no
fields) is `Unknown` and is "treated as potentially conflicting with
everything" (`:157-159`).

### Edges: what a conflict/dependency edge means

There are two distinct graphs built here, and it's important not to conflate
them:

1. **The undirected "conflict closure" graph** (§41 of the spec,
   `transaction_conflicts.rs:689-706`, `templates_conflict` at `:680-687`).
   An edge exists between two templates when *any* access pair between them
   is a `conflict()` (`:659-677`): at least one access writes, and neither
   the selector domains nor the field footprints are provably disjoint. This
   graph is used only to compute the **closure** — the connected component
   containing the root transaction — via a BFS/frontier walk
   (`closure()`, `:692-706`). This is deliberately transitive: "a
   serialization cycle can pass through a transaction that never touches the
   root's state directly" (module doc `:19-23`, spec §41 example at
   `Conseqa_Transaction_Consistency_and_Ordering_Revision__DSL_v4.md:1464-1472`).

2. **The directed "potential serialization-dependency graph"** (§43,
   `dependencies()` at `transaction_conflicts.rs:714-763`), built *only* over
   the members of a closure once it's known. For every ordered pair of
   accesses `(a, b)` across all closure members (including a template against
   itself) that conflict, it emits:
   - `WriteRead` if `a` writes and `b` reads (`:725-734`) — "the target reads
     what the source wrote" (`:1469`)
   - `ReadWriteAntiDependency` if `a` reads and `b` writes (`:736-745`) — the
     write-skew anti-dependency (`:1471-1472`)
   - `WriteWrite` if both write (`:747-756`)

   Each edge (`DependencyEvidence`, `:1602-1636`) records `kind`, `source`,
   `source_step`, `source_mode`, `target`, `target_step`, `target_mode`,
   `object`, `selector_overlap`, `field_overlap`, `evidence`
   (`CommitOrderEvidence`), an optional `fence`, and any `gaps`.

## 2. Detecting "concurrently running" transactions

There is **no dynamic/runtime concurrency detection** in Conseqa — no
scheduler, timestamps, or lock manager observed at execution time. Concurrency
is decided purely from the declared model, conservatively:

- **Which templates could ever be concurrent with each other**: implicitly,
  *any two inline transaction templates in the whole model* are candidates —
  `ConflictIndex::build` indexes every transaction of every operation
  (`:295-307`), and `closure()` (`:692-706`) walks outward from the root
  through every template whose accesses could conflict, with no notion of
  "these two operations can never run at the same wall-clock time." The
  doc comment is explicit about this scope: "no runtime fact ever
  participates" in serializability (module doc `:30-31`), and the checker
  module doc reiterates that the transaction families "never rest on L1"
  (runtime/placement facts) "because no placement, transport, or capacity
  fact survives redelivery, worker replacement, or reordering after failure"
  (`src/analyzer/verification/mod.rs:48-55`). Concretely: `apply_payment` and
  `cancel_order` are two *different* operations, driven by different message
  types, dispatched to different logical work — yet because both touch
  `object.order` they are unconditionally treated as potentially concurrent
  and put in one closure (worked example below).
  See also `tests/transaction_requirements.rs:1434-1533`
  (`no_transaction_verdict_depends_on_the_runtime_topology`,
  `a_serial_pool_and_affinity_prove_nothing_about_a_transaction`) which
  assert this directly: changing L1 (worker pool serialization, routing
  affinity) never changes a transaction verdict.

- **Which specific accesses "conflict" and thus create an edge**: this is
  where the actual concurrency-relevance test lives, `conflict()`
  (`transaction_conflicts.rs:659-677`):
  1. At least one of the two accesses must be a write (`.mode.writes()`,
     line 660) — two reads never conflict.
  2. `selector_overlap()` (`:606-654`) must not prove `Disjoint`. This checks,
     in order: different object ids → disjoint (`:611-612`); pinned-literal
     predicates naming incompatible values on the same field → disjoint
     (`:619-630`); both predicates `All` → overlapping (`:632-636`); complete
     declared identity fields pinned by *equal* literals on both sides →
     overlapping (single-instance match, `:640-648`); otherwise →
     `Unknown` (`:653`), which the conflict test treats as *not* disjoint,
     i.e. as a conflict (§39, spec `:1405-1426`: "Unknown overlap is never
     treated as disjoint"). Notably, "two executions evaluate their
     references independently, so equal references prove nothing across
     them" (`:651-652`) — a shared symbolic input reference (e.g. both
     reading `input:order_id`) does *not* prove the two executions target
     the same row; only literal/identity matches do.
  3. `field_overlap()` (`:1333-1353`) must not prove `Disjoint`: `Unknown`
     footprint on either side → `Overlapping`(unknown treated as
     conflicting); `All` on either side → `Overlapping`; otherwise field-path
     prefix matching between the two `Only` sets.

  So "concurrency relevance" reduces to: *could these two accesses, run by
  any two executions anywhere in the modeled system, touch the same
  persistent state in a way that isn't provably impossible?* If yes, an edge
  exists and the pair is folded into the closure regardless of how far apart
  the two operations are in the program.

This conservative design is stated as a first-class principle: "Everything
here is conservative (§70): an unknown selector overlap is potentially
overlapping, an unknown field footprint potentially conflicting, an
unspecified isolation no isolation, and incomplete version, cursor, or fence
coverage no credit at all" (module doc, `transaction_conflicts.rs:31-34`).

## 3. How edges get added — the exact trigger conditions

Once the conflict closure is known, `dependencies()`
(`transaction_conflicts.rs:714-763`) creates one `DependencyEvidence` per
conflicting `(source access, target access)` ordered pair, for **every**
kind that applies (a pair can produce more than one edge, e.g. a
`write→write` pair where both sides also read is possible via version
validation flows). Then `dependency()` (`:765-917`) classifies *why* the
commit order is (or isn't) fixed — this is the "commit-order evidence"
computation, §45–§51 of the spec:

- **`WriteRead`** (`:800-814`): commit-ordered (`IntrinsicCommittedRead`) if
  the target's isolation is not `Unspecified` — "the target reads only
  committed writes" (§46). Otherwise, if the target validates a version it
  observed and the source bumps that version, it falls back to
  `VersionValidation` evidence (`version_evidence`, `:919-929`). Otherwise
  `CommitOrderEvidence::None`, with an `IsolationUnspecified` gap recorded
  (`:808-812`).
- **`WriteWrite`** (`:816-844`): commit-ordered (`AtomicWriteOrder`) if
  *both* sides declare isolation ≠ `Unspecified` (§46). Otherwise falls back
  to version validation on either side (`:827-830`), else `None` with
  `IsolationUnspecified` gaps for whichever side lacks isolation.
- **`ReadWriteAntiDependency`** (`:846-884`, the write-skew edge, §44/§47):
  first tries `covering_lock()` on both sides (`:960-1008`) — a *strict
  lock* protects the edge only if the reader holds a shared-or-exclusive lock
  and the writer holds an exclusive lock, **both acquired at an earlier
  step than the access they protect** (§48; a lock acquired after the
  access "protects no earlier observation", `:982-999`). If either lock is
  missing/late, it falls back to version validation (source validates,
  target bumps, `:857-858`). Otherwise `None`, with `LockCoverageMissing` /
  `LockAcquiredAfterProtectedAccess` / `VersionValidationMissing` /
  `VersionBumpMissing` gaps recorded per side (`:860-878`).
- **Ordered-cursor short-circuit** (`:787-797`): if both accesses are
  `CursorReadWrite` on the same field under the *same* declared rule
  (`Successor` or `MonotonicAfter`), the edge is `OrderedCursor` evidence
  regardless of kind — "an older accepted position cannot commit after a
  newer one" (§50).
- **Fences are never evidence** — a `fence` pair is recorded separately as
  `dependency.fence` (`:779-785`) but "equal fencing tokens serialize
  nothing... a fence constrains no dependency by itself" (§51,
  `DependencyEvidence` doc `:1622-1624`).

`validates()` (`:933-938`) and `bumps()` (`:940-955`) are the two predicates
behind the OCC (optimistic concurrency control) fallback: a template
"validates" an access only if it has a `ValidateVersion` step whose expected
version is a *preceding read of the same instance's version field*
(`observes_version`, `:1258-1291` — the read must bind the value later cited
by the validation, target the same selector, precede the validation step,
and cover the version field); a template "bumps" an access if the access
itself is a `VersionBump`/`Delete`/`Insert`, or the template has a
`BumpVersion` of the same selector elsewhere (`:947-955`).

If, after all this, `evidence == CommitOrderEvidence::None`, extra gaps are
appended when the *reason* the edge exists at all is an unresolved unknown
rather than a proven overlap: `TransactionConflictUnknownSelectorOverlap` /
`TransactionConflictUnknownFieldOverlap` (`:888-900`).

## 4. From graph to proof: the actual mechanism

Two provers use `ConflictIndex`, both funneling through
`transaction_serializability::prove()` (`transaction_serializability.rs:203-280`):

1. `closure(root)` — BFS connected component (§41).
2. **Fast path** (§42, `:217-234`): if every closure member declares
   `isolation: serializable`, the requirement is proven immediately as
   `SerializableIsolationClosure` — no dependency graph is even built. "One
   serializable transaction among weaker conflicting ones is not enough" —
   this path requires *all* members serializable (`:227` `weaker.is_empty()`).
3. **Graph path** (§43–§52, `:237-247`): otherwise, `dependencies(&closure)`
   builds the directed graph (section 3 above), then
   `unconstrained_cycles(&closure, &dependencies)`
   (`transaction_conflicts.rs:1020-1074`) is the acyclicity check:
   - Builds a plain edge list `(usize, usize)` over closure-local indices
     from every `DependencyEvidence` (`:1031-1036`).
   - Runs **Tarjan's strongly-connected-components algorithm**
     (`strongly_connected_components`, `:1357-1433` — classic index/lowlink/
     stack implementation) over that edge list.
   - A component is "cyclic" if it has more than one node, or is a single
     node with a self-loop edge (`:1041-1044`) — this is exactly how a
     template's self-conflicting concurrent executions produce a cycle.
   - For each cyclic SCC, it collects the member `DependencyEvidence`s whose
     evidence is `CommitOrderEvidence::None` (`:1055-1063`). **If none are
     unconstrained, the SCC is dropped entirely** — this is the crux
     insight stated in the module doc: "An SCC every one of whose
     dependencies is commit-order constrained is not returned: its apparent
     cycle would imply a cycle in strict commit order and cannot occur in a
     committed history" (`:1014-1019`, also spec §52
     `:1757-1789`, and prover module doc
     `transaction_serializability.rs:16-24`). This is the mathematical core
     of the whole technique: it isn't naive "any cycle = non-serializable";
     it's "a cycle all of whose edges are proven to respect a strict commit
     order is a logical contradiction, so only a cycle with at least one
     *unconstrained* edge is a real witness of possible non-serializability."
   - If `unconstrained_cycles` returns empty, the requirement is **proven**
     as `ConflictGraph { root, key, closure, dependencies }` — i.e. the
     "certificate" for a proven graph-route verdict is literally the full
     dependency list with, for each edge, its commit-order evidence
     (`transaction_serializability.rs:241-246`).
   - If not empty, the requirement is **unproven**, and the returned
     `TransactionSerializabilityObstacle`s name the concrete unconstrained
     cycle (as a chain `a → b → a`, `chain()` at `:415-426`) plus, per
     unconstrained edge, either
     `TransactionSerializabilityUnprotectedReadWriteDependency` (anti-dep) or
     `TransactionSerializabilityUnconstrainedDependency` (wr/ww)
     (`:255-276`).

The verdict type is `TransactionSerializabilityVerdict::Proven { proof,
scope } | Unproven { obstacles }` (`transaction_serializability.rs:70-78`).
`scope` is always `ProofScope::L0Only` (`:103-108`, `mod.rs:142-148`): the
whole argument rests on declared L0 (application/program) facts, never on L1
(runtime/placement) facts — reiterated by the property tests in
`tests/transaction_requirements.rs:1434-1533`.

**Ordering proofs** (`transaction_ordering.rs`) build on top of this. An
`OrderedBy(K, P)` obligation needs two legs (§54, doc comment
`transaction_ordering.rs:1-27`):
1. The transaction's conflict closure must be serializable — literally calls
   `transaction_serializability::prove` again with the ordering requirement's
   key (`:349-362`), whether or not a `SerializableBy` was separately
   declared.
2. The transaction must persist its declared `position` through a
   state-level commit guard whose *accepted* values order the commits: an
   `advance_cursor` step whose `incoming` is canonically the position
   (§55), or a `fence` step whose `token` is the position (§56). `prove()`
   (`:221-403`) scans the transaction's steps for the first such guard whose
   incoming value matches (via `ConflictIndex::same_value`, canonical
   value-path equality, `transaction_conflicts.rs:1083-1106`), checks the
   guard's selector is *identified* by the requirement key
   (`key_identifies_domain`, `:1169-1206` — every identity field of the
   guarded object must be pinned by either a literal or the key itself), and
   checks no *other* write can touch that managed field outside the protocol
   (`uncontrolled_managed_writers`, `:1211-1251` — an ordinary write naming
   the field, or a cursor advance under a *different* rule, defeats the
   proof). If both legs succeed, the proof is `Cursor{...}` or `Fence{...}`
   carrying the nested `TransactionSerializabilityProof` (`transaction_ordering.rs:83-110`).

Neither prover ever touches runtime/L1 declarations (worker pools, transport
ordering, member assignment) as evidence — explicitly called out as a
non-route in spec §57 ("L1 is not an ordering proof route") and enforced by
the two "runtime topology proves nothing" tests noted above.

## 5. Cycle detection / conflict resolution / abort-retry logic

There is **no runtime abort/retry logic** here — Conseqa doesn't execute
transactions, it statically checks whether a *hypothetical* implementation
satisfying the declared facts would be serializable. So:

- **Cycle detection** = Tarjan's SCC algorithm described above
  (`transaction_conflicts.rs:1357-1433`, invoked from `unconstrained_cycles`
  at `:1020-1074`). This is the entire "conflict resolution" mechanism: it
  doesn't resolve anything, it *reports* the unresolved cycle as a checker
  obstacle (a `Diagnostic`, severity `Unknown` — meaning "not proven", never
  "violated": `transaction_serializability.rs:294-321`, and the module-level
  rule stated at `mod.rs:34-40`: "A requirement that cannot be proven is
  unproven, never 'violated'").
- What in a real database would be "abort and retry" is represented in the
  DSL as the OCC / locking *primitives themselves* being declared correctly:
  `ValidateVersion` (which the runtime is expected to make reject a stale
  commit — this is the semantic contract the analyzer trusts, not something
  it simulates), and strict two-phase-style locking via `Lock` steps. The
  analyzer's job is only to confirm the *model* declares these guards in a
  way that would make a real implementation's commit order consistent; it
  never simulates an abort.
  A related, explicitly out-of-scope concern is flagged in the fixture
  itself: `tx.transfer_stock` locks two rows in a fixed, unconditional order
  with "no acquisition order between them, so a concurrent transfer in the
  opposite direction may deadlock" — and the comment states plainly: "The
  DSL cannot yet say otherwise and no checker yet looks: serializability and
  deadlock freedom are separate properties" (`tests/fixtures/flash_checkout.yaml:830-835`).
  This is a genuine, acknowledged **gap**: Conseqa proves serializability but
  does **not** prove deadlock freedom, even though the lock-ordering shape
  that causes deadlocks is visible in the very model it analyzes. Noted here
  per the brief's read-only instruction — not fixed.

## 6. Worked example, traced through real code

Fixture: `tests/fixtures/flash_checkout.yaml` — a flash-sale checkout DSL
model with `operation.create_order`, `operation.reserve_inventory`,
`operation.charge_payment`, `operation.cancel_order`,
`operation.apply_payment`, `operation.transfer_stock`. I ran the test suite
that exercises exactly this model:

```
$ cargo test --lib transaction_proofs
running 3 tests
test viz::transaction_proofs::tests::apply_payment_ordering_shows_its_cursor_over_the_closure ... ok
test viz::transaction_proofs::tests::apply_payment_is_drawn_as_a_constrained_graph ... ok
test viz::transaction_proofs::tests::reserve_inventory_is_drawn_with_its_unconstrained_cycle ... ok
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 24 filtered out
```

And `tests/transaction_requirements.rs::flash_checkout_proves_apply_payment_and_leaves_reserve_inventory_unproven`
(also passing) exercises the same paths with direct assertions on the
`ConflictIndex` output.

### 6a. A proven closure: `tx.apply_payment`

`tx.apply_payment` (fixture lines 673-806) declares:
- `requirements.serializability[0].key = input.apply_payment.captured.order_id`
- `requirements.ordering[0]`: same key, `position =
  input.apply_payment.captured.sequence`

Its body: read `object.order` (binding `version`, `last_applied_sequence`) →
`validate_version` against that read → `advance_cursor` on
`last_applied_sequence` (rule `successor`, incoming = `sequence`) →
`transition` (`order.mark_paid`, which is both a `TransitionRead` and
`TransitionWrite` of `status`, plus establishes an effect intent) →
`bump_version`.

Running the actual checker (`transaction_serializability::check` /
`transaction_ordering::check`) on this model — captured as the golden fixture
`tests/fixtures/flash_checkout.report.json` — the closure for this
requirement is:

```
{tx.apply_payment, tx.cancel_order, tx.create_order.new}
```

i.e. `tx.apply_payment` is folded together with `tx.cancel_order` (both touch
`object.order` by `order_id`, and `tx.cancel_order` also validates+bumps the
version, `fixture:583-631`) and with `tx.create_order.new`'s `insert` step
(which the analyzer conservatively treats as able to touch *any* instance of
`object.order`, per `transaction_conflicts.rs:400-418`'s comment: "an insert
creates a complete logical instance whose identity is fixed by its
derivation, which the analysis cannot read: it may touch any instance of the
object"). Crucially, `tx.reserve_inventory` — which touches a *different*
object (`object.stock`) — is **not** in this closure (asserted directly in
`tests/transaction_requirements.rs:261-268`).

Report evidence (`tests/fixtures/flash_checkout.report.json:1-87`,
obligation `oblig.operation.apply_payment.tx.apply_payment.transaction_serializability.0`,
`"status": "proven"`, `"scope": "l0_only"`) lists ~50 individual dependency
sentences, e.g.:

> "the read-write anti-dependency from `tx.apply_payment` (step 1) to
> `tx.apply_payment` (step 3) on `object.order` is commit-ordered by version
> validation: `tx.apply_payment` validates `object.order.version` at commit
> against the version it observed, and the conflicting mutation advances that
> version, so a stale observation rejects instead of committing"

> "the write-read from `tx.apply_payment` (step 3) to `tx.apply_payment`
> (step 3) on `object.order` is commit-ordered by the cursor
> `object.order.last_applied_sequence` (successor): an older accepted
> position cannot commit after a newer one"

...and closes with:

> "no cyclic conflict component contains an unconstrained dependency: an
> apparent cycle would imply a cycle in strict commit order and cannot occur
> in a committed history"

That final sentence is the direct textual trace of the "drop the SCC if fully
constrained" logic at `transaction_conflicts.rs:1014-1019` / `:1055-1071`.

The ordering obligation (`transaction_ordering.0`, also `"status": "proven"`)
literally re-embeds *the entire serializability argument above* as its first
assumption group ("transaction state history is serializable:" followed by
the same ~50 lines), then closes with the cursor route (headline text
generated at `transaction_proofs.rs:272-296`, e.g. "Proven: the cursor on
object.order.last_applied_sequence applies exactly the next position within
each input.apply_payment.captured.order_id, over a serializable closure.").

`transaction_proofs.rs` test `apply_payment_is_drawn_as_a_constrained_graph`
(`:798-848`) further confirms via the view-model: `apply.route ==
"conflict_graph"`, `apply.cycles.is_empty()`, all edges `.constrained`, at
least one `version validation` evidence, and one arrow (`PairView`) is a
`self_loop` — the self-conflict between two hypothetical concurrent
executions of `tx.apply_payment` itself, constrained by its own
version-validate/bump-version pair.

### 6b. An unproven closure with a real cycle: `tx.reserve_inventory`

`tx.reserve_inventory` (fixture lines 334-458) reads `object.stock`
(`on_hand`, `reserved`) then writes it back (`reserved`), under
`isolation: read_committed`, with **no lock and no version protocol** — the
fixture's own comment calls this out as "the write-skew shape... deliberately
left unproven" (fixture lines 354-358). `tx.transfer_stock`
(fixture lines 807-946) also touches `object.stock` (same object, unpinned
warehouse/sku so selector overlap is `Unknown` → treated as conflicting),
also under `read_committed`.

Running the checker: closure = `{tx.reserve_inventory, tx.transfer_stock}`.
Since not all members are `serializable`-isolation, the graph route is tried.
`unconstrained_cycles` finds a genuine SCC:
`tx.reserve_inventory → tx.transfer_stock → tx.reserve_inventory` (both
directions of write-read/anti-dependency exist because both mutate the same
possibly-overlapping stock rows without any commit-order-fixing mechanism).

Report evidence (`tests/fixtures/flash_checkout.report.json:311-346`,
`"status": "unknown"`, `"remedy": "application"`):

> "A cyclic conflict component through `tx.reserve_inventory` →
> `tx.transfer_stock` → `tx.reserve_inventory` contains a dependency no
> declared fact commit-orders..."

> "Read-write anti-dependency: `tx.reserve_inventory` may read
> `object.stock` (step 1) before `tx.reserve_inventory` writes it (step 2),
> and neither strict locking nor version validation constrains the commit
> order — the anti-dependency behind write skew. Lock coverage is missing on
> the reader side... Lock coverage is missing on the writer side...
> `tx.reserve_inventory` does not validate the version of `object.stock`...
> The selected `object.stock` domains could not be proven disjoint..."

This is `tx.reserve_inventory`'s **self-loop** edge — the analyzer treats
two concurrent executions of the *same* template (e.g. two flash-sale
reservations racing for the same SKU) as a self-conflict, exactly per
`transaction_conflicts.rs:36-37`'s stated design.

`tests/transaction_requirements.rs:303-326` asserts this precisely:
`DependencyGap::LockCoverageMissing` and `DependencyGap::VersionValidationMissing`
both present, and the reported cycle contains `tx.transfer_stock`.
`transaction_proofs.rs::reserve_inventory_is_drawn_with_its_unconstrained_cycle`
(`:850-878`) confirms the view-model side: `reserve.proven == false`,
`reserve.route == None`, at least one node `in_cycle`, and the self-loop pair
carries gap `"no version validation"`.

### 6c. How locking flips the verdict (regression-style trace)

`tests/transaction_requirements.rs:690-723`
(`a_strict_exclusive_lock_before_the_read_constrains_the_write_skew_edge`)
mutates the fixture in-memory — inserting an exclusive `Lock` step *before*
the read in `tx.reserve_inventory` — and re-runs `serializability()`. Because
`tx.transfer_stock` already locks both rows exclusively before touching them
(fixture lines 836-873), every edge in the closure now has `StrictLock`
evidence, and the verdict flips from `Unproven` to `Proven` via the
`ConflictGraph` route. The companion tests
(`:726-751`, `:753-794`) show the boundary conditions of the lock-timing
rule: a lock inserted *after* the protected access yields
`LockAcquiredAfterProtectedAccess`, and a `Shared` (not `Exclusive`) lock
covers the reader side but leaves `LockCoverageMissing { side: Writer }`.

## 7. Relevant tests

- `tests/transaction_requirements.rs` (1511 lines) — the primary
  integration-test suite for this whole subsystem. Beyond the ones already
  cited:
  - `:1010-1069` — `serializable_isolation_across_the_closure_proves` /
    `one_weaker_transaction_in_the_closure_defeats_the_isolation_route`: the
    §42 fast path, and that one non-serializable closure member is enough to
    force the graph route.
  - `:972-1009` — `removing_the_validation_leaves_the_anti_dependency_unconstrained`:
    directly demonstrates the OCC evidence disappearing when a
    `ValidateVersion` step is removed.
  - `:1078-1240` — the cursor/fence ordering-route tests: missing
    cursor/fence, position mismatch, key-domain mismatch, uncontrolled
    managed-field writers, "a managed field has one role" (a field can't be
    both a cursor and something else).
  - `:1434-1533` — `no_transaction_verdict_depends_on_the_runtime_topology`
    and `a_serial_pool_and_affinity_prove_nothing_about_a_transaction`:
    proves the L0/L1 independence claim by mutating the fixture's `runtime:`
    section (pool concurrency, routing) and asserting the transaction
    verdicts are byte-identical.
- `src/viz/transaction_proofs.rs:777-902` — three unit tests
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
  pipeline tests; `tests/verification.rs` (4368 lines) covers the other
  requirement families (idempotency, recoverability, result-replay) that
  interact with but are distinct from the transaction graph.

All of the above ran green in this worktree:
```
$ cargo test --lib transaction_proofs
test result: ok. 3 passed; 0 failed
```
(I did not run the full `cargo test` suite, to keep this investigation
read-only/side-effect-free and fast; the targeted run above plus direct
source reading was sufficient to verify every claim in this report against
real, current code.)

## 8. Summary answers to the captain's intent

**How does the graph aid in generating serialization and ordering proofs?**
It doesn't just "aid" — it *is* the proof, in certificate form. A proven
`SerializableBy` verdict's `ConflictGraph` variant literally carries the full
list of `DependencyEvidence` with each edge's commit-order justification
(`transaction_serializability.rs:95-100`); a reader (or the JSON report, or
the viz layer) can walk that list and verify by hand that (a) it's exactly
the conflict closure, (b) every potential dependency is listed, and (c) every
dependency on a cycle names a specific declared fact (isolation / lock /
version-validate+bump / ordered cursor) that forces its commit order — so no
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

No bug was found; the implementation is consistent with its own spec
(`Conseqa_Transaction_Consistency_and_Ordering_Revision__DSL_v4.md` §35–§58)
and its test suite is thorough and exercises both the "proven" and "unproven"
paths concretely. The one substantive gap worth flagging to the team (already
self-documented in the fixture, section 5 above) is that **deadlock freedom
is out of scope** — the analyzer can prove a serializable committed history
while the same lock-acquisition-order shape it just certified could deadlock
in practice (`tx.transfer_stock`'s two unconditionally-ordered exclusive
locks). This is called out in-repo as a known, deliberate limitation ("A
model-wide deadlock checker remains earmarked (§71...)",
`src/analyzer/verification/mod.rs:26-28`), not something to fix under this
read-only task.
