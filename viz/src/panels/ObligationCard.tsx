import { Badge } from "@cloudflare/kumo/components/badge";
import { Button } from "@cloudflare/kumo/components/button";
import { Collapsible } from "@cloudflare/kumo/components/collapsible";
import { CaretRightIcon, CrosshairIcon } from "@phosphor-icons/react";
import { useState } from "react";

import { shortId } from "../lib/ids";
import { subjectText } from "../lib/obligations";
import { useApp } from "../state/AppState";
import { propertyName, type Obligation, type ProofScope } from "../types/report";
import { IdLink, StatusBadge } from "./parts";

/** A runtime-dependent proof holds of the declared topology and of no
 *  other, so it is worth saying so on the card itself. */
const SCOPE_LABEL: Record<ProofScope, string> = {
  l0_only: "L0 only",
  runtime_dependent: "runtime-dependent",
};

const STRIPE: Record<Obligation["status"], string> = {
  proven: "border-l-kumo-success",
  disproven: "border-l-kumo-danger",
  unknown: "border-l-kumo-warning",
};

export function ObligationCard({ ob, defaultOpen = false }: { ob: Obligation; defaultOpen?: boolean }) {
  const { focusSubject } = useApp();
  const [open, setOpen] = useState(defaultOpen);
  const hasDetail = ob.assumptions.length > 0 || ob.evidence.length > 0 || !!ob.counterexample;

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
              {ob.scope && (
                <Badge variant={ob.scope === "runtime_dependent" ? "warning" : "neutral"}>
                  {SCOPE_LABEL[ob.scope]}
                </Badge>
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
                    <li key={i}>{a}</li>
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
                      {ev.message}
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
                      {step.description}
                    </li>
                  ))}
                </ol>
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
