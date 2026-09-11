// TypeScript mirror of the serialized `conseqa::spec::Model`. Shapes
// follow the serde conventions in `src/spec/`: internally tagged enums
// carry `kind`, maps are plain objects keyed by id.

export type Id = string;
export type FieldPath = string[];

export interface Model {
  /** The DSL contract version the model is expressed in. */
  dsl: number;
  revision: number;

  // L0 — the abstract application machine.
  services: Record<Id, Service>;
  schemas: Record<Id, Schema>;
  data_models: Record<Id, DataModel>;
  topics: Record<Id, Topic>;
  state_machines: Record<Id, StateMachine>;
  operations: Record<Id, Operation>;

  // L1 — one runtime realization of it. Absent means no runtime facts
  // are declared, never that the realization lacks these properties.
  runtime?: RuntimeModel | null;
}

export interface Service {
  kind: "backend" | "frontend" | "worker" | "job";
}

export type ScalarType =
  | "string"
  | "bool"
  | "int"
  | "float"
  | "decimal"
  | "uuid"
  | "timestamp";

export type TypeRef =
  | { kind: "scalar"; value: ScalarType }
  | { kind: "schema"; value: Id }
  | { kind: "list"; value: TypeRef };

export interface Field {
  ty: TypeRef;
  optional: boolean;
}

export type Schema =
  | {
      kind: "canonical";
      description: string | null;
      completeness: "partial" | "complete";
      fields: Record<string, Field>;
    }
  | { kind: "fragment"; source: Id; mapping: Record<string, FieldPath> };

export interface DataModel {
  objects: Record<Id, DataObject>;
  /** Typed transactional message collections of this data model:
   *  written only by a transaction's `write_outbox` step, atomically
   *  with its commit, and consumed by outbox inputs. */
  outboxes?: Record<Id, Outbox>;
}

/** A typed logical message collection owned by a data model — not a
 *  topic: its producer is transaction-exclusive and its admission is
 *  atomic with the containing commit. */
export interface Outbox {
  messages: Id[];
  message_identity: MessageIdentity;
}

export interface DataObject {
  schema: Id;
  identity: FieldPath[];
}

export type MessageIdentity =
  | { kind: "unspecified" }
  | { kind: "keyed"; mapping: Record<Id, FieldPath[]> };

/** A logical message channel. Transport ordering is a realization
 *  fact and lives in `RuntimeModel.topics`. */
export interface Topic {
  messages: Id[];
  message_identity: MessageIdentity;
}

export interface StateMachine {
  subject: { kind: "object"; object: Id; state: FieldPath };
  states: Id[];
  initial: Id;
  transitions: Record<Id, Transition>;
}

export interface Transition {
  from: Id[];
  to: Id;
  side_effects: Record<Id, TransitionSideEffect>;
}

export type TransitionSideEffect =
  | ({ kind: "publication" } & PublicationEffect)
  | ({ kind: "request" } & RequestEffect);

export type ValueSourceKind =
  | "input"
  | "effect"
  | "transaction_output"
  | "state_machine_subject"
  | "transaction_read"
  | "effect_result_ok"
  | "effect_result_err";

export interface ValueSource {
  kind: ValueSourceKind;
  id: Id;
}

export interface ValueRef {
  source: ValueSource;
  path: FieldPath;
}

export type Derivation =
  | { kind: "unspecified" }
  | { kind: "deterministic"; from: ValueRef[] };

export interface IdempotencyKey {
  components: ValueRef[];
}

export interface IdempotencyKeyPropagation {
  source: IdempotencyKey;
  target: IdempotencyKey;
}

/** A transaction's commit-deduplication guarantee — transaction-only.
 *  External boundaries declare identity / idempotency / result_replay
 *  instead. */
export type IdempotencyGuarantee =
  | { kind: "unspecified" }
  | { kind: "not_deduplicated" }
  | { kind: "deduplicated_by"; key: IdempotencyKey };

/** What identifies one logical external interaction. Deliberately its
 *  own vocabulary: an interaction identity is not an idempotency
 *  declaration, though the key shape coincides. */
export type ExternalIdentity =
  | { kind: "unspecified" }
  | { kind: "keyed"; key: ExternalIdentityKey };

export interface ExternalIdentityKey {
  components: ValueRef[];
}

