import { Badge } from "@cloudflare/kumo/components/badge";
import { Button } from "@cloudflare/kumo/components/button";
import { Collapsible } from "@cloudflare/kumo/components/collapsible";
import { Tooltip } from "@cloudflare/kumo/components/tooltip";
import { CaretRightIcon, CrosshairIcon } from "@phosphor-icons/react";
import { useState } from "react";

import { shortId } from "../lib/ids";
import { subjectText } from "../lib/obligations";
import { useApp } from "../state/AppState";
import { propertyName, type Obligation } from "../types/report";
import { CitedText, IdLink, StatusBadge } from "./parts";

/** The layer note a verdict carries.
 *
 *  A proven obligation records the layers its argument consumed: an
 *  L0-only proof survives any change of runtime realization, a
 *  runtime-dependent one holds of the declared topology and of no other.
 *  An unproven one records the dual — the layer the facts it is waiting
 *  on belong to — so a reader knows whether the next declaration is an
 *  application one or a topology one. Neither is an alarm; the status
 *  badge beside it carries that. */
function layerNote(ob: Obligation): { label: string; hint: string } | null {
  if (ob.scope) {
    return ob.scope === "runtime_dependent"
      ? {
          label: "runtime-dependent",
          hint:
            "The proof consumed at least one declared L1 fact. It holds of this runtime " +
            "realization and must be re-examined whenever the topology changes.",
        }
      : {
          label: "L0 only",
          hint:
            "The proof consumed no L1 fact, so it survives any change of runtime realization. " +
            "It still assumes the implementation conforms to the L0 facts it names.",
        };
  }
  if (ob.remedy) {
    return ob.remedy === "runtime"
      ? {
          label: "needs L1 fact",
          hint:
            "Every remaining obstacle names a runtime fact — grouping, ordering, routing, member " +
            "assignment, or pool concurrency. A routing hint, not a promise: declaring one is " +
            "where to go next, not proof that it closes the argument.",
        }
      : {
          label: "needs L0 fact",
          hint:
            "At least one obstacle names an application fact — the program, the interface, or the " +
            "requirement itself — so no runtime declaration alone can discharge this.",
        };
  }
  return null;
}

const STRIPE: Record<Obligation["status"], string> = {
  proven: "border-l-kumo-success",
  disproven: "border-l-kumo-danger",
  unknown: "border-l-kumo-warning",
};

export function ObligationCard({ ob, defaultOpen = false }: { ob: Obligation; defaultOpen?: boolean }) {
  const { focusSubject } = useApp();
  const [open, setOpen] = useState(defaultOpen);
  const hasDetail = ob.assumptions.length > 0 || ob.evidence.length > 0 || !!ob.counterexample;
  const layer = layerNote(ob);

  return (
    <Collapsible.Root open={open} onOpenChange={setOpen}>
      <div className={`rounded-md border border-kumo-hairline border-l-2 bg-kumo-elevated/40 ${STRIPE[ob.status]}`}>
        <Collapsible.Trigger className="flex w-full cursor-pointer flex-col gap-1.5 px-3 py-2 text-left">
          <div className="flex items-center justify-between gap-2">
            <span className="flex items-center gap-1.5">
              <CaretRightIcon size={12} className={`text-kumo-inactive transition-transform ${open ? "rotate-90" : ""}`} />
              <Badge variant="neutral">{propertyName(ob.property)}</Badge>
            </span>
            <span className="flex items-center gap-1.5">
              {layer && (
                <Tooltip
                  content={layer.hint}
                  render={
                    <span className="inline-flex">
                      <Badge variant={ob.scope === "l0_only" ? "neutral" : "info"}>{layer.label}</Badge>
                    </span>
                  }
                />
              )}
              <StatusBadge status={ob.status} />
            </span>
          </div>
          <div className="text-sm leading-snug text-kumo-default">{ob.summary}</div>
          <div className="font-mono text-[11px] text-kumo-inactive">{subjectText(ob.subject)}</div>
        </Collapsible.Trigger>
        <Collapsible.Panel>
          <div className="space-y-3 border-t border-kumo-hairline px-3 py-2.5">
            {ob.assumptions.length > 0 && (
              <div>
                <div className="mb-1 text-[11px] font-semibold uppercase tracking-wider text-kumo-subtle">
                  {ob.scope === "runtime_dependent"
                    ? "relies on declared facts, runtime topology included"
                    : "relies on declared facts"}
                </div>
                <ul className="list-disc space-y-1 pl-4 text-sm text-kumo-default">
                  {ob.assumptions.map((a, i) => (
                    <li key={i}>
                      <CitedText text={a} />
                    </li>
                  ))}
                </ul>
              </div>
            )}
            {ob.evidence.length > 0 && (
              <div>
                <div className="mb-1 text-[11px] font-semibold uppercase tracking-wider text-kumo-subtle">evidence</div>
                <ul className="space-y-1.5 text-sm text-kumo-default">
                  {ob.evidence.map((ev, i) => (
                    <li key={i} className="rounded bg-kumo-base px-2 py-1.5">
                      {ev.subject && (
                        <span className="mr-1">
                          <IdLink id={ev.subject} />
                          <span className="text-kumo-inactive"> — </span>
                        </span>
                      )}
                      <CitedText text={ev.message} />
                    </li>
                  ))}
                </ul>
              </div>
            )}
            {ob.counterexample && (
              <div>
                <div className="mb-1 text-[11px] font-semibold uppercase tracking-wider text-kumo-subtle">
                  counterexample trace
                </div>
                <ol className="list-decimal space-y-1 pl-5 text-sm">
                  {ob.counterexample.trace.map((step, i) => (
                    <li key={i}>
                      {step.actor && <span className="font-mono text-[11px] text-kumo-subtle">{shortId(step.actor)}: </span>}
                      <CitedText text={step.description} />
                    </li>
                  ))}
                </ol>
              </div>
            )}
            {ob.remedy && (
              <div className="rounded-md border border-kumo-hairline bg-kumo-base px-2 py-1.5 text-xs leading-relaxed text-kumo-subtle">
                {ob.remedy === "runtime"
                  ? "Every obstacle above names an L1 fact: the next declaration belongs to the runtime realization."
                  : "An obstacle above names an L0 fact: the next declaration belongs to the application model, and no runtime topology alone will do."}
              </div>
            )}
            {!hasDetail && <div className="text-sm text-kumo-inactive">no further detail recorded</div>}
            <Button variant="ghost" size="xs" icon={CrosshairIcon} onClick={() => focusSubject(ob)}>
              focus subject
            </Button>
          </div>
        </Collapsible.Panel>
      </div>
    </Collapsible.Root>
  );
}
