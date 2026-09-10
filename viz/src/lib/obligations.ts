import type { Id } from "../types/model";
import { REPORT_FORMAT } from "../types/report";
import type { Obligation, ProverReport, Status, Subject } from "../types/report";

export const STATUS_ORDER: Record<Status, number> = { disproven: 0, unknown: 1, proven: 2 };
export const STATUS_GLYPH: Record<Status, string> = { proven: "✓", disproven: "✗", unknown: "?" };

/** Keys the graph uses to look obligations up by the entity they anchor to. */
export function subjectKeys(subject: Subject): string[] {
  switch (subject.kind) {
    case "operation":
      return [subject.operation];
    case "transaction":
      return [`${subject.operation}/${subject.transaction}`];
    case "object":
      return [`${subject.data_model}/${subject.object}`];
    case "state_machine":
      return [subject.transition ? `${subject.machine}/${subject.transition}` : subject.machine];
    case "topic":
      return [subject.topic];
  }
}

/**
 * Why a loaded report cannot be rendered, or null when it can.
 *
 * A report from another format carries assumptions written in a
 * vocabulary that may no longer describe the model, so it is refused
 * rather than rendered in terms the checker no longer uses. The refusal
 * is a fact about the page, not a console line: a refused report that
 * still counted its verdicts in the top bar while every ring, chip and
 * requirement row read indeterminate is precisely how this failed
 * before. `AppState` drops a refused report entirely and says so, so
 * every surface agrees.
 */
export function reportRejection(report: ProverReport | null): string | null {
  if (!report) return null;
  if (report.format !== REPORT_FORMAT) {
    return `report format ${report.format}, not ${REPORT_FORMAT}`;
  }
  return null;
}

export type ObligationIndex = Map<string, Obligation[]>;

export function buildObligationIndex(report: ProverReport | null): ObligationIndex {
  const index: ObligationIndex = new Map();
  if (!report || reportRejection(report)) return index;

  for (const ob of report.obligations) {
    for (const key of subjectKeys(ob.subject)) {
      const list = index.get(key);
      if (list) list.push(ob);
      else index.set(key, [ob]);
    }
  }
  return index;
}

export type StatusCounts = Partial<Record<Status, number>>;

export function statusCounts(obligations: Obligation[]): StatusCounts {
  const counts: StatusCounts = {};
  for (const ob of obligations) counts[ob.status] = (counts[ob.status] ?? 0) + 1;
  return counts;
}

export function worstStatus(obligations: Obligation[]): Status | null {
  let worst: Status | null = null;
  for (const ob of obligations) {
    if (worst === null || STATUS_ORDER[ob.status] < STATUS_ORDER[worst]) worst = ob.status;
  }
  return worst;
}

export function statusChipText(counts: StatusCounts): string {
  const parts: string[] = [];
  for (const status of ["disproven", "unknown", "proven"] as const) {
    const n = counts[status];
    if (n) parts.push(STATUS_GLYPH[status] + n);
  }
  return parts.join(" ");
}

export function subjectText(subject: Subject): string {
  switch (subject.kind) {
    case "operation":
      return subject.requirement !== undefined
        ? `${subject.operation} · requirement #${subject.requirement}`
        : subject.operation;
    case "transaction":
      return `${subject.operation} · ${subject.transaction}`;
    case "object":
      return `${subject.data_model} · ${subject.object}`;
    case "state_machine":
      return subject.transition ? `${subject.machine} · ${subject.transition}` : subject.machine;
    case "topic":
      return subject.topic;
  }
}

/** The operation an obligation belongs to, for grouping; objects group by data model. */
export function subjectGroup(subject: Subject): Id {
  switch (subject.kind) {
    case "operation":
    case "transaction":
      return subject.operation;
    case "object":
      return subject.data_model;
    case "state_machine":
      return subject.machine;
    case "topic":
      return subject.topic;
  }
}

/** Whether an obligation's property is the one a requirement chip refers to. */
export function propertyMatchesRequirement(property: Obligation["property"], kind: string): boolean {
  if (property.kind === kind) return true;
  // result_replay obligations anchor to the idempotency requirement.
  return kind === "idempotency" && property.kind === "result_replay";
}

/** Which semantic layer an obligation implicates. A proven obligation
 *  answers with the layers its proof consumed (`scope`); an unproven one
 *  with the layer the missing facts belong to (`remedy`). An obligation
 *  the checker classifies neither way answers `null`. */
export type Layer = "l0" | "runtime";

export function obligationLayer(ob: Obligation): Layer | null {
  if (ob.scope) return ob.scope === "runtime_dependent" ? "runtime" : "l0";
  if (ob.remedy) return ob.remedy === "runtime" ? "runtime" : "l0";
  return null;
}
