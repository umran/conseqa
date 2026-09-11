// Mirror of `src/bin/viz/graph.rs`: the derived system graph.

import type { Id } from "./model";

export const CLIENT_NODE_ID = "@client";
export const EXTERNAL_PREFIX = "@external:";

export interface Graph {
  services: ServiceNode[];
  operations: OperationNode[];
  topics: TopicNode[];
  outboxes: OutboxNode[];
  externals: ExternalNode[];
  /** The declared L1 runtime topology; empty for an L0-only model. */
  runtime: RuntimeView;
  client: ClientNode | null;
  edges: Edge[];
  effect_owners: Record<Id, EffectOwner>;
  transition_refs: Record<string, TransitionRef[]>;
}

export interface ServiceNode {
  id: Id;
  kind: string;
  operations: Id[];
}

export interface RequirementBadges {
  serialization: number;
  ordering: number;
  idempotency: number;
  recoverability: number;
}

export interface OperationNode {
  id: Id;
  service: Id;
  description: string | null;
  inputs: number;
  /** Steps of the operation program, nested ones included. */
  steps: number;
  machines: Id[];
  requirements: RequirementBadges;
}

export interface RuntimeView {
  execution_pools: ExecutionPoolNode[];
  routers: RouterNode[];
  storage_layouts: StorageLayoutNode[];
}

export interface ExecutionPoolNode {
  id: Id;
  member_concurrency: string;
  /** Boundaries assigned to this pool — the shared execution
   *  population made visible. */
  assigned: { operation: Id; input: Id }[];
}

export interface RouterNode {
  id: Id;
  operation: Id;
  input: Id;
  pool: Id;
  /** The semantic routing key; empty when the router declares none. */
  routing_key: string[];
  member_assignment: string | null;
}

export interface StorageLayoutNode {
  id: Id;
  data_model: Id;
  object: Id;
  partition_key: string[];
}

export interface TopicNode {
  id: Id;

  /** Topic-scoped transport facts. In subscription-scoped mode both
   *  read "none" and each subscribe edge carries its own. */
  ordering: string;
  grouping: string;
  topic_scoped_transport: boolean;

  messages: Id[];
}

/** A data-model-owned outbox: a transactional message collection,
 *  rendered distinctly from a topic so the atomic producer boundary
 *  stays visible. */
export interface OutboxNode {
  id: Id;
  data_model: Id;
  /** "keyed" or "unspecified". */
  message_identity: string;
  messages: Id[];
}

export interface ExternalNode {
  id: string;
  name: string;
}

export interface ClientNode {
  id: string;
}

export interface TransitionKey {
  machine: Id;
  transition: Id;
}

interface EdgeBase {
  id: string;
  from: string;
  to: string;
}

export type Edge = EdgeBase &
  (
    | {
        kind: "publish";
        operation: Id;
        effect: Id;
        schema: Id;
        via_transition: TransitionKey | null;
        /** Program steps executing the effect, as step locations. */
        executed_at: string[];
        /** The subset of `executed_at` that launches the effect
         *  asynchronously: initiation without a completion dependency
         *  on the following step. */
        async_executed_at: string[];
      }
    | {
        kind: "subscribe";
        operation: Id;
        input: Id;
        schemas: Id[];
        delivery: string;
        /** The transport facts in force, resolved from whichever scope
         *  declares them. */
        grouping: string;
        ordering: string;
        /** The dispatch routing key, or "none" when the dispatch
         *  declares no member affinity; null when the subscription has
         *  no declared runtime at all. */
        routing: string | null;
        pool: Id | null;
        member_assignment: string | null;
      }
    | {
        kind: "request";
        operation: Id;
        effect: Id;
        input: Id;
        schema: Id;
        retry: string;
        via_transition: TransitionKey | null;
        executed_at: string[];
        /** The subset of `executed_at` launching asynchronously. */
        async_executed_at: string[];
      }
    | {
        kind: "external";
        operation: Id;
        effect: Id;
        identity: string;
        idempotency: string;
        result_replay: string;
        executed_at: string[];
        /** The subset of `executed_at` launching asynchronously. */
        async_executed_at: string[];
      }
    | { kind: "client"; operation: Id; input: Id; schema: Id }
    | {
        kind: "outbox_write";
        operation: Id;
        effect: Id;
        schema: Id;
        /** The inline transaction whose commit admits the message;
         *  null only for the structurally invalid direct-site shape. */
        transaction: Id | null;
        /** Program steps whose transaction stages the write. */
        executed_at: string[];
      }
    | {
        kind: "outbox_consume";
        operation: Id;
        input: Id;
        schemas: Id[];
        acknowledge_on_success: boolean;
        delivery: string;
        /** Declared runtime facts; null without an outbox runtime. */
        partitioning: string | null;
        ordering: string | null;
        pool: Id | null;
        member_assignment: string | null;
        /** "none" when a runtime declares no batching stage; null
         *  without a runtime at all. */
        batching: string | null;
      }
  );

export type EdgeKind = Edge["kind"];

export type EffectOwner =
  | { kind: "operation"; operation: Id }
  | { kind: "transition"; machine: Id; transition: Id };

export interface TransitionRef {
  operation: Id;
  transaction: Id;
  step: number;
}
