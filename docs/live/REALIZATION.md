# Realization

Every Conseqa proof is a proof of the model, conditional on the code and the infrastructure doing what the model declares (§1.3 of the semantics contract). The model names no technology and may not (§10.9). Realization profiles connect a spec to the technologies that can realize it, beside the spec and outside the model:

1. a closed **capability vocabulary**, in the semantics contract's terms, and the **needs** each declaration derives from the spec;
2. **catalogue entries** stating, with citations, what one technology configured a given way provides, cannot provide, and requires;
3. a project's **stack profile**: the technologies it uses, at their versions;
4. **choice**: for each realizable declaration, the entry that realizes it, automatic when exactly one fits;
5. **realization requirements**: what a chosen entry demands of the declaration and of its neighbours, required of the code under conformance;
6. **checkers**: deterministic checks some entries configure for the evidence of their requirements;
7. the realization half of a **diff**, and gates for what the head's stack cannot realize.

A spec is then proven, realizable, and conformant separately, and each failure points somewhere different: the model, the stack, the code. Nothing here changes a verdict.

This document describes the implementation. The design record is `Conseqa_Realization_Profiles_Revision_Specification.md` (Part VI), now historical. Where they differ, the code and its tests win (see the README's precedence rule).

## Files

```text
.conseqa/realization/
  stack.yaml                        the stack profile; without it nothing here applies
  catalogue/<entry id>.yaml         the project's own catalogue entries
  choices/<kind>/<id>[/<id>].yaml   the architect's choice for one declaration
```

The store neither loads nor scans `realization/`, so the store format is unchanged. Every file records `format: 1` (`REALIZATION_FORMAT`, which counts the stack, choice, and entry formats and the capability vocabulary together; a DSL bump forces a bump). A file of another format, an unknown field, or an unexpected entry is refused by name, and a profile that does not load fails what reads it. Dotfiles are ignored. A choice file's path follows Part IV's record rule, and its `declaration` must be the one its path names.

```yaml
# .conseqa/realization/stack.yaml
format: 1
catalogue: conseqa-standard@1     # the standard catalogue judged against
technologies: [postgres@16, kafka@3.7, envoy@1.30]
```

Each technology is named once, as `name@version`. A version is dotted numbers compared part by part, missing parts reading as zero. A managed service without product versions is named at its API version, as `dynamodb@2012.08.10`. The `catalogue` line must name the standard catalogue this build ships, so adopting a new one is a reviewed change to the stack.

```yaml
# .conseqa/realization/choices/topic_runtime/topic.order_events.yaml
format: 1
declaration: topic_runtime topic.order_events
entry: kafka.keyed-topic@1
options: [single_partition]       # each once, written sorted; omitted when none
reason: One order across every event.   # optional
```

## The capability vocabulary

`src/realization/capability.rs`. A capability is an `axis: value` pair whose value means what the semantics contract says, nothing a vendor calls by the same name: `isolation: serializable` is serializability of committed histories, which Oracle's `SERIALIZABLE` is not.

| Axis | Values |
|---|---|
| `durability` | `committed` |
| `isolation` | `read_committed`, `snapshot`, `serializable` |
| `transaction` | `interactive`, `multi_instance` |
| `lock` | `shared`, `exclusive` |
| `mutation` | `conditional`, `upsert` |
| `uniqueness` | `identity` |
| `outbox` | `atomic` |
| `channel` | `fanout` |
| `redrive` | `durable` |
| `grouping` | `keyed` |
| `ordering` | `within_group`, `global` |
| `delivery` | `at_least_once`, `at_most_once` |
| `assignment` | `consistent_hash`, `round_robin` |
| `concurrency` | `bounded` |
| `partitioning` | `keyed`, `composite` |

Coverage is exact: no value implies another, and an entry lists every value it provides.

## Needs

Needs are derived from the spec on every derivation, never declared. The realizable kinds are `data_model`, `outbox`, `topic`, `topic_runtime`, `subscription_runtime`, `outbox_runtime`, `execution_pool`, `router`, and `storage_layout`. A data object is realized by its data model's store.

| Declaration | Needs |
|---|---|
| `data_model` | `durability: committed`, and for each transaction declaring it: its isolation unless `unspecified`; `transaction: interactive` when a step's value, comparison, or selector uses a `transaction_read` or a `state_machine_subject`; `transaction: multi_instance` when it writes more than one instance; `lock: <mode>` per lock step; `mutation: conditional` per compare-and-set, transition, cursor advance, or fence; `mutation: upsert` and `uniqueness: identity` per upsert; `uniqueness: identity` per insert; `outbox: atomic` per outbox write, a transition's admissions included |
| `topic` | `channel: fanout` |
| `outbox` | `redrive: durable` |
| `topic_runtime` | `grouping: keyed` when it groups; `ordering: within_group` or `global` as declared |
| `subscription_runtime` | `delivery` unless `unspecified`; grouping and ordering declared on it (subscription-scoped); `assignment` when `dispatch.routing` is declared |
| `outbox_runtime` | `grouping: keyed` for keyed partitioning; `ordering: within_group` for `partition` and `global` for `global`; `assignment` when routed |
| `router` | `assignment` when `routing` is declared |
| `execution_pool` | `concurrency: bounded` for a bounded member concurrency |
| `storage_layout` | `partitioning: keyed` for a one-field key, `composite` for more |

Instances written are counted so: an instance a selector identifies by its object's whole identity counts once however often it is written; a selector pinning less counts as several; each insert and each outbox write counts as one, since an insert's derivation names no identity. Each need records its sources: the transaction and step (`step 2 (update object.order) uses transaction_read:read.order`) or the declaration and field. Needs derive from an assembled model or from a workspace whose operations need not all have programs yet, as a side of a diff may not.

## Catalogue entries

`src/realization/catalogue.rs`. A **platform entry** realizes one kind; an **idiom entry** shows how to write one primitive with one framework on some platforms, and provides nothing itself.

```yaml
format: 1
id: kafka.keyed-topic
version: 1
technology: { name: kafka, versions: ">=2.8" }
realizes: { kind: topic_runtime }
provides:
  - capability: { grouping: keyed }
    cite: [https://kafka.apache.org/44/getting-started/introduction#main-concepts-and-terminology]
  - capability: { ordering: global }
    with: { option: single_partition }
    cite: [ … ]
cannot_provide:
  - capability: { ordering: global }
    unless: { option: single_partition }
    reason: Kafka orders records within a partition only; …
    cite: [ … ]
options:
  single_partition: { description: …, excludes: [] }
requirements:
  - id: record-key
    of: producers                  # self, or a neighbour relation of the kind
    statement: Every record is produced with its key set to the message's grouping key, …
    cite: [ … ]
  - id: single-partition
    of: self
    with: { option: single_partition }   # the option's own requirement
    statement: The topic has exactly one partition.
evidence:
  record-key: { look_for: the producer call that sets the record key }
checkers:
  - id: single-partition
    kind: hcl
    covers: [single-partition]
    assert: { resource: kafka_topic, attribute: partitions, equals: 1 }
citations: [ … every URL a claim cites … ]
```

A provision may also be written bare, `- grouping: keyed`. An entry defines at most sixteen options. The reader is strict, and refuses by name: unknown fields; unknown axes or values; a kind that is not realizable; an entry neither platform nor idiom, or under another id's file name; an option referenced but not defined, excluding itself, or excluding an undefined one; a requirement whose `of` is no relation of the kind; duplicate requirement or checker ids; evidence or a checker's `covers` naming no requirement; an idiom's `platforms` naming no platform entry; a capability both provided and excluded, unless the exclusion is lifted by the option providing it; and a checker assertion outside its kind's shape.

**The standard catalogue**, `conseqa-standard@1`, is embedded in `conseqa` from `catalogue/` at the repository root: `postgres.database` (≥ 9.5), `postgres.partitioned-table` (≥ 10), `oracle.database` (≥ 19), `dynamodb.table`, `kafka.topic`, `kafka.keyed-topic`, `kafka.consumer-group`, `sqs.fifo`, `sqs.standard`, `envoy.ring-hash`, `envoy.round-robin`, and `debezium.outbox-relay`. Every provision, exclusion, and requirement of a standard entry cites the vendor documentation it rests on, each URL listed in `citations`, and `tests/realization.rs` holds them to it (`catalogue/README.md` states the rules for editing the catalogue). **The project catalogue** holds `catalogue/<id>.yaml` entries beside it; an id is unique across both, so a project that disagrees with a standard entry copies it under an id of its own. Citations are optional for project entries.

`kafka.consumer-group` claims `assignment: consistent_hash` as partition affinity within one rebalance generation: the semantics contract's consistent hash holds equal routing domains on one member during a stable ownership epoch, and a consumer group gives each partition to one member per generation. Its requirements state the conditions: consumers in one consumer group, never a share group, on the pool's members, and the topic's records keyed by the grouping key the dispatch routes on.

## Choice

`src/realization/choice.rs`. A declaration's **candidates** are the platform entries realizing its kind whose technology the stack names at a version in range. A candidate with a set of its options is **valid** when what it provides with them covers every need, and a **minimal** option set is one from which no option can be dropped while staying valid. Every set of a candidate's options is tried.

| State | When |
|---|---|
| `chosen` | A choice file names a valid entry and option set. |
| `invalid_choice` | A choice file names an entry that is unknown, an idiom, at another version than the catalogue's, for another kind, outside the stack, with unknown or mutually excluding options, or leaving needs uncovered. Each problem is named. |
| `unneeded` | No choice file, and nothing is needed: nothing is chosen, nothing fails, nothing is bound. |
| `automatic` | Exactly one valid candidate, with exactly one minimal option set. |
| `ambiguous` | Several valid candidates or option sets: every one is listed. |
| `infeasible` | No valid candidate: each candidate with the needs it provides with no option set, and its `cannot_provide` reasons. |

Automatic choice never takes an option the declaration does not need: a topic runtime needing only per-key order gets `kafka.keyed-topic@1` without `single_partition`. A choice file whose declaration the spec no longer has is listed as orphaned and fails nothing.

## Realization requirements

`src/realization/requirements.rs`. The requirements of a chosen or automatic entry, and of its chosen options, are in force for the declaration it realizes. Each applies to that declaration (`self`) or to every declaration in one neighbour relation of it:

| Relation | Of | The declarations |
|---|---|---|
| `producers` | topic, topic runtime | the effect sites, and transitions, publishing to the topic (the graph's `Publishers`) |
| `producers` | outbox, outbox runtime | the effect sites and transitions writing the outbox; an outbox runtime's outbox is the one its input consumes |
| `consumers` | topic, topic runtime | the topic's subscription `input` declarations |
| `callers` | router | the request effect sites targeting the router's boundary operation and input |
| `writers` | data model | the transactions mutating any of its objects |
| `transactions` | data model | the transactions declaring it |
| `serializable_transactions` | data model | those declaring `isolation: serializable` |
| `guarded_transactions` | data model | those with a compare-and-set, transition, cursor advance, or fence |

Each requirement on each declaration it applies to is one `realization` fact (`declaration`, `entry`, `realizes`, `requirement`; fact format 1). No basis holds one. Part IV requires it of that declaration's code, attested, reviewed (`review.realization: required` by default), and reported like any other required fact.

## Conformance

`src/conformance/` reads the profile on every derivation. With a stack profile:

- every `realization` fact is a required fact of the declaration it is on, relied on by no proof, so `missing_facts` and `surplus` follow the chosen realizations;
- every chosen or automatic declaration must be bound, whatever `require_binding` lists: this brings routers, pools, storage layouts, and topic runtimes under conformance;
- attest writes a realized declaration's record with `realization: { entry: <id@version>, options: [...] }`, and the record is `stale` under any other choice, a new entry version included, or once the declaration is no longer realized; the reader refuses the field on a kind that is not realizable;
- a declaration that is `ambiguous`, has an `invalid_choice`, or is `infeasible` has the condition `unrealizable` (last in the conditions' order), and its work item takes the priority `parked`, after every other: the remedy is a decision about the spec or the stack, not code;
- a work item's **Realization** section, before its precedents, names the entry realizing the declaration and why (automatic, or the architect's reason), each requirement on it with its statement, evidence shape, and checker findings, the requirements its realization places on its neighbours, and the idiom entries for the primitives it uses: an idiom whose technology the stack names, whose platforms include the entry realizing the declaration the primitive is contributed to, and whose primitive is a need it contributes;
- a realization fact's load-bearing line names what the entry provides for the spec's needs: "without it, `topic_runtime topic.order_events` is no longer realized as `kafka.keyed-topic@1` provides `grouping: keyed`, `ordering: within_group`, as the spec declares".

## Checkers

`src/realization/checkers/`. Conseqa ships five kinds; entries only configure them, so a catalogue never runs code of its own.

| Kind | Reads | `assert` |
|---|---|---|
| `hcl` | `.tf`, `.hcl` | `{ resource, present: true }`, or `{ resource, attribute, equals \| contains: <value> }` |
| `yaml_path` | `.yaml`, `.yml`, `.json` | `{ path, exists: true }`, `{ path, equals: <value> }`, or `{ path, one_of: [ … ] }` |
| `properties` | `.properties`, `.conf`, `.cfg` | `{ key, equals: <value> }` |
| `sql_ddl` | `.sql` | `{ table }` with `primary_key`, `unique`, or `partition_by` columns, or `trigger: true` |
| `pattern` | any file | `{ regex }` |

A checker reads only the regions bound to the declaration its requirement is on, each region's own text with nested regions left out. A `yaml_path` path is dotted keys matched at any depth, crossing lists, so a region cut from inside a document reads. `hcl` reads `resource` blocks and their top-level attributes, skipping nested blocks; `sql_ddl` reads `CREATE TABLE` (column and table constraints, `PARTITION BY`), `ALTER TABLE … ADD`, `CREATE UNIQUE INDEX`, and `CREATE TRIGGER`. A checker finds `confirmed` when every value found holds, `disputed` when the value is absent or one found fails, and `not_applicable` when no region bound to the declaration is a file of its kind.

Findings are reviews by `{ kind: checker, name: <entry id>/<checker id>@<entry version> }` at the digests of the regions read. They are derived on every derivation and never written into records, so they hold of the regions as they are and lapse with them. A declaration's status lists them as `checker_reviews`. A confirmation satisfies the required review of exactly the requirement its checker covers, for the fact kinds `checker_satisfies` lists (by default `realization`); a dispute raises `disputed`, whoever else confirms. A review written into a record as a checker's satisfies nothing. The standard checkers: Envoy ring hash's `lb_policy` (one of `RING_HASH`, `MAGLEV`), and Kafka's `enable.idempotence=true` and `partitions = 1` for `single_partition`.

## Change analysis

When either side of a diff has a stack profile, `src/design/change/realization.rs` reads each side's realization files through the same file source as its spec, a git tree's included; a profile that does not load refuses the side, naming it. The diff document's `realization` section:

```json
"realization": {
  "stack": { "added": ["sqs@2012.11.05"], "removed": [] },
  "declarations": [
    { "declaration": "topic_runtime topic.order_events",
      "needs": { "added": [], "removed": [] },
      "base": { "state": "automatic", "entry": "kafka.keyed-topic@1", "options": [] },
      "head": { "state": "ambiguous" } } ],
  "unrealizable": [
    { "declaration": "topic_runtime topic.order_events", "state": "ambiguous",
      "needs": [{ "grouping": "keyed" }, { "ordering": "within_group" }],
      "valid": [{ "entry": "kafka.keyed-topic@1", "options": [] }, { "entry": "sqs.fifo@1", "options": [] }] } ],
  "requirements": { "introduced": [ … ], "withdrawn": [ { "kind": "realization", … } ] }
}
```

`declarations` lists every realizable declaration whose needs, state, entry, or options changed; `unrealizable` every head declaration that is ambiguous (with `valid`), has an invalid choice (with `problems`), or is infeasible (with `gaps`); `requirements` the realization facts the head introduces and withdraws. The section does not wait on the obligation diff. `--fail-on infeasible` fails when a head declaration is infeasible or has an invalid choice, and `--fail-on ambiguous` when one is ambiguous; both read this section alone, so without a stack on either side neither fails.

## Surfaces

| Surface | What it does |
|---|---|
| `conseqa realize status [--require-realizable] [--json]` | Every realizable declaration's needs and state, and the orphaned choices. Without a stack profile it says so. `--require-realizable` exits 2 on an ambiguous, invalid, or infeasible declaration. |
| `conseqa realize explain <key> [--json]` | Every candidate: what it provides, the needs it misses and why, the option sets it is valid with; and the kind's entries outside the stack, with why. Exits 1 for a declaration that is not realizable, or without a stack. |
| `conseqa realize choose <key> <entry>[@<version>] [--option <name>]… [--reason <text>] [--json]` | Writes the choice file, atomically under `.conseqa/local/realization.lock`. An entry without a version takes the catalogue's. An identical choice writes nothing; another replaces it. Exits 2 when refused, naming what it lacks. |
| `conseqa realize catalogue [list [--json] \| show <entry> [--json]]` | The entries the project can choose from; runs outside a project on the standard catalogue alone. |
| `conseqa check --require-realizable` | Also exits 2 on an ambiguous, invalid, or infeasible declaration, and 1 when the profile does not load. |
| `conseqa diff --fail-on infeasible,ambiguous` | The gates above. |
| Design MCP `realization_status`, `explain_realization` | Every role and a review server's agents. A design task's worker reads the spec its task pinned; every other role the engine's head; a review server the project's files. |
| Design MCP `choose_realization` | The coordinator, through the server's write path. |

Exit codes otherwise follow the project CLI: 0 when the command ran, 1 when the spec, the stack, or the catalogue did not load.

Each `--json` prints one document, as the MCP tools return them: `{ "format": 1, "document": …, "dsl": 6, "realization_format": 1, … }`, with `format` a counter of its own.

| Document | Payload |
|---|---|
| `realization_status` | `profile` (`catalogue`, `technologies`, or `null` without a stack), `declarations` (each `declaration`, `needs` with their `sources`, and its `state` with the state's fields: `entry`, `options`, `reason`, `valid`, `problems`, `gaps`), `orphaned`. |
| `realization_explain` | `declaration`, `needs`, the state's fields, `candidates` (each `entry`, `source`, `provides`, `missing`, `option_sets`), `outside` (each `entry`, `source`, `technology`, `versions`, and `stack` when the stack names it at another version). |
| `realization_choice` | `outcome`: `written` or `unchanged` with `declaration`, `entry`, `options`, `reason`, `path`, `written`, `replaced`; or `refused` with `refusal` and its fields, and `message`. |
| `catalogue` | `catalogue` and `entries` (each `entry`, `source`, `technology`, `versions`, and `realizes` or `primitive`). |
| `catalogue_entry` | `source` and `entry`, in full. |

## Modules and tests

| Module | Holds |
|---|---|
| `src/realization/mod.rs` | The realization format, the file source the readers share, and the errors. |
| `capability.rs`, `catalogue.rs`, `stack.rs` | The vocabulary and needs; entries and the catalogues; the stack profile, versions, and ranges. |
| `choice.rs`, `requirements.rs`, `checkers/` | Candidates, states, explanations, and choices; requirements over the neighbour relations; the five checker kinds. |
| `document.rs` | The realization documents. |
| `src/design/change/realization.rs` | The diff's realization section. |
| `src/design/mcp/realization.rs` | The three tools. |
| `catalogue/` | The standard catalogue's entries. |

Tests: `tests/realization.rs` (needs against `tests/realization/needs.yaml`, choice, options, strictness, the standard catalogue's citations, documents, and the diff's realization section), `tests/realization_cli.rs` (`conseqa realize`, `check --require-realizable`, and the diff gates through the binary), `tests/realization_mcp.rs` (the tools and where each reads), `tests/conformance_work.rs` (requirements on their neighbours, binding, the record's `realization`, parked items, the Realization section, checker reviews), and the checkers' unit tests.
