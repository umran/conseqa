# conseqa-viz

`conseqa-viz` renders an Conseqa model — its services, operations
and their programs, topics, state machines, and the checker's verdicts
on them — as a single self-contained, interactive HTML file. The
output opens directly from disk, makes no network requests, and can be
attached to a review, a PR, or a design document as-is.

```
cargo run --bin conseqa-viz -- tests/fixtures/flash_checkout.yaml
# wrote tests/fixtures/flash_checkout.html

cargo run --bin conseqa-viz -- model.yaml --verify \
    --out model.html \
    --title "checkout architecture"
```

Analyzer validation runs before rendering; diagnostics go to stderr and
rendering proceeds anyway, so imperfect models can still be inspected
(`--no-validate` silences the pass).

`--verify` runs the model checker and overlays its obligation report.
`--report <PATH>` overlays a report produced earlier by
`conseqa model.yaml --report <PATH>`; the two are mutually exclusive.

## Views

**System view** (`#/system`). Services are drawn as boundary boxes with
their operations inside; topics, outboxes, external systems, and a
synthetic "clients" vertex (for request inputs no modeled operation
invokes) sit around them. Edges are the model's information routes:

- publication effects: operation → topic
- subscription inputs: topic → operation
- outbox writes: operation → outbox — drawn distinctly from
  publication, because the admission is atomic with a transaction's
  commit; the edge names the transaction whose commit admits it
- outbox inputs: outbox → operation — the outbox's one exclusive
  consumer, re-driven intrinsically until consumption succeeds — and,
  with L1 drawn, the runtime's partitioning, ordering, routing, member
  assignment, and batching facts
- request effects: operation → operation
- external effects: operation → external system
- client requests: clients → operation, for request inputs no modeled
  operation invokes

An **outbox** node (dashed teal, distinct from a topic) belongs to a
data model and says so on the card: it is the transactional message
collection of §5.1 of the semantics, not a logical channel.

Operation-owned effects are derived directly from the program — its
inline `execute_effect` sites and `establish_effect_intent` sites —
never from a separate declaration inventory. A dashed edge is a
*declared but unexecuted* effect: an established intent no program
step executes; a solid edge's detail names the program steps that
execute the effect, by location. Effects owned by state-machine
transitions are attributed to the operations whose transactions bind
them through transition applications and marked "via transition".
Click anything for a structured detail panel; double-click an
operation to drill in. The top bar's filter box dims non-matching
vertices, and a fit control in the canvas corner re-centres the graph.

*Layout.* The graph is layered left to right along the flow of
information: columns follow reachability, and everything that can happen
in parallel stacks vertically inside a column, so a large architecture
grows along both axes instead of into a strip. An edge to the
neighbouring column crosses the gutter directly; a longer one, a return,
or a hop within a column climbs a gutter into the channel reserved above
its band and comes back down another, so no edge is ever drawn through a
card. A drawing too wide to fit at a readable size wraps: its columns
break into bands the way a paragraph breaks into lines, at whichever
width leaves everything largest once fitted. An edge into the
neighbouring band continues the way a line of text wraps — through its
own gutter into the one channel separating the two bands, straight
across, and on into its target; only an edge that must clear whole
bands travels around the outside. Small graphs never wrap.

*Layers.* L0 — the abstract application machine — is always drawn. L1,
the declared runtime realization, is switched from the top bar (`L0` is
a label, not a control: the machine is the model, not an overlay on it),
and it is laid *onto* the machine rather than beside it, because every
L1 fact is a fact about some L0 thing:

- A **router** or a **subscription dispatch** realizes a boundary — the
  way a caller or a topic enters an operation — so it is an intermediate
  vertex *on that edge*: the caller/topic edge ends at the vertex and a
  short arm carries on into the operation (caller → [router] → op, topic
  → [dispatch] → op). The vertex is marked request (solid) or subscribe
  (dashed) and names the execution pool, its member concurrency, and the
  routing/affinity fact. The pool name is its own target: a pool is a
  shared population, and selecting one lights every vertex that names it
  — which is all "shared pool" means (§52), with no pool node to say it.
