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
their operations inside; topics, external systems, and a synthetic
"clients" vertex (for request inputs no modeled operation invokes) sit
around them. Edges are the model's information routes:

- publication effects: operation → topic
- subscription inputs: topic → operation
- request effects: operation → operation
- external effects: operation → external system
- client requests: clients → operation, for request inputs no modeled
  operation invokes

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

**Operation view** (`#/op/<id>`). A page header (name, copyable id,
description, and a fact strip: service, transaction and
program-step counts, the state machines it drives, verdict tally),
then three sections as Kumo layer cards. **Requirements** is a table —
one row per declared requirement with its key, its semantics
(replay-consistent result, guaranteed completion) and, when a report
is loaded, the verdict over its obligations; **Inputs** is a table of
what starts an invocation (kind, source schema or topic, request
identity and result contract, and — for a subscription with a declared
runtime — its delivery, routing, member assignment and the target
pool's member concurrency). The two sit side by side when the pane is
wide enough and stack otherwise.
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
inline transaction, transaction step, program step, requirement, or
graph edge — opens a detail panel organized into collapsible, counted
sections (execution, inputs, inline effects, requirements,
obligations, …) with key/value grids, typed badges, and clickable ids
that open the referenced entity in place. The entities are resolved
from the program, which is the source of truth for every
operation-owned execution occurrence.

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
segmented status filter (all / unknown / proven / disproven), a text
filter, per-group status counts, and cards that expand to the declared
facts a proof relies on, the checker's evidence, or a counterexample
trace — each with a "focus subject" action that navigates to the
entity. With a report loaded, vertices gain status rings and rollup
chips (worst status wins: disproven > unknown > proven), requirement
chips in the operation view are colored by their obligation status, and
transitions in the machine view inherit theirs.

## The obligation report

The report format is `conseqa::analyzer::report` (`ProverReport`,
`format: 3`): one obligation per declared requirement — serialization,
ordering, idempotency, result replay (the result half of an idempotency
requirement declaring `result: replay_consistent`), recoverability —
with status `proven`, `disproven`, or `unknown`. Format 2 replaced the
response-replay property with result replay, dropped object-history
obligations and the flow subject, and made proofs cite the program
paths and decisions they rest on; format 3 added proof `scope` and
rebuilt the serialization and ordering arguments on the L1 runtime
model. Unknown is epistemic: the checker could not establish the
property, typically because a required fact is `unspecified` or no V1
verifier attempts that family. It is never evidence of a violation.

Each obligation carries its `summary`, `subject`, `scope`,
`assumptions` (the declared facts a proof relies on — conditional, per
§25 of the semantics contract), `evidence` (the checker's obstacles),
and, for disproofs, a `counterexample` trace. `scope` is `l0_only` or
`runtime_dependent`, and the card shows it as a badge: a
runtime-dependent proof holds of the declared runtime topology and must
be re-examined when that topology changes.

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
stylesheet inlined (`vite-plugin-singlefile`). `conseqa-viz` embeds
that file at compile time (`include_str!`) and injects the page data —
title, model, derived graph, report — as `window.CONSEQA`, so `cargo`
needs no Node toolchain. **Rebuild and commit `viz/dist/index.html`
after changing the front end.** During development the app fetches
`public/conseqa.json` instead; regenerate it with `npm run data`, or
directly with `conseqa-viz <model> --verify --json --out <path>`.

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
