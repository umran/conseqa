import { Badge } from "@cloudflare/kumo/components/badge";
import { ClipboardText } from "@cloudflare/kumo/components/clipboard-text";
import { Empty } from "@cloudflare/kumo/components/empty";
import { Text } from "@cloudflare/kumo/components/text";
import { GraphIcon } from "@phosphor-icons/react";
import { useEffect, type ReactNode } from "react";

import { TxStepRow } from "../graph/OperationView";
import {
  commitGuarantee, isolation, orderingRequirement, serializabilityRequirement, transactionRejection,
} from "../lib/explain";
import { shortId } from "../lib/ids";
import { findTransactionSite } from "../lib/index";
import { hashes, type RequirementRef } from "../lib/route";
import { ObligationCard } from "../panels/ObligationCard";
import { Fact, FactBadge, IdLink, KeyComponents, Muted, NavLink, RefText, SectionCard, StatusChips } from "../panels/parts";
import { OrderingProof, SerializabilityProof } from "../panels/TransactionProof";
import { useApp, useObligationsAt } from "../state/AppState";

/**
 * A transaction's page: what it is, what it requires of its committed
 * history with each argument drawn in full — the conflict closure, the
 * dependencies behind every arrow, the guard that carries an ordering
 * position — its steps, where control goes when it rejects, and the
 * report's verdicts on it. The drawing needs this room; the inspector
 * and the obligation cards summarize the argument and lead here.
 */