- A **storage layout** is a fact about an object, so the objects
  operations persist to are drawn as a downstream data tier, wired to
  the operations that touch them by always-visible access edges. Each
  access edge is selectable and carries the one fact that matters of it
  — whether the access keys to the object's partition (solid) or does
  not (dashed) — and its detail says why. Partitioned objects (a layout
  is declared) are solid and spined; unpartitioned ones are open and
  dashed. No prose labels the tier; the shapes carry it.
- A **topic** carries its transport facts (grouping, ordering) on its
  own node, since topic-scoped transport is a fact about the topic.

The switch is disabled for a model that declares no L1 facts, and with
L1 off none of the above appears — those are facts of the layer that
declares them. Selection follows what a thing is a fact *about*, and
nothing wider: a router or a subscription lights only its own path — the
caller edges, the vertex, the operation — not the operation's other
edges; a pool lights every path it runs, which is what a shared pool is;
an access edge lights just its operation and object. Selecting the
operation itself still lights its one-hop neighbourhood, its
realizations, and the objects it writes. Nothing is a parallel graph
joined by on-demand links: the realization sits on the paths and the
entities it is about.

**Operation view** (`#/op/<id>`). A page header (name, copyable id,
description, and a fact strip: service, transaction and
program-step counts, the state machines it drives, verdict tally),
then three sections as Kumo layer cards. **Requirements** is a table —
one row per declared requirement with its key, its semantics
(replay-consistent result, guaranteed completion) and, when a report
is loaded, the verdict over its obligations; **Inputs** is a table of
what starts an invocation, split by layer: the **L0 contract** (request
identity and result contract, or the messages a subscription consumes)
and the **L1 realization** (for a request, the router, its routing key,
member assignment and the target pool's member concurrency; for a
subscription, its delivery, routing, member assignment and the same pool
fact). A boundary with no declared realization says so, and says that
absence is the absence of a fact rather than a realization lacking these
properties. The two sit side by side when the pane is wide enough and
stack otherwise.
**Program** shows the operation's one program — there is exactly one,
so there are no tabs and the route carries no query: `#/op/<id>` lands
on it directly, and an old `?flow=` query is tolerated and ignored.
The program is a Kumo `Flow` diagram: steps in order with connectors
drawn between them, each card carrying its location as the checker
names it (`3`, `3.ok.1` — one-based, arm-qualified). Inline
transaction steps carry their stable id and expand in place into their
body (reads with their binds, writes, inserts, deletes, locks,
transitions with their intent bindings, transaction-output and
effect-intent binders), each with its selector, provenance, and a full
detail panel; execute-effect steps carry their inline contract and,
like execute-intent steps, are badged with the effect's kind
(publication, request, external) and, when they bind a result, with
the binding and its `Result<Ok, Err>` schemas; `match_result` and `branch` cards show their arms side
by side — `ok` / `err`, or `then` / `otherwise` with the condition
rendered as text — each arm a nested sequence of the same step cards;
`return` cards name the request input and the variant and provenance
of the payload they construct, and `complete` cards close a
subscription-driven path. Transition steps link into the owning state
machine. An obligation's evidence names paths by the arms they take
(`ok(result.x) › then(step 3)`), which is how a reader finds the
decision it points at. Selecting any row or card opens its detail
panel.

**State machine view** (`#/machine/<id>`). A page header (name,
copyable id, governed object and state field, initial state, counts,
verdict tally), the state graph as a section — legal states, initial
state, transitions with ⚡ badges counting transition-owned side
effects — and a transitions table: from/to sets, each side effect
structured as kind, effect, target topic or operation, and the
operations that execute it through an intent, plus every transaction
step that takes the transition. The selected transition is part of
the route (`#/machine/<id>?t=<transition id>`): selecting one in the
graph or the table navigates there, selecting a state drops it, and a
deep link or history navigation selects what the address bar names.

## Panels

