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

export type ObligationIndex = Map<string, Obligation[]>;

export function buildObligationIndex(report: ProverReport | null): ObligationIndex {
  const index: ObligationIndex = new Map();
  if (!report) return index;

  // A report from another format carries assumptions written in a
  // vocabulary that may no longer describe the model. Rendering it
  // would explain a verdict in terms the checker no longer uses.
  if (report.format !== REPORT_FORMAT) {
    console.warn(
      `report format ${report.format} is not ${REPORT_FORMAT}; obligations not rendered`,
    );

    return index;
  }

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
