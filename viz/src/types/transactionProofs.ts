// Mirror of `src/viz/transaction_proofs.rs`: the transaction proofs —
// the serializability and ordering arguments — in the shape a drawing
// is made from.
//
// A serializability verdict is an argument over a graph — the
// requiring transaction's conflict closure, the potential serialization
// dependencies among its members, and the declared fact that
// commit-orders each of them, or the cycle no fact orders. An ordering
// verdict is that graph plus one guard step that makes commits follow
// the declared position. The report says whether the argument holds;
// this says what the argument is.

import type { Id } from "./model";

export interface TransactionProofs {
  serializability: SerializabilityView[];
  ordering: OrderingView[];
}

/** `serializable_isolation`: every transaction of the closure declares
 *  serializable isolation, so the database orders the closure itself.
 *  `conflict_graph`: each dependency that could close a cycle is
 *  commit-ordered by a declared fact. */
export type SerializabilityRoute = "serializable_isolation" | "conflict_graph";

export interface SerializabilityView {
  /** The report obligation this argument belongs to. */
  obligation: string;
  operation: Id;
  transaction: Id;
  requirement: number;
  /** The key, as `input.x.field`. */
  key: string;
  proven: boolean;
  route: SerializabilityRoute | null;
  /** One sentence on why the verdict is what it is. */
  headline: string;
  /** The conflict closure, the requiring transaction first. */
  nodes: ClosureNode[];
  /** Every potential dependency among the closure's members. */
  edges: DependencyView[];
  /** The dependencies grouped by ordered transaction pair — one arrow
   *  of the drawing each, with the underlying dependencies behind it. */
  pairs: PairView[];
  /** Strongly connected components a non-serializable history can still
   *  run through, each a list of transaction ids. Empty when proven. */
  cycles: Id[][];
  /** The checker's obstacle sentences; empty when proven. */
  obstacles: string[];
}

export interface ClosureNode {
  operation: Id;
  transaction: Id;
  /** The transaction step's location in its operation's program. */
  location: string;
  isolation: string;
  serializable: boolean;
  /** The transaction the requirement is declared on. */
  root: boolean;
  /** A member of an unconstrained cycle. */
  in_cycle: boolean;
}

export type DependencyKind = "wr" | "rw" | "ww";

export interface DependencyView {
  id: string;
  /** Transaction ids; equal for a dependency between two concurrent
   *  executions of one transaction. */
  source: Id;
  target: Id;
  kind: DependencyKind;
  /** The kind, said: `write → read`, `read → write (anti-dependency)`,
   *  `write → write`. */
  kind_label: string;
  object: Id;
  /** The overlapping fields, `all fields`, or `unknown fields`. */
  fields: string;
  /** One-based steps of the two accesses, and their access modes. */
  source_step: number;
  target_step: number;
  source_mode: string;
  target_mode: string;
  /** `same instance`, `may overlap`, or `disjoint`. */
  overlap: string;
  constrained: boolean;
  /** `strict lock`, `version validation`, `ordered cursor`, `atomic
   *  write order`, or `committed read`; null when nothing constrains
   *  the edge. */
  evidence: string | null;
  /** Short labels for what is missing on an unconstrained edge. */
  gaps: string[];
  /** The evidence, or the gaps, as sentences. */
  explanation: string;
  /** A fence is recorded on the edge; never evidence on its own. */
  fence: boolean;
}

/** The dependencies from one transaction to another, or to a concurrent
 *  execution of itself, summarized for one arrow. */
export interface PairView {
  id: string;
  source: Id;
  target: Id;
  /** Two concurrent executions of one transaction. */
  self_loop: boolean;
  /** How many dependencies the arrow stands for, and how many of them
   *  no declared fact commit-orders. */
  dependencies: number;
  open: number;
  constrained: boolean;
  /** The distinct kinds present, in `wr`, `rw`, `ww` order. */
  kinds: DependencyKind[];
  objects: Id[];
  /** Distinct evidence labels across the constrained dependencies. */
  evidence: string[];
  /** Distinct gap labels across the open dependencies. */
  gaps: string[];
  /** The ids of the underlying dependencies, in edge order. */
  edge_ids: string[];
  /** One line for the arrow's label or tooltip. */
  summary: string;
}

export interface OrderingView {
  obligation: string;
  operation: Id;
  transaction: Id;
  requirement: number;
  key: string;
  position: string;
  proven: boolean;
  headline: string;
  /** The cursor or fence that carries the position, when the
   *  transaction has one — present even when the argument fails for
   *  another reason, so the drawing can show what is there. */
  mechanism: MechanismView | null;
  /** The serializability argument over the ordering key, which an
   *  ordered history presupposes. */
  serializability: SerializabilityView;
  obstacles: string[];
}

export interface MechanismView {
  kind: "cursor" | "fence";
  object: Id;
  field: string;
  /** `successor` or `monotonic_after` for a cursor; null for a fence. */
  rule: string | null;
  /** One-based step in the transaction. */
  step: number;
  /** The value the guard compares against the stored one, as text. */
  incoming: string;
  /** Whether that value is the requirement's position. */
  carries_position: boolean;
}