export function TransactionPage({ id, req }: { id: string; req: RequirementRef | null }) {
  const { model, index, transactionProofs, selection } = useApp();
  const entry = index.get(id);
  const opId = entry?.kind === "transaction" ? entry.op : null;
  const op = opId ? model.operations[opId] : null;
  const site = op ? findTransactionSite(op, id) : null;
  const obKey = opId ? `${opId}/${id}` : id;
  const obligations = useObligationsAt(obKey);

  // A link into one requirement — from an obligation card, a
  // requirement row, a conflict arc — lands on that requirement's
  // argument, and a selection made from elsewhere is brought into view.
  useEffect(() => {
    if (!req) return;
    const block = document.querySelector<HTMLElement>(`[data-req="${CSS.escape(`${req.prop}:${req.index}`)}"]`);
    block?.scrollIntoView({ block: "start", behavior: "smooth" });
  }, [req, id]);
  useEffect(() => {
    if (!selection) return;
    const row = document.querySelector<HTMLElement>(`[data-selkey="${CSS.escape(selection)}"]`);
    row?.scrollIntoView({ block: "nearest", behavior: "smooth" });
  }, [selection]);

  if (!op || !opId || !site) {
    return (
      <div className="flex h-full items-center justify-center">
        <Empty size="sm" icon={<GraphIcon size={32} className="text-kumo-inactive" />} title={`unknown transaction ${id}`} />
      </div>
    );
  }

  const tx = site.transaction;
  const rejected = site.rejected;
  const requirementCount = tx.requirements.serializability.length + tx.requirements.ordering.length;
  const isReq = (prop: RequirementRef["prop"], i: number) => req !== null && req.prop === prop && req.index === i;

  return (
    <div className="@container h-full overflow-auto">
      <div className="mx-auto max-w-[1240px] space-y-6 p-6">
        <header className="space-y-4 border-b border-kumo-hairline pb-5">
          <div className="text-[11px] font-semibold uppercase tracking-wider text-kumo-subtle">transaction</div>
          <div className="flex flex-wrap items-center gap-x-4 gap-y-2">
            <Text variant="heading" size="lg" as="h1">{shortId(id)}</Text>
            <ClipboardText text={id} size="sm" tooltip={{ text: "Copy id", copiedText: "Copied" }} />
          </div>
          <dl className="flex flex-wrap gap-x-8 gap-y-3">
            <Fact label="operation"><IdLink id={opId}>{shortId(opId)}</IdLink></Fact>
            <Fact label="program step">
              <NavLink hash={hashes.op(opId)} selection={`tx:${id}`}>step {site.location} →</NavLink>
            </Fact>
            <Fact label="data model">
              {tx.data_model ? <IdLink id={tx.data_model}>{shortId(tx.data_model)}</IdLink> : <Muted>none — framework artifacts only</Muted>}
            </Fact>
            <Fact label="isolation"><FactBadge fact={isolation(tx.isolation)} /></Fact>
            <Fact label="commit">
              <FactBadge fact={commitGuarantee(tx.idempotency)} />
              {tx.idempotency.kind === "deduplicated_by" && (
                <span className="text-xs text-kumo-subtle">by <KeyComponents value={tx.idempotency.key} /></span>
              )}
            </Fact>
            <Fact label="rejection"><FactBadge fact={transactionRejection(rejected !== null)} /></Fact>
            {obligations.length > 0 && <Fact label="verdicts"><StatusChips obKey={obKey} /></Fact>}
          </dl>
        </header>

        <SectionCard
          title="Requirements"
          count={requirementCount}
          hint="obligations over this transaction's committed history, argued from the transactions alone — never from runtime topology"
          bodyClassName="space-y-4 p-4"
        >
          {requirementCount === 0 && <Muted>the transaction declares no serializability or ordering requirement</Muted>}
          {tx.requirements.serializability.map((r, i) => {
            const proof = transactionProofs.proofForRequirement(opId, id, "transaction_serializability", i);
            return (
              <RequirementBlock
                key={`s${i}`}
                prop="transaction_serializability"
                index={i}
                highlighted={isReq("transaction_serializability", i)}
                label={<Badge variant="blue">serializability #{i}</Badge>}
                fact={<FactBadge fact={serializabilityRequirement(r.key)} />}
                declares={<><span className="text-xs text-kumo-subtle">key</span><RefText value={r.key} /></>}
              >
                {proof?.kind === "serializability"
                  ? <SerializabilityProof view={proof.view} layout="wide" />
                  : <Muted>no argument recorded for this requirement</Muted>}
              </RequirementBlock>
            );
          })}
          {tx.requirements.ordering.map((r, i) => {
            const proof = transactionProofs.proofForRequirement(opId, id, "transaction_ordering", i);
            return (
              <RequirementBlock
                key={`o${i}`}
                prop="transaction_ordering"
                index={i}
                highlighted={isReq("transaction_ordering", i)}
                label={<Badge variant="purple">ordering #{i}</Badge>}
                fact={<FactBadge fact={orderingRequirement(r.key, r.position)} />}
                declares={
                  <>
                    <span className="text-xs text-kumo-subtle">key</span><RefText value={r.key} />
                    <span className="text-xs text-kumo-subtle">position</span><RefText value={r.position} />
                  </>
                }
              >
                {proof?.kind === "ordering"
                  ? <OrderingProof view={proof.view} layout="wide" />
                  : <Muted>no argument recorded for this requirement</Muted>}
              </RequirementBlock>
            );
          })}
        </SectionCard>

        <SectionCard
          title="Steps"
          count={tx.steps.length}
          hint="the body, in order — a commit guard is a step that can reject the whole transaction"
          bodyClassName="space-y-0.5 p-2"
        >
          {tx.steps.map((s, i) => (
            <TxStepRow key={i} step={s} index={i} txId={id} opId={opId} />
          ))}
        </SectionCard>

        <SectionCard
          title="Outcomes"
          hint="where control goes after an attempt"
          bodyClassName="space-y-2 p-4"
        >
          <p className="text-sm leading-relaxed text-kumo-default">
            <Badge variant="success">↓ committed</Badge>{" "}
            <span className="text-kumo-subtle">the program continues after step {site.location}, with what the commit established available.</span>
          </p>
          {rejected ? (
            <p className="text-sm leading-relaxed text-kumo-default">
              <Badge variant="warning">↘ rejected</Badge>{" "}
              <span className="text-kumo-subtle">
                nothing committed and no binding of the body available; control enters the block at{" "}
                {site.location}.rejected ({rejected.steps.length} step{rejected.steps.length === 1 ? "" : "s"}).
              </span>
            </p>
          ) : (
            <p className="text-sm leading-relaxed text-kumo-subtle">
              The body has no commit guard, so an attempt either commits or is interrupted — it never rejects.
            </p>
          )}
          <NavLink hash={hashes.op(opId)} selection={`tx:${id}`}>see the program on the operation page →</NavLink>
        </SectionCard>

        <SectionCard title="Obligations" count={obligations.length} hint="the report's verdicts on this transaction" bodyClassName="space-y-2 p-3">
          {obligations.length
            ? obligations.map((ob) => <ObligationCard key={ob.id} ob={ob} />)
            : <Muted>no report loaded, or no obligation anchors to this transaction</Muted>}
        </SectionCard>
      </div>
    </div>
  );
}

/** One declared requirement with its argument beneath it. The block a
 *  link names is ringed, so a reader arriving from a card or an arc
 *  sees which argument they were sent to. */
function RequirementBlock({
  prop, index, highlighted, label, fact, declares, children,
}: {
  prop: RequirementRef["prop"];
  index: number;
  highlighted: boolean;
  label: ReactNode;
  fact: ReactNode;
  declares: ReactNode;
  children: ReactNode;
}) {
  return (
    <div
      data-req={`${prop}:${index}`}
      className={`min-w-0 space-y-3 rounded-lg border p-4 ${highlighted ? "border-kumo-brand ring-1 ring-kumo-brand" : "border-kumo-hairline"}`}
    >
      <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
        {label}
        {fact}
        {declares}
      </div>
      {children}
    </div>
  );
}