/** Duplicate-side-effect behaviour, relative to the identity. */
export type ExternalIdempotency =
  | "unspecified"
  | "distinguishable"
  | "identical_per_identity"
  | "side_effect_free";

/** Terminal-result replay behaviour, relative to the identity. */
export type ExternalResultReplay = "unspecified" | "unstable" | "replay_stable";

/** Whether observing the contract's `Err` terminally resolves the
 *  logical interaction (`terminal`), conclusively ends one attempt
 *  while admitting another (`retryable`), or says nothing
 *  (`unspecified`). */
export type ErrorDisposition = "unspecified" | "terminal" | "retryable";

/** The `Err` half of a result contract: the payload schema and the
 *  declared disposition of observing that error. */
export interface ErrorResultType {
  schema: Id;
  disposition: ErrorDisposition;
}

/** A first-class `Result<Ok, Err>` contract: two schemas, exactly one
 *  of which shapes a given outcome. */
export interface ResultType {
  ok: Id;
  err: ErrorResultType;
}

export type ResultVariant = "ok" | "err";

export interface PublicationEffect {
  topic: Id;
  schema: Id;
  idempotency_key_propagation: IdempotencyKeyPropagation[];
}

/** Transactional admission of one message to a data-model outbox; its
 *  only legal execution site is a transaction's `write_outbox` step. */
export interface OutboxWriteEffect {
  outbox: Id;
  schema: Id;
  idempotency_key_propagation: IdempotencyKeyPropagation[];
}

export interface RequestEffect {
  target: { operation: Id; input: Id };
  schema: Id;
  retry: "unspecified" | "never" | "may_repeat";
  idempotency_key_propagation: IdempotencyKeyPropagation[];
}

export interface ExternalEffect {
  name: string;
  /** What makes two applications the same logical interaction. */
  identity: ExternalIdentity;
  /** Duplicate-side-effect behaviour, relative to that identity. */
  idempotency: ExternalIdempotency;
  /** Terminal-result replay behaviour, relative to that identity. */
  result_replay: ExternalResultReplay;
  /** The synchronous result the boundary returns; null when none is modeled. */
  result: ResultType | null;
}

export type Effect =
  | ({ kind: "publication" } & PublicationEffect)
  | ({ kind: "request" } & RequestEffect)
  | ({ kind: "external" } & ExternalEffect)
  | ({ kind: "outbox_write" } & OutboxWriteEffect);

export type RequestIdentity =
  | { kind: "unspecified" }
  | { kind: "keyed"; fields: FieldPath[] };

export type MessageSelector = { kind: "all" } | { kind: "only"; schemas: Id[] };

export type DeliverySemantics = "unspecified" | "at_most_once" | "at_least_once";

export type Input =
  | { kind: "request"; schema: Id; identity: RequestIdentity; result: ResultType }
  | {
      kind: "subscription";
      topic: Id;
      messages: MessageSelector;
      /** Subscription-only acknowledgement semantic: absent is no
       *  declared acknowledgement fact. */
      acknowledge_on_success?: boolean | null;
    }
  | {
      /** The outbox's one consuming boundary: exactly one outbox
       *  input in the model references a given outbox, and it admits
       *  every schema the outbox declares. Consumption is intrinsic —
       *  durable re-drive until successful completion, overlapping
       *  attempts admitted — so there is no selector and no
       *  acknowledgement field. */
      kind: "outbox";
      outbox: Id;
    };

export type Literal =
  | { kind: "string"; value: string }
  | { kind: "bool"; value: boolean }
  | { kind: "int"; value: number };

export type SelectorValue =
  | { kind: "value"; value: ValueRef }
  | { kind: "literal"; value: Literal };

export type SelectorPredicate =
  | { kind: "all" }
  | { kind: "eq"; field: FieldPath; value: SelectorValue }
  | { kind: "and"; predicates: SelectorPredicate[] };

export interface ObjectSelector {
  object: Id;
  predicate: SelectorPredicate;
}

export type FieldSelection = { kind: "all" } | { kind: "only"; fields: FieldPath[] };

export type LockOrder =
  | { kind: "unspecified" }
  | { kind: "by"; terms: { field: FieldPath; direction: "ascending" | "descending" }[] };

/** One transition side effect's application facts: the operation-local
 *  intent binding and the instance derivation. */
export interface TransitionEffectIntent {
  bind: Id;
  values: Derivation;
}

