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

export interface Obligation {
  /** `oblig.<operation>.<transaction>.<family>.<index>` for the
   *  transaction families, `oblig.<operation>.<family>.<index>`
   *  otherwise. */
  id: string;
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
  counterexample?: { trace: TraceStep[] };
}

export interface ProverReport {
  format: number;
  /** The DSL contract version the verdicts are relative to. */
  dsl?: number | null;
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
export const REPORT_FORMAT = 7;

export function propertyName(property: Property): string {
  return property.kind === "custom" ? property.name : property.kind;
}
