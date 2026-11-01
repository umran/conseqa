# Conseqa Realization Profiles Revision Specification

## 1. Status

**Status:** Proposed  
**Part:** VI of the spec-versioning programme: realization profiles.  
**Depends on:**
- Part I (`Conseqa_Spec_Change_Analysis_Revision_Specification.md`): the declaration partition (§7), the textual declaration key and durable facts (§10.9), and the diff (§14).
- Part IV (`Conseqa_Conformance_and_Implementation_Guidance_Revision_Specification.md`): markers, records, required facts, work items, and reviews.

Both are amended by this revision (§17, §18).  
**Target DSL version:** 6 (unchanged)  
**Project store format:** 10 (unchanged). The realization files live in `.conseqa/` outside what the store loads and scans (§10.1).  
**Realization format:** 1 (new)  
**Fact format:** amended before format 1 ships, adding the `realization` kind (§12.2)  
**Baseline:** Conseqa `master`, commit `f184f34`  
**Amended in implementation (M8, 2026-10-31):** with the maintainer's approval:
- §6.2 and §6.3: coverage is exact, and which steps read, write several instances, and guard;
- §7.1, §7.2, §8.1–§8.3, and §9: entries carry `format`, realize one kind, cite each claim, give options their own requirements and exclusions; the standard catalogue's first entries; the stack pins the standard catalogue; versions and ranges;
- §10.1, §10.3, §10.4, §13.1, §14, and §15: a choice file's fields, the `unneeded` state, the infeasibility report, checker assertions' shapes, the realization documents, each subcommand's output and exit codes, and where the tools read;
- §20.7 and §21: the record-staleness test moves with the record's `realization` field; Kafka's consumer-group assignment is settled, and compatibility between neighbouring realizations is a new open question.

This revision connects a spec to the concrete technologies that can realize it. It introduces:

- the **capability vocabulary**: what each declaration needs from whatever realizes it, derived from the spec in Conseqa's own terms (§6);
- the **realization catalogue**: entries stating which technology, configured how, provides which capabilities, what it requires of neighbouring declarations, what evidence shows it, and which checkers can confirm it (§7, §8);
- the **stack profile**: the technologies one project uses (§9);
- **choosing a realization** per declaration: automatic when exactly one entry fits, recorded when the architect chooses, and **infeasible** when none does (§10);
- **realization requirements**: configuration a chosen entry demands of the declaration and of its neighbours, which become required facts under Part IV (§11, §12);
- **checkers**: deterministic checks a catalogue entry ships for its evidence (§13);
- `conseqa realize` subcommands, design MCP tools, and gates (§15).

The DSL is unchanged. No technology name, product setting, or number enters the spec.

---

# 2. Motivation

## 2.1 Nothing steers the choice of technology

Part IV tells an implementing agent what the code must provide and shows how this project already provides it (precedents, Part IV §12.3). On a codebase with no precedents, the agent chooses a load balancer, a broker, a datastore, or a framework idiom by taste. Many tasteful choices are wrong:

- `ordering: within_group` realized on an SQS standard queue, which orders nothing;
- `isolation: serializable` realized on Oracle's `SERIALIZABLE` level, which is snapshot isolation and admits write skew;
- `isolation: serializable` realized on PostgreSQL before 9.1, whose `SERIALIZABLE` was also snapshot isolation;
- a read-decide-write transaction realized on DynamoDB, which has no transaction spanning a read, a decision, and a write;
- consistent-hash routing realized on an Envoy cluster that hashes a header no caller sets.

Each of these is knowable once, written down, and reviewed. Rediscovering it in every agent session is where projects go wrong.

## 2.2 L1 gets almost no conformance pressure

Part IV requires only the facts that proofs rely on (Part IV §7.3). By design, almost no L1 fact is relied on: no L1 fact is serializability or ordering evidence (§10 of the semantics contract), and only `delivery` feeds a proof. Routing, member assignment, pool concurrency, storage layout, grouping and transport ordering exist for the external quantitative analysis (§10.9 of the semantics contract). That analysis assumes the realization matches the declaration just as much as a proof does. Part IV's default policy does not even require those declarations to be bound.

So today, a Kafka topic keyed by the wrong field, or an Envoy cluster that round-robins where the spec declares consistent hashing, passes every gate.

## 2.3 Infeasible specs are found too late

A spec can be valid and fully proven, and still be unbuildable on the project's stack: a design that needs interactive serializable transactions, on a team committed to DynamoDB. Today that surfaces only when an implementing agent gives up. The orchestrator design calls this `raise_objection` (`Conseqa_Delivery_Loop_Orchestrator_Design.md` §7.4). It should surface at the spec gate, before any agent starts.

---

# 3. Goals

## 3.1 Needs are derived, not authored

What a declaration needs from its realization is computed from the spec, in Conseqa's own vocabulary (§6). Nobody declares it twice.

## 3.2 Knowledge about technologies is written once

A catalogue entry states, once and with citations, what a technology provides, under which configuration, and what it cannot provide. Every project and every agent session reads the same entry.

## 3.3 Feasibility is checked at the spec gate

A spec that its project's stack cannot realize fails the gate, naming the declaration, the capability, and every candidate that was excluded and why.

## 3.4 L1 is held to what it declares

Declarations with a chosen realization must be bound, and the chosen entry's configuration becomes required facts. The infrastructure is then checked against the topology the spec declares, whether or not a proof relies on it.

## 3.5 Requirements cross declaration boundaries

A realization can require something of its neighbours: Kafka's per-key ordering requires every producer to set the record key, and Envoy's ring hash requires every caller to send the hashed header. Those requirements become required facts on the neighbours.

## 3.6 Mechanical checks where a shape is known

Where a catalogue entry knows the shape of its evidence (a Terraform attribute, an Envoy cluster field, a DDL clause), a deterministic checker confirms it, and the result is recorded as a review.

## 3.7 The DSL stays clean