export type TransactionStep =
  | { kind: "read"; bind: Id; target: ObjectSelector; fields: FieldSelection }
  | { kind: "write"; target: ObjectSelector; fields: FieldPath[]; values: Derivation }
  | { kind: "insert"; object: Id; values: Derivation }
  | { kind: "delete"; target: ObjectSelector }
  | { kind: "lock"; target: ObjectSelector; mode: "shared" | "exclusive"; order: LockOrder }
  | {
      kind: "transition";
      machine: Id;
      transition: Id;
      subject: ObjectSelector;
      effect_intents: Record<Id, TransitionEffectIntent>;
    }
  | { kind: "establish_effect_intent"; bind: Id; effect_id: Id; effect: Effect; values: Derivation }
  | { kind: "establish_transaction_output"; bind: Id; schema: Id; values: Derivation }
  | { kind: "write_outbox"; effect_id: Id; effect: OutboxWriteEffect; values: Derivation };

/** An inline transaction: declared and executed at the program step
 *  that carries it. `id` is its stable logical identity. */
export interface Transaction {
  id: Id;
  data_model: Id | null;
  isolation: "unspecified" | "read_committed" | "snapshot" | "serializable";
  idempotency: IdempotencyGuarantee;
  steps: TransactionStep[];
}

/** The predicate of a branch: deterministic over the references it
 *  exposes, except `unspecified`. */
export type Condition =
  | { kind: "unspecified" }
  | { kind: "eq"; value: ValueRef; equals: SelectorValue }
  | { kind: "and"; conditions: Condition[] }
  | { kind: "not"; condition: Condition }
  | { kind: "present"; value: ValueRef };

export type ResultOutcome =
  | { kind: "ok"; values: Derivation }
  | { kind: "err"; values: Derivation };

export interface OperationBlock {
  steps: OperationStep[];
}

/** One joined handle and, when the underlying effect is result-bearing,
 *  the binding its result becomes available under after the barrier. */
export interface AsyncJoin {
  handle: Id;
  bind: Id | null;
}

export type OperationStep =
  | ({ kind: "transaction" } & Transaction)
  | { kind: "execute_effect"; effect_id: Id; effect: Effect; values: Derivation; bind: Id | null }
  /** Constructs and initiates the same instance an `execute_effect`
   *  would, without waiting for completion; binds only the handle. */
  | { kind: "execute_effect_async"; handle: Id; effect_id: Id; effect: Effect; values: Derivation }
  | { kind: "execute_effect_intent"; intent: Id; bind: Id | null }
  /** Initiates the captured intent's exact instance asynchronously. */
  | { kind: "execute_effect_intent_async"; intent: Id; handle: Id }
  /** All-completion barrier: continues after every referenced
   *  execution completes; entries may bind their effects' results. */
  | { kind: "join_all"; handles: AsyncJoin[] }
  /** First-completion barrier: continues after the first referenced
   *  execution completes — first completion, not first success; the
   *  losers are not cancelled. */
  | { kind: "race"; handles: Id[]; bind: Id | null }
  | { kind: "match_result"; result: Id; ok: OperationBlock; err: OperationBlock }
  | { kind: "branch"; condition: Condition; then: OperationBlock; otherwise: OperationBlock | null }
  | { kind: "return"; request: Id; outcome: ResultOutcome }
  | { kind: "complete" };

export type ResultReplayRequirement = "unspecified" | "replay_consistent";

export interface OperationRequirements {
  serialization: { key: ValueRef }[];
  ordering: { key: ValueRef }[];
  idempotency: { key: IdempotencyKey; result: ResultReplayRequirement }[];
  recoverability: { key: IdempotencyKey; completion: "resumable" | "guaranteed" }[];
}

export type RequirementKind = keyof OperationRequirements;

/** An operation: invocation sources, one causal program, requirements,
 *  and execution facts. Transactions, direct effects, transaction
 *  outputs, and effect intents are declared inline at the program or
 *  transaction site that executes or establishes them — the program is
 *  the source of truth for every operation-owned execution
 *  occurrence. */
export interface Operation {
  service: Id;
  description: string | null;
  inputs: Record<Id, Input>;
  /** Entry synchronization: an exclusive lock on the evaluated key,
   *  held from operation entry to the invocation's terminal. */
  invocation_lock?: InvocationLock | null;
  program: OperationBlock;
  requirements: OperationRequirements;
}

export interface InvocationLock {
  key: ValueRef;
}