**Detail panel.** Every model entity — service, operation, topic,
schema, data object, state machine, state, transition, input, inline
effect, intent binding, transaction-output binding, result binding,
inline transaction, transaction step, program step, requirement, graph
edge, or L1 declaration (execution pool, router, storage layout) —
opens a detail panel organized into collapsible, counted sections
(execution, inputs, inline effects, requirements, obligations, …) with
key/value grids, typed badges, and clickable ids that open the
referenced entity in place. The entities are resolved from the program,
which is the source of truth for every operation-owned execution
occurrence.

An L1 declaration's panel says what the fact does and, as importantly,
what it does not: a pool carries no cardinality, sharing one relates
execution populations and not routing domains, a partition key is
neither an object identity nor a routing key. A data object's panel
carries its storage layout (or says none is declared) and the operations
that access it; a **data-access** edge's panel names the operation, the
object, and whether that access keys to the partition, with the reason. Each L1 declaration also lists **the proofs resting on
it** — the obligations whose reasoning names it — so the verdicts a
change to the topology would put back in question can be read off the
declaration itself. Topics and inputs carry the same list, being where
L1 facts attach to L0 entities.

**Top bar.** Model name and revision, breadcrumbs for the current
page, the id filter on the system view, and — when a report is loaded
— an "Obligations" button carrying the report's tally that opens the
obligations panel. It names the model, not the tool: the document
title already reads `<model> · conseqa`, and a host embedding the
views has a name of its own in its chrome. Proof-status colouring is always on when a report
is loaded. The bar shares the pages' content column, so its edges
line up with the page headers beneath it. As the bar narrows it gives
width away in order of what it costs the reader: the breadcrumbs go
first (the page header below already says where you are), then the
model title truncates, then the filter narrows. The controls keep
their size throughout — a control pushed out of reach is worse than a
crowded one.

**Notes.** Model-wide warnings the checker raises that belong to no
single obligation — today, a subscription that admits duplicate
deliveries while its operation declares no idempotency requirement
keyed from it — appear at the top of the obligations panel.

**Obligations panel.** The checker's obligations, grouped by the
operation (or data model, machine, topic) they anchor to, with a
segmented status filter (all / unknown / proven / disproven), a
segmented layer filter (any / L0 / L1), a text filter, per-group status
counts, and cards that expand to the declared facts a proof relies on,
the checker's evidence, or a counterexample trace — each with a "focus
subject" action that navigates to the entity. Every id a verdict names
in its prose is a link to that declaration, which is what makes a
proof's provenance followable rather than merely stated. The layer
filter reads `scope` for a proven obligation and `remedy` for an
unproven one; an obligation the checker classifies neither way appears
only under "any layer". With a report loaded, vertices gain status rings and rollup
chips (worst status wins: disproven > unknown > proven), requirement
chips in the operation view are colored by their obligation status, and
transitions in the machine view inherit theirs.

## The obligation report

The report format is `conseqa::analyzer::report` (`ProverReport`,
`format: 4`): one obligation per declared requirement — serialization,
ordering, idempotency, result replay (the result half of an idempotency
requirement declaring `result: replay_consistent`), recoverability —
with status `proven`, `disproven`, or `unknown`. Format 2 replaced the
response-replay property with result replay, dropped object-history
obligations and the flow subject, and made proofs cite the program
paths and decisions they rest on; format 3 added proof `scope` and
rebuilt the serialization and ordering arguments on the L1 runtime
model; format 4 added `remedy` to unproven serialization and ordering
obligations. Unknown is epistemic: the checker could not establish the
property, typically because a required fact is `unspecified` or no V1
verifier attempts that family. It is never evidence of a violation.