Technologies, product settings, and numbers stay out of the model, as §10.9 of the semantics contract requires. Realization profiles sit beside the spec and never change a verdict.

---

# 4. Non-goals

This revision does not:

- add technology, product configuration, or numbers to the DSL;
- provision infrastructure, generate code, or inspect deployed resources;
- rank technologies by performance or cost: feasibility is a yes or no, and choosing among feasible entries is the architect's call;
- model caches or other non-durable storage (§21);
- vary the stack by environment (§21);
- change any verdict, the store format, or the DSL contract.

---

# 5. Terms

| Term | Meaning |
|---|---|
| **Realizable declaration** | A declaration of a kind that technologies realize (§6.1). |
| **Capability** | One thing a realization can provide, named in Conseqa's vocabulary, such as `isolation: serializable` or `ordering: within_group` (§6). |
| **Needs** | The capabilities a realizable declaration requires, derived from the spec (§6.3). |
| **Catalogue entry** | A versioned statement of what one technology, configured a given way, provides and requires (§7). |
| **Platform entry** | An entry that realizes a declaration: a datastore, a broker, a proxy, a relay. |
| **Idiom entry** | An entry that shows how to write one primitive with one framework on one platform (§7.4). |
| **Stack profile** | The technologies one project uses (§9). |
| **Candidate** | A platform entry for the declaration's kind, whose technology is in the stack. |
| **Valid** | A candidate whose provisions cover the declaration's needs (§10.2). |
| **Chosen realization** | The entry that realizes a declaration: chosen by the architect, or automatic when exactly one candidate is valid (§10.3). |
| **Realization requirement** | A configuration property the chosen entry demands, of the declaration or of a neighbour (§11). |
| **Checker** | A deterministic check an entry ships for the evidence of a realization requirement (§13). |

---

# 6. The capability vocabulary

## 6.1 Realizable kinds

| Layer | Kind | Typically realized by |
|---|---|---|
| L0 | `data_model` | a transactional datastore |
| L0 | `topic` | a broker's channel |
| L0 | `outbox` | the data model's store, and a relay that re-drives it |
| L1 | `topic_runtime`, `subscription_runtime`, `outbox_runtime` | broker and consumer configuration |
| L1 | `router` | a load balancer or proxy |
| L1 | `execution_pool` | a worker deployment's concurrency setting |
| L1 | `storage_layout` | a datastore's partitioning |

`data_object` declarations are realized by their data model's store. A data model is one transactional boundary (§5 of the semantics contract), so it has exactly one platform realization. Its objects cannot be spread across stores.

## 6.2 Capabilities

A capability is an `axis: value` pair. Every value means exactly what the semantics contract says. In particular, `isolation: serializable` means serializability of committed histories (§17 of the semantics contract), not whatever a vendor calls `SERIALIZABLE`. In the table below, every section number refers to the semantics contract (`docs/live/CONSEQA_DSL_SEMANTICS.md`).

| Axis | Values | Meaning |
|---|---|---|
| `durability` | `committed` | A committed transaction survives process and node failure. |
| `isolation` | `read_committed`, `snapshot`, `serializable` | The semantics contract's isolation levels (§17). |
| `transaction` | `interactive` | One transaction may read, decide, and then write, committing all of it atomically. |
| `transaction` | `multi_instance` | One atomic commit may mutate several identified instances, of one or several objects. |
| `lock` | `shared`, `exclusive` | Instance locks held to commit (§21). |
| `mutation` | `conditional` | An atomic conditional update of one identified instance: `compare_and_set`, or a guarded `transition`, `advance_cursor` or `fence` (§20). |
| `mutation` | `upsert` | An atomic identity-arbitrated insert-or-update (§20). |
| `uniqueness` | `identity` | Inserting an existing identity fails atomically. |
| `outbox` | `atomic` | Outbox admissions commit atomically with object mutations (§5). |
| `channel` | `fanout` | Every subscription receives every published message (§6). |
| `redrive` | `durable` | An admitted outbox message is re-driven until it is consumed (§8.3). |
| `grouping` | `keyed` | Messages with equal grouping keys share one runtime group (§10.2). |
| `ordering` | `within_group`, `global` | Transport precedence (§10.2). |
| `delivery` | `at_least_once`, `at_most_once` | Delivery semantics (§10.3.1). |
| `assignment` | `consistent_hash`, `round_robin` | Member assignment (§10.6). |
| `concurrency` | `bounded` | A per-member bound on active invocations is enforced (§10.5). |
| `partitioning` | `keyed`, `composite` | Instances are placed by a partition key of one field, or of several (§10.7). |

The vocabulary is closed and versioned by `REALIZATION_FORMAT` (§14). A `dsl` bump forces a `REALIZATION_FORMAT` bump, because capabilities are stated in the DSL's terms.

No value implies another. A candidate covers a need only by listing that very capability, so an entry that provides serializable isolation lists snapshot isolation too, if it provides it. Coverage is then a membership test a reviewer can check by reading the entry.

## 6.3 Deriving needs

Needs are computed from the spec, with no declaration of their own:

| Declaration | Needs |
|---|---|
| `data_model` | `durability: committed`. Then, for every transaction declaring this data model: <ul><li>its `isolation`, unless `unspecified`;</li><li>`transaction: interactive` if a step's value, comparison or selector uses a `transaction_read` or `state_machine_subject` reference: persistent state the transaction observed before it commits;</li><li>`transaction: multi_instance` if it writes more than one instance. An instance a selector identifies, pinning its object's whole identity, counts once however often it is written; a selector pinning less counts as several; each insert and each outbox write counts as one, since an insert's derivation names no identity;</li><li>`lock: <mode>` per lock step;</li><li>`mutation: conditional` per guarded mutation: `compare_and_set`, `transition`, `advance_cursor`, or `fence` (§20 of the semantics contract);</li><li>`mutation: upsert` per `upsert`;</li><li>`uniqueness: identity` per `insert` or `upsert`;</li><li>`outbox: atomic` per outbox write, a transition's admissions included.</li></ul> |
| `topic` | `channel: fanout` |
| `outbox` | `redrive: durable` |
| `topic_runtime` | `grouping: keyed` when `grouping` is declared, and `ordering: <value>` when it is `within_group` or `global` |
| `subscription_runtime` | `delivery: <value>` unless `unspecified`. `grouping` and `ordering` as for a topic runtime, when subscription-scoped. `assignment: <kind>` when `dispatch.routing` is declared. |
| `outbox_runtime` | `grouping: keyed` when `partitioning` is keyed; `ordering: within_group` when `ordering` is `partition`, since a partition is the runtime's grouping domain, and `ordering: global` when it is `global`; `assignment` when `dispatch.routing` is declared |
| `router` | `assignment: <kind>` when `routing` is declared; nothing otherwise |
| `execution_pool` | `concurrency: bounded` when `member_concurrency` is `bounded` |
| `storage_layout` | `partitioning: keyed` for a one-field key, `partitioning: composite` for more |

Needs are recomputed on every derivation, like everything else. When a spec change adds a need the chosen realization does not provide, the declaration becomes infeasible (§10.4) at the spec gate.

Each need records where the spec asks for it: the transaction and the step, or the declaration and the field it declares. The infeasibility report and `explain` name them.

---

# 7. Catalogue entries

## 7.1 Platform entries

```yaml
format: 1
id: kafka.keyed-topic
version: 1
technology: { name: kafka, versions: ">=2.8" }
realizes:
  kind: topic_runtime
provides:
  - grouping: keyed
  - ordering: within_group
  - capability: { ordering: global }
    with: { option: single_partition }
cannot_provide:
  - capability: { ordering: global }
    unless: { option: single_partition }
    reason: Kafka orders records within a partition only.
options:
  single_partition:
    description: The topic has exactly one partition. Provides global order at the cost of parallelism.
requirements:
  - id: record-key
    of: producers                  # neighbours: §11.2
    statement: Every record is produced with its key set to the message's grouping key, serialized the same way by every producer.
  - id: idempotent-producer
    of: producers
    statement: Producers set enable.idempotence=true with max.in.flight.requests.per.connection of at most 5, or disable idempotence and allow at most 1 in flight, so a retry cannot reorder records.
  - id: stable-partitions
    of: self
    statement: The partition count does not change while ordering is relied on. Repartitioning moves keys between partitions.
evidence:
  record-key: { look_for: the producer call that sets the record key }
  idempotent-producer: { look_for: producer configuration }
  stable-partitions: { look_for: the topic's infrastructure definition }
checkers:
  - id: terraform-topic
    kind: hcl
    covers: [stable-partitions]
    assert: { resource: kafka_topic, present: true }
  - id: producer-idempotence
    kind: properties
    covers: [idempotent-producer]
    assert: { key: enable.idempotence, equals: "true" }
citations:
  - https://kafka.apache.org/documentation/#producerconfigs_enable.idempotence
  - https://kafka.apache.org/documentation/#intro_concepts_and_terms
```

| Field | Meaning |
|---|---|
| `format` | The realization format (§14). |
| `id`, `version` | Identity. A record names the entry it was checked against as `id@version` (§17). An id is plain names joined by `.`; a version counts from 1. |
| `technology` | The product, and the versions the entry is true for (§9). A stack whose version is outside the range makes the entry no candidate. |
| `realizes.kind` | One realizable kind (§6.1). An entry realizes exactly one: a technology that realizes several kinds has an entry for each, since what it provides and requires differs by kind. |
| `provides` | Capabilities, each optionally conditional `with` an option. A provision is written bare, `- grouping: keyed`, or long, with `capability:` and then `with:` or `cite:`. |
| `cannot_provide` | Capabilities the technology does not provide, with the reason, optionally `unless` an option provides it. This turns "not listed" into an explanation in `realize explain` (§15). |
| `options` | Named configuration choices that extend `provides`, at most sixteen per entry. An option may list the options it `excludes`, and a set holding both is never valid. Choosing an option adds its requirements. |
| `requirements` | Realization requirements (§11), each `of: self` or of a neighbour relation (§11.2) the entry's kind has, stated in a sentence an agent and a reviewer can both check. A requirement marked `with` an option is that option's. |
| `evidence` | Where, in a region, the evidence for each requirement is found. |
| `checkers` | Deterministic checks of some requirements (§13). |
| `cite` | On any provision, exclusion or requirement: the vendor documentation that claim rests on. |
| `citations` | The vendor documentation the entry's claims rest on, together. |

## 7.2 More platform entries, in brief

