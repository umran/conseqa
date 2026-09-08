// Mirror of `conseqa::analyzer::report`: the obligation report.

import type { Id } from "./model";

export type Status = "proven" | "disproven" | "unknown";

export type Property =
  | { kind: "serialization" }
  | { kind: "ordering" }
  | { kind: "idempotency" }
  | { kind: "recoverability" }
  | { kind: "result_replay" }
  | { kind: "custom"; name: string };

export type Subject =
  | { kind: "operation"; operation: Id; requirement?: number }
  | { kind: "transaction"; operation: Id; transaction: Id }
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
 *  re-examined whenever the runtime realization changes. */
export type ProofScope = "l0_only" | "runtime_dependent";

export interface Obligation {
  id: string;
  property: Property;
  subject: Subject;
  status: Status;
  summary: string;
  /** Absent for an obligation that is not proven. */
  scope?: ProofScope;
  assumptions: string[];
  evidence: EvidenceItem[];
  counterexample?: { trace: TraceStep[] };
}

export interface ProverReport {
  format: number;
  model_revision: number | null;
  obligations: Obligation[];
  /** Model-wide warnings that belong to no single obligation. */
  notes?: EvidenceItem[];
}

export function propertyName(property: Property): string {
  return property.kind === "custom" ? property.name : property.kind;
}