// ---------------------------------------------------------------------
// L1 — runtime topology and realization semantics
// ---------------------------------------------------------------------

export interface RuntimeModel {
  topics?: Record<Id, TopicRuntime>;
  subscriptions?: Record<Id, Record<Id, SubscriptionRuntime>>;
  outboxes?: Record<Id, Record<Id, OutboxRuntime>>;
  execution_pools?: Record<Id, ExecutionPool>;
  routers?: Record<Id, Router>;
  storage_layouts?: Record<Id, StorageLayout>;
}

/** Transport facts for a topic, in topic-scoped mode. Declaring either
 *  puts the topic in that mode: every subscription observes these, and
 *  none may declare its own. */
export interface TopicRuntime {
  grouping?: GroupingKey;
  ordering?: OrderingSemantics;
}

/** Where a runtime grouping key lives in each grouped message schema.
 *  Its presence is the declaration — there is no "none" to spell — and
 *  it is independent of ordering, so a consumer needing only "same key,
 *  same group" never touches an ordering declaration. */
export type GroupingKey = Record<Id, FieldPath[]>;

/** The precedence a transport establishes, independent of grouping.
 *  Absent is "none". */
export type OrderingSemantics = "none" | "global" | "within_group";

export interface SubscriptionRuntime {
  delivery: DeliverySemantics;

  /** Present only in subscription-scoped mode — only when the
   *  subscribed topic declares neither fact. */
  grouping?: GroupingKey;
  ordering?: OrderingSemantics;

  dispatch: SubscriptionDispatch;
}

/** Absence of `routing` is not a routing mode: it is the absence of any
 *  member-affinity fact. */
export interface SubscriptionDispatch {
  pool: Id;
  routing?: SubscriptionRouting | null;
}

/** Runtime facts for the outbox's one consuming input: the outbox's
 *  one grouping concept (partitioning), its own ordering vocabulary,
 *  and dispatch. There is no delivery field — durable re-drive until
 *  successful consumption is intrinsic to the outbox. Absence of the
 *  whole declaration is epistemic. */
export interface OutboxRuntime {
  partitioning: OutboxPartitioning;
  ordering: OutboxOrdering;
  dispatch: OutboxDispatch;
}

export type OutboxPartitioning =
  | { kind: "none" }
  | { kind: "keyed"; mapping: Record<Id, FieldPath[]> };

export type OutboxOrdering = "none" | "global" | "partition";

export interface OutboxDispatch {
  pool: Id;
  /** Absence of `routing` is not a routing mode: it is the absence of
   *  any member-affinity fact. */
  routing?: OutboxRouting | null;
  /** An opaque batching stage over per-message logical invocations;
   *  absent means no batching fact is declared. */
  batching?: { ordering: "preserved" | "unspecified" } | null;
}

/** Mirrors SubscriptionRouting: which established semantic domain is
 *  routed, and how that domain is assigned to pool members. */
export interface OutboxRouting {
  key: OutboxRoutingKey;
  member_assignment: MemberAssignment;
}

/** partition_key routes by the domain OutboxRuntime.partitioning
 *  establishes, and requires keyed partitioning. */
export type OutboxRoutingKey = "partition_key";

export interface SubscriptionRouting {
  key: SubscriptionRoutingKey;
  member_assignment: MemberAssignment;
}

export type SubscriptionRoutingKey = "grouping_key";

export interface Router {
  boundary: { operation: Id; input: Id };
  pool: Id;
  routing?: RequestRouting | null;
}

export interface RequestRouting {
  key: FieldPath[];
  member_assignment: MemberAssignment;
}

export type MemberAssignment = { kind: "consistent_hash" } | { kind: "round_robin" };

/** A logical population of interchangeable runtime members. Carries no
 *  cardinality: member counts are external scenario inputs. */
export interface ExecutionPool {
  member_concurrency: MemberConcurrency;
  /** Continuity of exclusive execution authority across ownership and
   *  member transitions. Absent means no fact about such overlap. */
  execution_handoff?: ExecutionHandoff | null;
}

export type ExecutionHandoff = "exclusive_ownership";

export type MemberConcurrency =
  | { kind: "unspecified" }
  | { kind: "bounded"; value: number }
  | { kind: "unbounded" };

export interface StorageLayout {
  object: { data_model: Id; object: Id };
  partition_key: FieldPath[];
}