| Entry | Realizes | Provides | Cannot provide | Requires |
|---|---|---|---|---|
| `postgres.database@1` (PostgreSQL ≥ 9.5) | `data_model` | `durability: committed`; `isolation`: `read_committed`, `snapshot` (`REPEATABLE READ`), `serializable` (`SERIALIZABLE`); `transaction`: `interactive`, `multi_instance`; `lock`: `shared` (`FOR SHARE`), `exclusive` (`FOR UPDATE`); `mutation`: `conditional`, `upsert` (`ON CONFLICT DO UPDATE`); `uniqueness: identity`; `outbox: atomic` | nothing on these axes | serializable transactions retried from their first statement on SQLSTATE `40001`; a guarded update is one `UPDATE` carrying every comparison, with a check that exactly one row changed; version publication by trigger (option `version_trigger`) or by every writer |
| `dynamodb.table@1` | `data_model` | `durability: committed`; `transaction: multi_instance` (`TransactWriteItems`); `mutation`: `conditional` (`ConditionExpression`), `upsert`; `uniqueness: identity` (`attribute_not_exists`); `outbox: atomic` | `transaction: interactive`, since no transaction spans a read, a decision and a write; `lock` | every decision a transaction makes is expressed as a condition on its writes |
| `oracle.database@1` | `data_model` | `isolation: read_committed`; `snapshot` (`SERIALIZABLE`); `transaction`: `interactive`, `multi_instance`; `lock: exclusive`; `mutation: conditional` | `isolation: serializable`: Oracle's `SERIALIZABLE` is snapshot isolation and admits write skew | none beyond the usual |
| `sqs.fifo@1` | `topic_runtime` | `grouping: keyed` (`MessageGroupId`); `ordering: within_group`; `delivery: at_least_once` | `ordering: global` | producers set `MessageGroupId` from the grouping key |
| `sqs.standard@1` | `topic_runtime` | `delivery: at_least_once` | `grouping`, `ordering` | a redrive policy, so that failed messages are retried |
| `kafka.topic@1` | `topic` | `channel: fanout` | none on this axis | each subscription reads through a consumer group of its own |
| `kafka.consumer-group@1` | `subscription_runtime` | `delivery: at_least_once` (option `commit_after_processing`), `at_most_once` (option `commit_before_processing`, which excludes the other); `assignment: consistent_hash`, as partition affinity within one rebalance generation (§21) | `assignment: round_robin`: a group assigns whole partitions, never rotating one's records across members | the consumers form one consumer group, never a share group, running on the pool's members; the topic's records are keyed by the grouping key the dispatch routes on; offsets are committed as the option says |
| `envoy.ring-hash@1` | `router` | `assignment: consistent_hash` (`RING_HASH` or `MAGLEV`) | `assignment: round_robin` (a separate entry, `envoy.round-robin@1`) | the cluster's `hash_policy` hashes a named header; every caller through the router's boundary sets that header from the routing key |
| `debezium.outbox-relay@1` | `outbox` | `redrive: durable` | none | the outbox table's connector is configured with the outbox event router |
| `postgres.partitioned-table@1` (PostgreSQL ≥ 10) | `storage_layout` | `partitioning`: `keyed`, `composite` (`PARTITION BY`) | none | the object's table is partitioned by exactly the layout's key |

The table is illustrative: the standard catalogue's entries are authored with their citations under §8.3. The PostgreSQL, Oracle and DynamoDB rows are the cases §2.1 needs, and the version bound on PostgreSQL is part of the entry, not a footnote: PostgreSQL's `SERIALIZABLE` is serializable from 9.1, and `ON CONFLICT DO UPDATE` arrived in 9.5.

The standard catalogue `conseqa-standard@1` ships `postgres.database`, `postgres.partitioned-table`, `oracle.database`, `dynamodb.table`, `kafka.topic`, `kafka.keyed-topic`, `kafka.consumer-group`, `sqs.fifo`, `sqs.standard`, `envoy.ring-hash`, `envoy.round-robin`, and `debezium.outbox-relay`, at version 1. Their checkers arrive with the checkers themselves.

## 7.3 Requirements of the declaration's own regions

`of: self` requirements apply to the realized declaration's own regions: the topic's Terraform, the cluster's Envoy configuration, the table's migration. Those regions are required bound (§18, item 4).

## 7.4 Idiom entries

An idiom entry shows how to write one primitive with one framework on one platform. It provides no capability, since the platform does that. It gives the agent a template, the evidence shape, and optionally a checker.

```yaml
id: laravel.compare-and-set
version: 1
technology: { name: laravel, versions: ">=10" }
platforms: [postgres.database, mysql.database]
primitive: { mutation: conditional }
template: |
  $affected = DB::table('orders')
      ->where('order_id', $orderId)
      ->where('version', $observedVersion)
      ->update(['status' => 'paid', 'version' => DB::raw('version + 1')]);
  if ($affected !== 1) {
      // the guarded mutation was rejected: take the transaction's rejected arm
  }
evidence:
  look_for: the update carrying every comparison, and the affected-rows check
```

| Primitive | Laravel | Express with Prisma | Rails | Spring |
|---|---|---|---|---|
| `mutation: conditional` on a version | guarded `update`, then check the affected rows (above) | `updateMany({ where: { id, version }, data: { …, version: { increment: 1 } } })`, then check `count` | `lock_version`; a stale update raises `StaleObjectError` | JPA `@Version` |
| `lock: exclusive` | `lockForUpdate()` | `$queryRaw` with `FOR UPDATE` | `lock` / `with_lock` | `@Lock(PESSIMISTIC_WRITE)` |
| `isolation: serializable` | `DB::transaction` at `SERIALIZABLE`, retried on `40001` | `$transaction(fn, { isolationLevel: 'Serializable' })`, retried on `P2034` | `transaction(isolation: :serializable)` | `@Transactional(isolation = SERIALIZABLE)` |
| request input | controller action on a route | route handler | controller action | `@RequestMapping` handler |

When a project's stack names a framework, the packet of every item using a primitive includes the matching idiom entry (§18, item 5).

---

# 8. Catalogues

## 8.1 Sources

| Source | Where | Versioned by |
|---|---|---|
| **The standard catalogue** | Shipped inside the `conseqa` binary, as the semantics guide is shipped inside the design MCP server. | `conseqa-standard@<n>`, bumped with every entry change. |
| **The project catalogue** | `.conseqa/realization/catalogue/<id>.yaml`, committed. Holds entries for the project's own technologies, and vendored third-party entries. | each entry's own `version` |

An entry id is unique across both sources, and a project entry with a standard entry's id is refused by name. A project that disagrees with a standard entry copies it under its own id and says why. It never silently overrides.

A stack names the standard catalogue it was judged against (§9), and a build shipping another refuses that stack by name. Adopting a new standard catalogue is then a reviewed change to the stack, like any other.

## 8.2 Strictness

The catalogue reader refuses unknown fields, unknown capability axes or values, a `kind` that is not realizable, an option referenced but not defined, and a requirement whose `of` names no neighbour relation (§11.2) of the entry's kind. It also refuses:

- an entry that is neither a platform entry (with `realizes`) nor an idiom entry (with `primitive`), and an entry whose id is not its file's name;
- an entry of more than sixteen options, an option excluding itself or an undefined one, and duplicate requirement or checker ids;
- evidence, or a checker's `covers`, naming no requirement of the entry;
- an idiom's `platforms` naming no platform entry;
- a capability both provided and excluded, unless the exclusion is lifted by the option that provides it;
- a checker assertion outside its kind's shape (§13.1).

It uses the same strictness rule as the store and the conformance reader.

## 8.3 Trust

A catalogue entry is trusted the way a proof's assumptions are: a wrong entry misleads every project that uses it. The standard catalogue's rules:

- every capability, every exclusion, and every requirement cites vendor documentation in its own `cite:`, each URL listed in the entry's `citations`, and a test holds the standard catalogue to it;
- every entry is reviewed by someone other than its author before release;
- an entry with a version-dependent claim states the version range, as PostgreSQL's does;
- where feasible, an entry ships a reproduction under `tests/realization/<id>/`: for example, a container test showing a Kafka topic with keyed records keeping per-key order across a producer retry.

Project entries are the project's responsibility and are reviewed in the pull requests that add them.

---

# 9. The stack profile

```yaml
# .conseqa/realization/stack.yaml
format: 1
catalogue: conseqa-standard@1
technologies:
  - postgres@16
  - kafka@3.7
  - envoy@1.30
  - debezium@2.6
  - terraform@1.9
  - laravel@11
```

The stack is a project-wide architectural decision, made by the architect and changed through the spec pull request like any other.

- Without a stack profile, nothing in this revision applies, and Parts I and IV behave as written.
- A technology must be named with a version, so that version-dependent entries can be judged (PostgreSQL before 9.1 is no candidate for `isolation: serializable`). A version is dotted numbers, compared part by part with missing parts read as zero, so `9.1` is `9.1.0`. An entry's range is one or more comparisons (`>=`, `>`, `<=`, `<`, `=`), comma-separated, all of which must hold: `>=9.1, <13`. A managed service without product versions is named at its API version, as `dynamodb@2012.08.10`.
- The stack names each technology once, and names the standard catalogue it was judged against (§8.1).
- A technology no entry describes is allowed: it simply provides nothing, until a project entry describes it.

---

# 10. Choosing a realization

## 10.1 Files

```text
.conseqa/
  realization/
    stack.yaml
    catalogue/<entry id>.yaml          project entries
    choices/<kind>/<id>[/<id>].yaml    one per declaration the architect chose for
```

The choice files use the same path rule as Part IV's records: the declaration's textual key, kind first, escaped ids as path segments. The store neither loads nor scans `realization/` (`src/design/store.rs` scans `project.yaml`, `spec/`, and `intent/`), so the store format is unchanged and older builds ignore it.

A choice file:

```yaml
format: 1
declaration: subscription_runtime operation.ship/input.ship.captured
entry: kafka.consumer-group@1
options: [commit_after_processing]
reason: Shipping must not drop captured payments under load.
```

The `declaration` is the one the file's path names. `options` lists each chosen option once, written sorted, and is omitted when none is chosen; `reason` is optional.

## 10.2 Candidates and validity

A realizable declaration's **candidates** are the platform entries that realize its kind, and whose technology the stack names at a version inside the entry's range. A candidate, with a set of options, is **valid** when its provisions under those options cover the declaration's needs.

## 10.3 The chosen realization

| Situation | Result |
|---|---|
| A choice file exists, and its entry and options are valid | that choice |
| A choice file exists, and its entry is no longer valid (needs grew, the stack changed, the entry changed) | **invalid choice**: infeasible until re-chosen |
| No choice file, and the declaration needs nothing | **unneeded**: nothing is chosen, nothing fails, and nothing is bound |
| No choice file, exactly one valid candidate with exactly one minimal option set | that candidate, **automatically** |
| No choice file, several valid candidates or option sets | **ambiguous**: a choice is needed |
| No valid candidate | **infeasible** |

A minimal option set is one from which no option can be dropped while staying valid. Automatic choice never picks an option the declaration does not need, so a Kafka topic that does not need global order is never made single-partition by default.

A declaration needs nothing when it declares nothing a technology must provide: a router without routing, a pool whose concurrency is unbounded or unspecified, a topic runtime declaring neither grouping nor ordering. It is unneeded whatever the stack offers. A choice file may still choose an entry for it, which is then judged like any other choice.

## 10.4 Infeasibility

An infeasible declaration is reported with:

- each candidate;
- the needs it does not cover;
- its `cannot_provide` reasons for those needs.

```text
infeasible     data_model data.orders
  needs transaction: interactive    transaction operation.pay/tx.apply_payment: step 2 (update object.order) uses `transaction_read:read.order`, read in the same transaction
  dynamodb.table@1  cannot provide transaction: interactive: A DynamoDB transaction is one TransactWriteItems call, [...] Express the decision as conditions on the writes.
```

A candidate that provides every need only with options that exclude one another says so. With no candidate at all, the report says that no entry in the stack realizes the kind.

The report is the architect's choice of remedies:

- change the design, for instance expressing the decision as a condition on the write;
- change the stack;
- or add a project entry for a technology that does provide it.

---

# 11. Realization requirements

## 11.1 What they are

A chosen entry's `requirements`, plus those of its chosen options, are **realization requirements**. Each one is a sentence an implementing agent can act on and a reviewer can check: "every record is produced with its key set to the grouping key".

## 11.2 Neighbour relations

A requirement applies `of: self`, or to the declarations in one neighbour relation of the realized declaration. The relations are graph queries the design layer already answers:

| Relation | Of a | Means | Graph query |
|---|---|---|---|
| `producers` | `topic`, `topic_runtime` | every effect site publishing to the topic | `Publishers` |
| `producers` | `outbox`, `outbox_runtime` | every effect site writing the outbox | the outbox's writers |
| `consumers` | `topic`, `topic_runtime` | every subscription input of the topic | `Consumers` |
| `callers` | `router` | every request effect site targeting the router's boundary operation | `Callers` |
| `writers` | `data_model`, `data_object` | every transaction mutating the object | `Writers` |
| `transactions` | `data_model` | every transaction declaring the data model | the data model's transactions |
| `serializable_transactions` | `data_model` | every transaction declaring `isolation: serializable` | the data model's transactions, filtered |
| `guarded_transactions` | `data_model` | every transaction with a conditional mutation | the data model's transactions, filtered |