Each obligation carries its `summary`, `subject`, `scope`, `remedy`,
`assumptions` (the declared facts a proof relies on — conditional, per
§25 of the semantics contract), `evidence` (the checker's obstacles),
and, for disproofs, a `counterexample` trace. `scope` is `l0_only` or
`runtime_dependent`, and the card shows it as a badge: a
runtime-dependent proof holds of the declared runtime topology and must
be re-examined when that topology changes. `remedy` is its dual on an
unproven obligation — `application` or `runtime`, the layer the facts it
is waiting on belong to — and shows as "needs L0 fact" or "needs L1
fact". It is a routing hint, not a promise that declaring one closes the
argument.

The front end refuses a report of any other format, and says so in the
top bar rather than in the console: a report that is counted in one
place and silently dropped everywhere else leaves proven obligations
reading as indeterminate, which is the one thing a proof view must never
do. `REPORT_FORMAT` in `viz/src/types/report.ts` mirrors
`conseqa::analyzer::report::FORMAT` and has to move with it.

```
conseqa model.yaml --report proof.json       # produce a report
conseqa-viz model.yaml --report proof.json   # overlay it
conseqa-viz model.yaml --verify              # produce and overlay in one step
conseqa-viz model.yaml --example-report      # scaffold, every status unknown
```

`tests/fixtures/flash_checkout.report.json` is generated by the checker
(see `tests/report.rs`), and records the fixture's genuine findings
(10 proven, 4 unknown): the non-deduplicated, read-dependent
`tx.reserve_inventory` is neither idempotent nor recoverable, and that
gap propagates to `create_order`'s idempotency through the
`OrderCreated` cascade; the external card charge is explicitly not
deduplicated; and `charge_payment` branches on the payment provider's
result — `ok` publishes the capture, `err` the decline with its reason
— which no declared fact makes replay-consistent, so a retry is not
established to take the same arm.

## Front end

The presentation layer is a React + TypeScript application in `viz/`,
built with Vite, styled with Tailwind CSS v4 and Cloudflare's
[Kumo](https://kumo-ui.com/) design system (dark and light modes via
`data-mode`). The graph views are SVG rendered by React; the layout
algorithms live in `viz/src/graph/layout*.ts`.

`App` takes the page data and, optionally, a colour mode:

```tsx
<App data={pageData} />                  // manages its own mode
<App data={pageData} theme={hostTheme} /> // follows the host's
```

Left alone — as in the page `conseqa-viz` writes, which has no host —
the app restores the stored choice, sets `data-mode` on the document,
and offers a toggle in the top bar. An application embedding the views
usually has a colour mode already, and a second toggle for one setting
is a control that can disagree with itself; passing `theme` hands
ownership over, and the app then follows the host, touches neither the
document nor storage, and shows no toggle.

```
cd viz
npm install
npm run data    # page data for the video-streaming example → public/conseqa.json
npm run dev     # Vite dev server on http://localhost:5173
npm run build   # typecheck + single-file bundle → dist/index.html
```

The production build is one `dist/index.html` with every script and
stylesheet inlined (`vite-plugin-singlefile`). `conseqa::viz::render`
embeds that file at compile time (`include_str!`) and injects the page
data — title, model, derived graph, report — as `window.CONSEQA`, so
`cargo` needs no Node toolchain. During development the app fetches
`public/conseqa.json` instead; regenerate it with `npm run data`, or
directly with `conseqa-viz <model> --verify --json --out <path>`.

Compile time is the catch: every binary that renders carries the bundle
it was built with, so a front-end change reaches a reader only after the
whole chain runs.

```
cd viz && npm run build     # → viz/dist/index.html (committed)
cargo build --release       # rebakes it into every binary that renders
```

`conseqa-viz` is one of those binaries; so is `conseqa-confluence`,
whose `export_spec` writes `spec.html` through the same renderer, and
`conseqa-harness`. **Rebuild and commit `viz/dist/index.html` after
changing the front end, and rebuild the binaries after that.** An MCP
server already running keeps the image it started with — a rebuilt
binary reaches it only on its next start.

## Layout of the implementation

```
src/bin/viz/
  main.rs      CLI: parse, validate, verify or load a report, render
  graph.rs     Model → system graph (vertices, edges, indexes);
               resolves intents, transition ownership, message
               selectors so the front end never re-implements them
  report.rs    re-export of conseqa::analyzer::report
  render.rs    embeds viz/dist/index.html and injects the page data

viz/
  src/types/   TypeScript mirrors of the model, graph, and report JSON
  src/lib/     id index, obligation index, routing, text helpers
  src/state/   app state (selection, detail target, filters, theme)
  src/graph/   SVG canvas (pan/zoom), layouts, the three views
  src/panels/  detail panel, obligations panel, shared Kumo parts
  src/chrome/  top bar
  dist/        committed single-file production bundle
```
