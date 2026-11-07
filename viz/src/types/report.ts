// Mirror of `conseqa::analyzer::report`: the obligation report.

import type { Id } from "./model";

export type Status = "proven" | "disproven" | "unknown";

/** The correctness property an obligation discharges. The two
 *  transaction properties mirror `TransactionRequirements`; idempotency
 *  and recoverability mirror `OperationRequirements`, and
 *  `result_replay` splits out the result half of an idempotency
 *  requirement. */
export type Property =
  | { kind: "transaction_serializability" }
  | { kind: "transaction_ordering" }
  | { kind: "idempotency" }
  | { kind: "recoverability" }
  | { kind: "result_replay" }
  | { kind: "custom"; name: string };

/** The model entity an obligation is anchored to. `requirement` indexes
 *  into the corresponding requirement list on the operation or the
 *  transaction, tying the obligation back to the declaration that
 *  produced it. */
export type Subject =
  | { kind: "operation"; operation: Id; requirement?: number }
  | { kind: "transaction"; operation: Id; transaction: Id; requirement?: number }
  | { kind: "object"; data_model: Id; object: Id }
  | { kind: "state_machine"; machine: Id; transition?: Id }
  | { kind: "topic"; topic: Id };

export interface EvidenceItem {
  subject?: Id;
  message: string;
}

export interface TraceStep {
  actor?: Id;
  description: string;
}

/** Which semantic layers a proof consumed. `runtime_dependent` means
 *  the argument rests on at least one declared L1 fact, so it must be
 *  re-examined whenever the runtime realization changes. Proofs of the
 *  transaction families are always `l0_only`: they rest on the
 *  transaction primitives and never on topology. */
export type ProofScope = "l0_only" | "runtime_dependent";

/** Which semantic layer holds the facts an unproven obligation waits on:
 *  the dual of `ProofScope`. `application` means an L0 declaration is
 *  missing and no runtime topology alone can discharge the obligation;
 *  `runtime` means every remaining obstacle names an L1 fact. A routing
 *  hint, not a verdict: it says where the next declaration goes, not
 *  that adding one there closes the proof. An unproven transaction
 *  obligation is always `application`. */
export type RemedyLayer = "application" | "runtime";

/** A value reference in canonical form. */
export interface ValueRefJson {
  source: { kind: string; id: Id };
  path: string[];
}

/** The obligation's identity across reports: subject, property, and
 *  requirement key, which reordering requirements leaves unchanged. */
export interface ObligationKey {
  subject:
    | { kind: "operation"; operation: Id }
    | { kind: "transaction"; operation: Id; transaction: Id };
  property:
    | "transaction_serializability"
    | "transaction_ordering"
    | "idempotency"
    | "recoverability"
    | "result_replay";
  requirement_key: ValueRefJson[];
  occurrence: number;
}

/** A member of the declaration partition, in its `kind` / `value` form. */
export interface DeclarationKey {
  kind: string;
  value: unknown;
}

/** One fact a proof consumed, with the declarations it was read from.
 *  `fact` is tagged by `kind`; its fields depend on the kind. */
export interface BasisFact {
  fact: { kind: string; [field: string]: unknown };
  anchors: DeclarationKey[];
  layer: "l0" | "l1";
}

/** What an unproven verdict is blocked on. */
export interface BlockedOn {
  present: BasisFact[];
  missing: BasisFact[];
  anchors: DeclarationKey[];
}

export interface Obligation {
  /** `oblig.<operation>.<transaction>.<family>.<index>` for the
   *  transaction families, `oblig.<operation>.<family>.<index>`
   *  otherwise. */
  id: string;
  /** The obligation's position-free identity. Nothing in the viz reads
   *  it yet. */
  key?: ObligationKey;
  property: Property;
  subject: Subject;
  status: Status;
  summary: string;
  /** Absent for an obligation that is not proven. */
  scope?: ProofScope;
  /** Absent for a proven obligation, and for families whose obstacles
   *  the checker does not yet classify. */
  remedy?: RemedyLayer;
  assumptions: string[];
  evidence: EvidenceItem[];
  /** The facts a proof consumed. Present only for a proven obligation. */
  basis?: BasisFact[];
  /** What an unproven verdict is blocked on. */
  blocked_on?: BlockedOn;
  counterexample?: { trace: TraceStep[] };
}

export interface ProverReport {
  format: number;
  /** The DSL contract version the verdicts are relative to. */
  dsl?: number | null;
  /** The fact vocabulary the bases are written in. */
  fact_format?: number;
  model_revision: number | null;
  obligations: Obligation[];
  /** Model-wide warnings that belong to no single obligation. */
  notes?: EvidenceItem[];
}

/** The report format this build understands — `conseqa::analyzer::report::FORMAT`.
 *  A report from another format carries assumptions in a vocabulary that
 *  may no longer correspond to the model, so it is refused rather than
 *  rendered. The refusal is shown, never silent: a rendered verdict the
 *  reader cannot see is indistinguishable from no verdict at all. */
export const REPORT_FORMAT = 10;

export function propertyName(property: Property): string {
  return property.kind === "custom" ? property.name : property.kind;
}