So Kafka's `record-key` lands on every producing effect site. Envoy's header requirement lands on every caller. PostgreSQL's `40001` retry lands on every serializable transaction.

## 11.3 Requirements become facts

Each realization requirement, for each declaration it applies to, is a durable fact of the new kind `realization` (§12.2). Its implementer is that declaration. Part IV attests it, evidences it, reviews it, and reports it like any other required fact (§18).

---

# 12. Amendments to durable facts

## 12.1 Why a new kind

Realization requirements must be recorded, attested and diffed like facts, so they need the durable form Part I §10.9 gives facts. They are not proof facts. No basis contains one, and no verdict depends on one.

## 12.2 The `realization` kind

Added to Part I §10.9's catalogue of fields:

| Kind | Fields |
|---|---|
| `realization` | `declaration` (the textual key the requirement applies to), `entry` (`id@version` of the chosen entry), `realizes` (the textual key of the declaration that entry realizes), `requirement` (the requirement's id) |

Part I is Proposed and `FACT_FORMAT` 1 has not shipped, so the kind joins format 1 rather than bumping it.

---

# 13. Checkers

## 13.1 Kinds

Conseqa ships a closed set of checker kinds. Entries configure them; they never run arbitrary code.

| Kind | Reads | Asserts |
|---|---|---|
| `hcl` | Terraform files within bound regions | a resource of a type exists; an attribute equals, or contains, a value |
| `yaml_path` | YAML or JSON configuration within bound regions (Kubernetes, Envoy, compose files) | a path exists; its value equals, or is one of, values |
| `properties` | Java-style `.properties` and `key=value` client configuration | a key equals a value |
| `sql_ddl` | SQL migrations within bound regions | a table has a primary key or unique constraint over given columns; `PARTITION BY` over given columns; a trigger exists on a table |
| `pattern` | any bound region | a regular expression matches. The weakest kind, for idioms only. |

A checker reads only regions bound to the declaration its requirement applies to. It never searches the repository.

Each kind's `assert` has one shape, and asserts exactly one thing:

| Kind | `assert` |
|---|---|
| `hcl` | `{ resource, present: true }`, or `{ resource, attribute, equals: <value> }`, or `{ resource, attribute, contains: <value> }` |
| `yaml_path` | `{ path, exists: true }`, or `{ path, equals: <value> }`, or `{ path, one_of: [<value>, ...] }` |
| `properties` | `{ key, equals: <value> }` |
| `sql_ddl` | `{ table }` with one of `primary_key: [<column>, ...]`, `unique: [<column>, ...]`, `partition_by: [<column>, ...]`, or `trigger: true` |
| `pattern` | `{ regex }`, which must compile |

A value is a string, a boolean, or an integer, compared as it is written. An idiom entry's checker has no `covers`, since an idiom states no requirements: it checks the idiom's evidence shape.

## 13.2 Results are reviews

A checker run yields `confirmed`, `disputed`, or `not_applicable` for each requirement it covers. It is recorded as a Part IV review, `by: { kind: checker, name: <entry id>/<checker id>@<entry version> }`, at the digests it read. Its review lapses when those digests change, like any other.

## 13.3 When a checker suffices

Part IV's policy decides whether a confirming checker satisfies a required review, for the requirements it covers (§18, item 6). The default is yes for realization requirements. Proof facts cannot be listed at this revision, since no checker covers one. A checker can confirm that a Terraform topic exists. It cannot confirm that a transaction's conditional update guards what the proof needs.

---

# 14. Versioning

`REALIZATION_FORMAT` counts the formats of `stack.yaml`, choice files, catalogue entries, and the capability vocabulary together, starting at 1.

- A `dsl` bump forces a `REALIZATION_FORMAT` bump, never the reverse.
- Every realization file records its `format`. A reader refuses any other format by name.
- A record names the entry it was checked against as `id@version`. A new entry version makes every record naming the old one stale (§18, item 2), because the claims it was checked against may have changed. A choice naming the old version is likewise an invalid choice (§10.3).
- The realization documents (§15.1) count a `format` of their own, starting at 1, since a document and a profile file change for different reasons.

---

# 15. Surfaces

## 15.1 CLI

```text
conseqa realize status [--require-realizable] [--json]  per declaration: needs, chosen entry, state
conseqa realize explain <declaration key> [--json]      every candidate: provides, missing needs, exclusions
conseqa realize choose <declaration key> <entry>[@<version>] [--option <name>]… [--reason <text>] [--json]
conseqa realize catalogue [list [--json] | show <entry>[@<version>] [--json]]
```

- `status` reports every realizable declaration as `chosen`, `automatic`, `ambiguous`, `invalid_choice`, `infeasible` or `unneeded`, with its needs and where each comes from. It lists apart the orphaned choices, whose declaration the spec no longer has, and they fail nothing. Without a stack profile it says so, and `--require-realizable` passes, since nothing applies (§9).
- `explain` lists every candidate with what it provides, the needs it misses and why, and the option sets it is valid with. It also lists the kind's entries outside the stack, each with why: not in the stack, or the stack's version outside its range. That gives "change the stack" something to name.
- `choose` writes a choice file. An entry named without a version takes the catalogue's current one. It refuses by name an unknown declaration, a kind that is not realizable (a data object names its data model), an unknown entry or version, an entry realizing another kind, a technology outside the stack, an unknown option or two that exclude each other, and needs left uncovered. A choice replaces the declaration's last one, and an identical choice writes nothing.
- `catalogue` lists every entry the project can choose from, or shows one in full. It runs outside a project too, on the standard catalogue alone.

Exit codes follow the project CLI:
- **0:** the command ran;
- **1:** the spec, the stack, or the catalogue did not load; `explain` also exits 1 for a declaration that is not realizable or a project without a stack, and `catalogue show` for an entry no catalogue holds;
- **2:** `status --require-realizable` found an ambiguous, invalid or infeasible declaration, or `choose` was refused, as `conform attest` exits when refused.

`conseqa check` gains `--require-realizable`, with the same meaning, so a single CI step can demand both proof and feasibility.

Each subcommand's `--json` prints one document (T1 of the orchestrator design): `format`, a counter of its own (§14); `document`, one of `realization_status`, `realization_explain`, `realization_choice`, `catalogue`, and `catalogue_entry`; `dsl`; and `realization_format`; then the payload. The same inputs give a byte-identical document.

## 15.2 Design MCP tools

| Tool | Served to | Does |
|---|---|---|
| `realization_status` | every role, and a review server's agents | §15.1's `status` |
| `explain_realization` | every role, and a review server's agents | §15.1's `explain` |
| `choose_realization` | the coordinator | §15.1's `choose`, through the server's write path |

The tools read the spec where the caller reads it: a design task's worker at the head its task pinned, every other role at the engine's head, and a review server's agents from the project's files. Each returns §15.1's document, after a text rendering for the status and the explanation. `choose_realization` writes the choice file under a lock of its own, outside the spec's commit gate, as an attest writes a record.

Design agents can then keep a design feasible while designing it, which is the cheapest place to steer technology: a design agent asks `explain_realization` before it reaches for interactive transactions on a DynamoDB stack.

---

# 16. Layering

This revision keeps the semantics contract's boundary where it is. The contract's §10.9 says L1 is qualitative and everything countable stays outside it. Its "Beyond the pool" entry (§27) puts hosts, containers, database nodes and queue capacities outside L1 altogether. Realization profiles add nothing to the model:

| Layer | States | Checked by |
|---|---|---|
| **Model** (L0 and L1) | what the system is, and what it requires | validation and verification: *proven* |
| **Realization profile** | which technology provides what the model needs, configured how | feasibility at the spec gate: *realizable* |
| **Conformance records** | that the code and configuration were checked to do it | Part IV's derivation, checkers and reviews: *conformant* |

A proof stays a proof of the model, conditional on conformance (§1.3 of the semantics contract). Feasibility says the model can be built on this stack. Conformance says it was. Each level can fail independently, and each failure points somewhere different. Numbers stay in the infrastructure definitions and the simulation scenarios. An entry says "provides consistent-hash affinity on a header"; it never says "a ring of 1024 entries".

---

# 17. Amendments to Part I

Part I (`Conseqa_Spec_Change_Analysis_Revision_Specification.md`) changes as follows:

1. **§10.9** gains the `realization` fact kind (§12.2).
2. **§13.4 (new), realization changes.** When either side has a stack profile, the diff reads `.conseqa/realization/` on both sides. It reports:
   - stack changes;
   - per-declaration changes of chosen realization (entry, version, options);
   - changes of needs;
   - every declaration whose state became `ambiguous`, `invalid_choice` or `infeasible`.

   It also reports added and withdrawn realization requirements, as the realization part of the reliance delta.
3. **§14** gains a `realization` section in the diff document, carrying §13.4's report.
4. **§15.3** gains the gates `infeasible` (any declaration infeasible or with an invalid choice on head) and `ambiguous`.
5. **§19 and §20** gain the matching tests and acceptance criteria.

---

# 18. Amendments to Part IV

Part IV (`Conseqa_Conformance_and_Implementation_Guidance_Revision_Specification.md`) changes as follows:

1. **§7.1 and §7.2.** The `realization` kind is attestable, and its implementer is the declaration named in its `declaration` field.
2. **§7.3, required facts.** A declaration's required facts are the attestable facts proofs rely on, as before, plus every `realization` fact applying to it. A record's `realization` field (§8.2) names the chosen entry, and a different chosen entry, version or option set makes the record `stale`.
3. **§8.2, records** gain `realization: { entry: <id@version>, options: [...] }` on records of realized declarations.
4. **§14.4, policy.** When a stack profile exists, every declaration with a chosen realization must be bound, whatever `require_binding` lists. This is what brings routers, pools, storage layouts and topic runtimes under conformance.
5. **§12.1, work items** gain a **Realization** section, before Precedents, holding:
   - the chosen entry, and why it was chosen (automatic or the architect's reason);
   - each realization requirement on this declaration, with its statement and evidence shape;
   - the requirements this declaration imposes on its neighbours, so a topic's implementer knows producers must set keys;
   - the idiom entries matching the project's frameworks for the primitives the declaration uses.

   This answers T11 of the orchestrator design: guidance exists before any precedent does.
6. **§14.3, reviews** gain checker reviews (§13.2). **§14.4's policy** gains two keys:
   - `review.realization: required`, the default;
   - `review.checker_satisfies: [realization]`, the default, naming the fact kinds a confirming checker review satisfies.
7. **§10.1, conditions** gain `unrealizable`: the declaration is ambiguous, has an invalid choice, or is infeasible. Its work item is parked, since the remedy is a spec or stack decision, not code.
8. **§21** records deterministic checkers as partly resolved by §13.

---

# 19. Implementation changes by module

| Module | Changes |
|---|---|
| `src/realization/` (new, `design` feature) | `capability.rs` (the vocabulary and needs derivation, §6); `catalogue.rs` (entries, the embedded standard catalogue, the project catalogue, strictness, §7, §8); `stack.rs` (§9); `choice.rs` (candidates, validity, choice, infeasibility reports, §10); `document.rs` (the realization documents, §15.1); `requirements.rs` (realization requirements over neighbour relations, §11); `checkers/` (the five kinds, §13) |
| `src/analyzer/basis.rs` | the `realization` fact kind (§12) |
| `src/design/change/` (Part I) | the realization section of the diff, and the gates (§17) |
| `src/conformance/` (Part IV) | required realization facts, the record field, the binding rule, packet sections, checker reviews, the `unrealizable` condition (§18) |
| `src/design/mcp.rs` | the three tools (§15.2), in its `realization` child module |
| `src/main.rs` | `conseqa realize` and `check --require-realizable` (§15.1) |
| `catalogue/` (new, at the repository root) | the standard catalogue's entries, embedded at build time |
| `tests/realization/` | entry reproductions where feasible (§8.3), and the tests of §20 |

---

# 20. Test plan

## 20.1 Needs

For every fixture, derived needs match a hand-written expectation per realizable declaration. A spec change adding a lock step adds `lock: exclusive` to its data model's needs, and nothing else.

## 20.2 Choice

On a fixture with the standard catalogue:

- a stack of PostgreSQL and Kafka realizes every declaration automatically;
- adding SQS to the stack makes the topic runtimes ambiguous until chosen;
- a stack of DynamoDB makes a data model with an interactive transaction infeasible, with the report of §10.4;
- PostgreSQL 9.0 is no candidate for a data model needing `isolation: serializable`;
- Oracle is never a candidate for one.

## 20.3 Options

Automatic choice of `kafka.keyed-topic@1` never selects `single_partition` unless `ordering: global` is needed. Choosing it explicitly adds no requirement beyond its own.

## 20.4 Requirements and neighbours

Kafka's `record-key` applies to exactly the topic's publishing effect sites; Envoy's header requirement to exactly the router's callers; PostgreSQL's `40001` retry to exactly the serializable transactions. Each appears as a required `realization` fact in Part IV's derivation.

## 20.5 Conformance pressure on L1

With a stack profile, a router, a pool, and a storage layout with chosen realizations are required bound. A region round-robinning where `consistent_hash` is chosen fails its checker and stays `disputed`.

## 20.6 Checkers

Each checker kind confirms a matching region, disputes a mismatching one, and reports `not_applicable` outside its kind. No checker reads outside the regions bound to its declaration. Its review lapses on a digest change.

## 20.7 Strictness and versions

The following are each refused by name:

- unknown fields;
- an unknown capability;
- a duplicate id across catalogues;
- a choice file of another format;
- a stack technology without a version.

A new entry version makes the records naming the old one stale. That needs the record's `realization` field (§18, item 3), so the test arrives with it; until then, a choice naming an entry's old version is an invalid choice.

## 20.8 Gates

Part I's `infeasible` and `ambiguous` gates and `conseqa check --require-realizable` exit as §15.1 and §17 say.

---

# 21. Open questions

- **Caches and non-durable storage.** A data model is persistent by definition (`durability: committed` is always needed), so a cache cannot honestly realize one. A cache is also a source of stale reads, which the serializability analysis would need to see. Whether caching becomes an L0 concept, with read-staleness semantics the proofs can consume, is a DSL question for its own revision. Until then a cache is outside the model, and a region reading through one is a conformance reviewer's concern.
- **Kafka's consumer-group assignment. Settled.** `kafka.consumer-group@1` claims `assignment: consistent_hash` on the reading that partition affinity within one rebalance generation is the DSL's stable-epoch affinity. §10.6 of the semantics contract holds equal routing domains on one member during a stable ownership epoch, prescribing no hash function and saying nothing between epochs. A consumer group gives each partition to exactly one member per generation, and equal record keys share a partition while the partition count holds. The reading therefore holds while the topic's records are keyed by the grouping key the dispatch routes on, and while the consumers form a consumer group, never a share group, on the pool's members. The entry states both as requirements.
- **Compatibility between neighbouring realizations.** Each declaration is judged alone, so nothing stops a Kafka consumer group being chosen for a subscription whose topic runtime is an SQS queue. An entry cannot yet say what it requires of the entries realizing its neighbours.
- **Per-environment stacks.** Development on a local broker, production on Kafka. One stack per project keeps feasibility simple. Environments may later need stack overlays, each checked separately.
- **Third-party catalogues.** Vendoring into the project catalogue is enough to start. A registry with signed entries is a later question.
- **Capability granularity.** Isolation is stated at the semantics contract's three levels. Whether entries should also state anomaly-level detail is open: MySQL's `REPEATABLE READ` is not snapshot isolation in the textbook sense, for example.

---

# 22. Decisions taken, and alternatives

1. **Profiles live beside the spec, not in the DSL.** *Alternative:* a technology field on declarations. Rejected because it breaks §10.9's boundary, ties proofs to products, and forces a DSL bump for every new technology.
2. **Needs are derived.** *Alternative:* authored requirement lists per declaration. Rejected as a second, driftable statement of what the spec already says.
3. **Automatic choice when exactly one fits.** *Alternative:* always require a choice. Rejected as busywork on the common single-technology stack. The architect still chooses whenever there is a real choice.
4. **One choice file per declaration.** It matches Part IV's records: merge conflicts only where two branches chose for the same declaration.
5. **Requirements as `realization` facts under Part IV.** *Alternative:* a separate conformance mechanism for realizations. Rejected because it would duplicate markers, records, reviews and work items.
6. **A closed set of checker kinds.** *Alternative:* entries shipping scripts. Rejected because running catalogue-supplied code on every derivation is a supply-chain risk, and it makes checkers non-deterministic.

---

# 23. Documentation reconciliation

When this part is implemented:

- `docs/live/` gains `REALIZATION.md`, describing the capability vocabulary, needs derivation, the catalogue format, the stack profile, choice, requirements, checkers and the surfaces, and the live corpus table lists it;
- `PROJECT_STATE.md` adds `realization/` to the layout, and `conseqa realize` and `check --require-realizable` to its surfaces;
- `CONSEQA_DSL_SEMANTICS.md` §10.9 gains a sentence: realization profiles map L1 to technologies outside the model, and never inside it;
- this document becomes a historical design record.
