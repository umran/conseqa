import { Badge } from "@cloudflare/kumo/components/badge";
import { Button } from "@cloudflare/kumo/components/button";
import { ClipboardText } from "@cloudflare/kumo/components/clipboard-text";
import { Collapsible } from "@cloudflare/kumo/components/collapsible";
import { Empty } from "@cloudflare/kumo/components/empty";
import { Flow } from "@cloudflare/kumo/components/flow";
import { Table } from "@cloudflare/kumo/components/table";
import { Text } from "@cloudflare/kumo/components/text";
import { ArrowSquareOutIcon, CaretRightIcon, GraphIcon } from "@phosphor-icons/react";
import type { CSSProperties, ComponentPropsWithRef, ReactElement, ReactNode } from "react";

import {
  commitGuarantee,
  delivery,
  isolation,
  memberAssignment,
  memberConcurrency,
  noRuntimeDeclared,
  requestIdentity,
  requestRouting,
  subscriptionRouting,
} from "../lib/explain";
import { pathText, shortId } from "../lib/ids";
import { effectDef, effectSummary, locationLabel, operationTransactions, walkProgram, type StepHop } from "../lib/index";
import { propertyMatchesRequirement, worstStatus } from "../lib/obligations";
import { hashes } from "../lib/route";
import { conditionText, predicateText } from "../lib/text";
import { useApp, type DetailContext } from "../state/AppState";
import { Fact, FactBadge, IdLink, KeyComponents, Mono, Muted, RefText, SectionCard, StatusBadge, StatusChips, selectableRow } from "../panels/parts";
import type { Effect, Id, Operation, OperationBlock, RequirementKind, TransactionStep, TransitionSideEffect } from "../types/model";

type EffectKind = (Effect | TransitionSideEffect)["kind"];

const EFFECT_BADGE: Record<EffectKind, { variant: "purple" | "orange" | "warning"; label: string }> = {
  publication: { variant: "purple", label: "publication" },
  request: { variant: "orange", label: "request" },
  external: { variant: "warning", label: "external" },
};

/** Left-edge stripes reuse the system graph's edge colours, so a step's
 *  kind reads the same way here as its edge does there. */
const STEP_STRIPE: Record<string, string> = {
  tx: "var(--arch-edge-request)",
  effect: "var(--arch-edge-publish)",
  intent: "var(--arch-edge-publish)",
  decision: "var(--arch-edge-subscribe)",
  terminal: "var(--arch-edge-client)",
};

function EffectKindBadge({ kind }: { kind: EffectKind | null }) {
  if (!kind) return <Badge variant="neutral">unresolved</Badge>;
  const { variant, label } = EFFECT_BADGE[kind];
  return <Badge variant={variant}>{label}</Badge>;
}

// ---------------------------------------------------------------------------
// Program steps
// ---------------------------------------------------------------------------

type StepCardProps = Omit<ComponentPropsWithRef<"div">, "children"> & {
  selKey: string;
  detailId: string;
  ctx?: DetailContext;
  stripe: string;
  dashed?: boolean;
  children: ReactNode;
};

/** A selectable step card. Rendered through `Flow.Node`'s `render` prop, so
 *  it forwards the ref, position style and data attributes Kumo's layout
 *  engine clones onto it. Decision cards nest whole blocks, so activation
 *  stops propagating: a click inside an arm selects the inner card only. */
function StepCard({ selKey, detailId, ctx, stripe, dashed, children, className, style, ...rest }: StepCardProps) {
  const { selection, select } = useApp();
  const selected = selection === selKey;
  const activate = () => select(selKey, { id: detailId, ctx: ctx ?? {} });
  return (
    <div
      {...rest}
      role="button"
      tabIndex={0}
      onClick={(e) => {
        e.stopPropagation();
        activate();
      }}
      onKeyDown={(e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          e.stopPropagation();
          activate();
        }
      }}
      className={`w-(--step-w) rounded-lg border border-kumo-hairline bg-kumo-base p-3 text-left shadow-sm transition-shadow hover:shadow-md ${selected ? "ring-2 ring-kumo-brand" : ""} ${className ?? ""}`}
      // Flow.Node pins `cursor: default` inline; the card is interactive.
      style={{ ...style, cursor: "pointer", borderLeft: `3px ${dashed ? "dashed" : "solid"} ${stripe}` }}
    >
      {children}
    </div>
  );
}

function StepTitle({ children }: { children: ReactNode }) {
  return <div className="mt-1.5 font-mono text-[13px] font-semibold text-kumo-strong">{children}</div>;
}

function TxStepRow({ step, index, txId, opId }: { step: TransactionStep; index: number; txId: Id; opId: Id }) {
  const { selection, select, navigateTo } = useApp();
  const selKey = `ts:${txId}:${index}`;
  const selected = selection === selKey;

  let kind: string;
  let title: string;
  let note: string;
  switch (step.kind) {
    case "read":
      kind = "read"; title = shortId(step.bind);
      note = `${shortId(step.target.object)} where ${predicateText(step.target.predicate)}`;
      break;
    case "write":
      kind = "write"; title = shortId(step.target.object);
      note = `${step.fields.map(pathText).join(", ")} · ${step.values.kind}`;
      break;
    case "insert":
      kind = "insert"; title = shortId(step.object); note = `values: ${step.values.kind}`;
      break;
    case "delete":
      kind = "delete"; title = shortId(step.target.object); note = `where ${predicateText(step.target.predicate)}`;
      break;
    case "lock":
      kind = "lock"; title = shortId(step.target.object); note = `${step.mode} · order ${step.order.kind}`;
      break;
    case "transition":
      kind = "transition"; title = shortId(step.transition); note = shortId(step.machine);
      break;
    case "establish_effect_intent":
      kind = "establish intent"; title = shortId(step.bind);
      note = `${shortId(step.effect_id)} · values: ${step.values.kind}`;
      break;
    case "establish_transaction_output":
      kind = "establish output"; title = shortId(step.bind);
      note = `${shortId(step.schema)} · values: ${step.values.kind}`;
      break;
  }

  const activate = () => select(selKey, { id: txId, ctx: { txStep: { op: opId, tx: txId, index } } });

  return (
    <div
      role="button"
      tabIndex={0}
      onClick={(e) => {
        e.stopPropagation();
        activate();
      }}
      onKeyDown={(e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          e.stopPropagation();
          activate();
        }
      }}
      className={`flex cursor-pointer items-start gap-2 rounded-md px-2 py-1.5 hover:bg-kumo-tint ${selected ? "bg-kumo-tint ring-1 ring-kumo-brand" : ""}`}
    >
      <Badge variant="neutral">{index + 1}</Badge>
      <div className="min-w-0 flex-1">
        <div className="flex flex-wrap items-center gap-1.5">
          <span className="text-[11px] uppercase tracking-wider text-kumo-subtle">{kind}</span>
          <Mono className="text-kumo-strong">{title}</Mono>
        </div>
        <div className="truncate text-xs text-kumo-subtle">{note}</div>
      </div>
      {step.kind === "transition" && (
        <Button
          variant="ghost"
          size="xs"
          shape="square"
          icon={ArrowSquareOutIcon}
          aria-label="Open state machine"
          onClick={(e) => {
            e.stopPropagation();
            navigateTo(hashes.machine(step.machine, step.transition), `t:${step.transition}`);
          }}
        />
      )}
    </div>
  );
}

/** One arm of a decision: its label and its block, rendered recursively. */
function DecisionArm({ opId, op, label, block, hops }: { opId: Id; op: Operation; label: string; block: OperationBlock | null; hops: StepHop[] }) {
  return (
    <div className="min-w-0 space-y-2 rounded-md border border-kumo-hairline bg-kumo-elevated/30 p-2">
      <Badge variant="outline">{label}</Badge>
      {block ? (
        block.steps.length ? (
          <ProgramBlock opId={opId} op={op} block={block} hops={hops} nested />
        ) : (
          <Muted>empty arm</Muted>
        )
      ) : (
        <Muted>falls through</Muted>
      )}
    </div>
  );
}

/** One block of the program as a vertical sequence of step cards. The
 *  top-level block is a Kumo Flow with connectors; nested arm blocks are
 *  plain stacks, so arbitrary nesting stays legible. */
function ProgramBlock({ opId, op, block, hops, nested }: { opId: Id; op: Operation; block: OperationBlock; hops: StepHop[]; nested?: boolean }) {
  const { model, index, expandedTx, toggleTx } = useApp();
  const effectKind = (effectId: Id): EffectKind | null => effectDef(model, index, effectId)?.effect.kind ?? null;

  const stepCtx = (location: string): DetailContext => ({ step: { op: opId, location } });

  const nodes: { key: string; element: ReactElement }[] = block.steps.map((step, si) => {
    const ownHops: StepHop[] = [...hops, { step: si }];
    const location = locationLabel(ownHops);
    const under = (arm: StepHop["arm"]): StepHop[] => [...hops, { step: si, arm }];

    switch (step.kind) {
      case "transaction": {
        const expanded = expandedTx.has(location);
        return {
          key: location,
          element: (
            <StepCard selKey={`tx:${step.id}`} detailId={step.id} stripe={STEP_STRIPE.tx}>
              <div className="flex items-center justify-between gap-2">
                <Badge variant="neutral">transaction</Badge>
                <StatusChips obKey={`${opId}/${step.id}`} />
              </div>
              <StepTitle>{shortId(step.id)}</StepTitle>
              <div className="mt-1.5 flex flex-wrap items-center gap-1.5 text-xs text-kumo-subtle">
                <FactBadge fact={commitGuarantee(step.idempotency)} />
                {step.idempotency.kind === "deduplicated_by" && (
                  <span>by <KeyComponents value={step.idempotency.key} /></span>
                )}
              </div>
              <div className="mt-1 flex flex-wrap items-center gap-1.5 text-xs text-kumo-subtle">
                <FactBadge fact={isolation(step.isolation)} />
                {step.data_model && <span>on {shortId(step.data_model)}</span>}
              </div>
              <Collapsible.Root open={expanded} onOpenChange={() => toggleTx(location)}>
                <Collapsible.Trigger
                  className="mt-2 flex w-full cursor-pointer items-center gap-1 text-xs text-kumo-link hover:underline"
                  onClick={(e) => e.stopPropagation()}
                >
                  <CaretRightIcon size={12} className={`transition-transform ${expanded ? "rotate-90" : ""}`} />
                  {step.steps.length} step{step.steps.length === 1 ? "" : "s"}
                </Collapsible.Trigger>
                <Collapsible.Panel>
                  <div className="mt-1.5 space-y-0.5 rounded-md border border-kumo-hairline bg-kumo-elevated/40 p-1">
                    {step.steps.map((ts, ti) => (
                      <TxStepRow key={ti} step={ts} index={ti} txId={step.id} opId={opId} />
                    ))}
                  </div>
                </Collapsible.Panel>
              </Collapsible.Root>
            </StepCard>
          ),
        };
      }

      case "execute_effect":
        return {
          key: location,
          element: (
            <StepCard selKey={`fx:${location}:${step.effect_id}`} detailId={step.effect_id} stripe={STEP_STRIPE.effect}>
              <div className="flex flex-wrap items-center gap-1.5">
                <Badge variant="neutral">execute effect</Badge>
                <EffectKindBadge kind={step.effect.kind} />
                {step.bind && <Badge variant="info">binds {shortId(step.bind)}</Badge>}
              </div>
              <StepTitle>{shortId(step.effect_id)}</StepTitle>
              <div className="mt-1 text-xs text-kumo-subtle">{effectSummary(model, index, step.effect_id)}</div>
              <div className="mt-1 text-xs text-kumo-subtle">
                instance: <Badge variant={step.values.kind === "deterministic" ? "info" : "warning"}>{step.values.kind}</Badge>
              </div>
            </StepCard>
          ),
        };

      case "execute_effect_intent": {
        const intent = index.get(step.intent);
        const eff = intent?.kind === "intent" ? intent.effect : null;
        const via = intent?.kind === "intent" && intent.via !== undefined;
        const effKind = eff ? effectKind(eff) : null;
        return {
          key: location,
          element: (
            <StepCard selKey={`fi:${location}:${step.intent}`} detailId={step.intent} stripe={STEP_STRIPE.intent} dashed>
              <div className="flex flex-wrap items-center gap-1.5">
                <Badge variant="neutral">execute intent</Badge>
                <EffectKindBadge kind={effKind} />
                {via && <Badge variant="info">via transition</Badge>}
                {step.bind && <Badge variant="info">binds {shortId(step.bind)}</Badge>}
              </div>
              <StepTitle>{shortId(step.intent)}</StepTitle>
              <div className="mt-1 text-xs text-kumo-subtle">{eff ? effectSummary(model, index, eff) : "unresolved intent"}</div>
            </StepCard>
          ),
        };
      }

      case "match_result":
        return {
          key: location,
          element: (
            <StepCard selKey={`step:${location}`} detailId={opId} ctx={stepCtx(location)} stripe={STEP_STRIPE.decision}>
              <div className="flex flex-wrap items-center gap-1.5">
                <Badge variant="neutral">match result</Badge>
                <Badge variant="outline">step {location}</Badge>
              </div>
              <StepTitle>{shortId(step.result)}</StepTitle>
              <div className="mt-2 grid gap-2 sm:grid-cols-2">
                <DecisionArm opId={opId} op={op} label="ok" block={step.ok} hops={under("ok")} />
                <DecisionArm opId={opId} op={op} label="err" block={step.err} hops={under("err")} />
              </div>
            </StepCard>
          ),
        };

      case "branch":
        return {
          key: location,
          element: (
            <StepCard selKey={`step:${location}`} detailId={opId} ctx={stepCtx(location)} stripe={STEP_STRIPE.decision}>
              <div className="flex flex-wrap items-center gap-1.5">
                <Badge variant="neutral">branch</Badge>
                <Badge variant="outline">step {location}</Badge>
                {step.condition.kind === "unspecified" && <Badge variant="warning">condition unspecified</Badge>}
              </div>
              <StepTitle>
                <span className="break-words font-normal text-kumo-subtle">{conditionText(step.condition)}</span>
              </StepTitle>
              <div className="mt-2 grid gap-2 sm:grid-cols-2">
                <DecisionArm opId={opId} op={op} label="then" block={step.then} hops={under("then")} />
                <DecisionArm opId={opId} op={op} label="otherwise" block={step.otherwise} hops={under("otherwise")} />
              </div>
            </StepCard>
          ),
        };

      case "return":
        return {
          key: location,
          element: (
            <StepCard selKey={`step:${location}`} detailId={opId} ctx={stepCtx(location)} stripe={STEP_STRIPE.terminal}>
              <div className="flex flex-wrap items-center gap-1.5">
                <Badge variant="neutral">return</Badge>
                <Badge variant={step.outcome.kind === "ok" ? "success" : "warning"}>{step.outcome.kind}</Badge>
              </div>
              <StepTitle>{shortId(step.request)}</StepTitle>
              <div className="mt-1 text-xs text-kumo-subtle">
                payload: <Badge variant={step.outcome.values.kind === "deterministic" ? "info" : "warning"}>{step.outcome.values.kind}</Badge>
              </div>
            </StepCard>
          ),
        };

      case "complete":
        return {
          key: location,
          element: (
            <StepCard selKey={`step:${location}`} detailId={opId} ctx={stepCtx(location)} stripe={STEP_STRIPE.terminal}>
              <Badge variant="neutral">complete</Badge>
              <div className="mt-1 text-xs text-kumo-subtle">terminates without a returned value</div>
            </StepCard>
          ),
        };
    }
  });

  if (nested) {
    // Arm blocks stack without connectors; the surrounding decision card
    // already communicates the sequence.
    return (
      <div className="space-y-2" style={{ "--step-w": "100%" } as CSSProperties}>
        {nodes.map((n) => (
          <div key={n.key}>{n.element}</div>
        ))}
      </div>
    );
  }

  return (
    // Step cards size to the section body (a container), capped for
    // readability; the 12px accounts for the diagram's own padding,
    // which keeps selection rings clear of its clipping edge.
    <div className="arch-flow" style={{ "--step-w": "min(640px, 100cqw - 12px)" } as CSSProperties}>
      <Flow orientation="vertical" canvas={false} padding={{ x: 6, y: 6 }}>
        {nodes.map((n) => (
          <Flow.Node key={n.key} id={n.key} render={n.element} />
        ))}
      </Flow>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Requirements and inputs
// ---------------------------------------------------------------------------

function RequirementsTable({ id, op }: { id: Id; op: Operation }) {
  const { obligations, selection, select } = useApp();
  const reqs = op.requirements;

  const rows: { prop: RequirementKind; i: number; declares: ReactNode }[] = [];
  reqs.serialization.forEach((r, i) => rows.push({ prop: "serialization", i, declares: <RefText value={r.key} /> }));
  reqs.ordering.forEach((r, i) => rows.push({ prop: "ordering", i, declares: <RefText value={r.key} /> }));
  reqs.idempotency.forEach((r, i) =>
    rows.push({
      prop: "idempotency",
      i,
      declares: (
        <>
          <KeyComponents value={r.key} />
          {r.result === "replay_consistent" && <Badge variant="info">replay-consistent result</Badge>}
        </>
      ),
    }));
  reqs.recoverability.forEach((r, i) =>
    rows.push({
      prop: "recoverability",
      i,
      declares: (
        <>
          <KeyComponents value={r.key} />
          <Badge variant={r.completion === "guaranteed" ? "success" : "neutral"}>{r.completion} completion</Badge>
        </>
      ),
    }));

  if (!rows.length) return <Muted>operation declares no requirements</Muted>;

  return (
    <Table>
      <Table.Header variant="compact">
        <Table.Row>
          <Table.Head>requirement</Table.Head>
          <Table.Head>declares</Table.Head>
          <Table.Head>verdict</Table.Head>
        </Table.Row>
      </Table.Header>
      <Table.Body>
        {rows.map((row) => {
          const key = `req:${row.prop}:${row.i}`;
          const obs = (obligations.get(id) ?? []).filter(
                (ob) => ob.subject.kind === "operation" && ob.subject.requirement === row.i &&
                  propertyMatchesRequirement(ob.property, row.prop));
          const status = obs.length ? worstStatus(obs) : null;
          return (
            <Table.Row
              key={key}
              className={selectableRow(selection === key)}
              onClick={() => select(key, { id, ctx: { req: { prop: row.prop, index: row.i } } })}
            >
              <Table.Cell className="whitespace-nowrap">
                <span className="font-medium text-kumo-strong">{row.prop}</span>
                <span className="ml-1.5 text-kumo-inactive">#{row.i}</span>
              </Table.Cell>
              <Table.Cell>
                <span className="flex flex-wrap items-center gap-x-2 gap-y-1">{row.declares}</span>
              </Table.Cell>
              <Table.Cell className="whitespace-nowrap">
                {status ? (
                  <span className="inline-flex items-center gap-1.5">
                    <StatusBadge status={status} />
                    {obs.length > 1 && <span className="text-xs text-kumo-inactive">{obs.length} obligations</span>}
                  </span>
                ) : (
                  <span className="text-kumo-inactive">—</span>
                )}
              </Table.Cell>
            </Table.Row>
          );
        })}
      </Table.Body>
    </Table>
  );
}

/** How one boundary is realized: the pool that executes it, the member
 *  affinity it declares, and what one member does at a time.
 *
 *  Requests and subscriptions are separate primitives with the same
 *  shape — a routing key and a member assignment, terminating at a pool
 *  — so they are shown the same way and in the same column, next to but
 *  never mixed with the L0 contract they realize. */
function Realization({ opId, inputId, kind }: { opId: Id; inputId: Id; kind: "request" | "subscription" }) {
  const { model } = useApp();

  if (kind === "request") {
    const routed = Object.entries(model.runtime?.routers ?? {}).find(
      ([, r]) => r.boundary.operation === opId && r.boundary.input === inputId,
    );
    if (!routed) return <FactBadge fact={noRuntimeDeclared()} />;
    const [routerId, router] = routed;
    const pool = model.runtime?.execution_pools?.[router.pool];
    return (
      <>
        <span className="inline-flex items-center gap-1 text-xs text-kumo-subtle">
          router
          <IdLink id={routerId}>{shortId(routerId)}</IdLink>
        </span>
        <FactBadge fact={requestRouting(router.routing?.key)} />
        {router.routing && <FactBadge fact={memberAssignment(router.routing.member_assignment)} />}
        {pool && <FactBadge fact={memberConcurrency(pool.member_concurrency)} />}
      </>
    );
  }

  const runtime = model.runtime?.subscriptions?.[opId]?.[inputId];
  if (!runtime) {
    return (
      <>
        <FactBadge fact={noRuntimeDeclared()} />
        <FactBadge fact={delivery("unspecified")} />
      </>
    );
  }
  const pool = model.runtime?.execution_pools?.[runtime.dispatch.pool];
  return (
    <>
      <FactBadge fact={delivery(runtime.delivery)} />
      <FactBadge fact={subscriptionRouting(runtime.dispatch.routing?.key)} />
      {runtime.dispatch.routing && (
        <FactBadge fact={memberAssignment(runtime.dispatch.routing.member_assignment)} />
      )}
      {pool && <FactBadge fact={memberConcurrency(pool.member_concurrency)} />}
    </>
  );
}

function InputsTable({ opId, op }: { opId: Id; op: Operation }) {
  const { selection, select } = useApp();
  const inputs = Object.entries(op.inputs);

  if (!inputs.length) return <Muted>operation declares no inputs</Muted>;

  return (
    <Table>
      <Table.Header variant="compact">
        <Table.Row>
          <Table.Head>input</Table.Head>
          <Table.Head>kind</Table.Head>
          <Table.Head>source</Table.Head>
          <Table.Head>L0 contract</Table.Head>
          <Table.Head>L1 realization</Table.Head>
        </Table.Row>
      </Table.Header>
      <Table.Body>
        {inputs.map(([inputId, input]) => {
          const key = `in:${inputId}`;
          return (
            <Table.Row
              key={key}
              className={selectableRow(selection === key)}
              onClick={() => select(key, { id: inputId, ctx: {} })}
            >
              <Table.Cell><Mono className="text-kumo-strong">{shortId(inputId)}</Mono></Table.Cell>
              <Table.Cell><Badge variant={input.kind === "request" ? "info" : "blue"}>{input.kind}</Badge></Table.Cell>
              <Table.Cell className="whitespace-nowrap">
                {input.kind === "request" ? (
                  <span className="inline-flex items-center gap-1.5">
                    <span className="text-xs text-kumo-subtle">schema</span>
                    <IdLink id={input.schema}>{shortId(input.schema)}</IdLink>
                  </span>
                ) : (
                  <span className="inline-flex items-center gap-1.5">
                    <span className="text-xs text-kumo-subtle">topic</span>
                    <IdLink id={input.topic}>{shortId(input.topic)}</IdLink>
                  </span>
                )}
              </Table.Cell>
              <Table.Cell>
                <span className="flex flex-wrap items-center gap-1.5">
                  {input.kind === "request" ? (
                    <>
                      <FactBadge fact={requestIdentity(input.identity)} />
                      <span className="inline-flex flex-wrap items-center gap-1 text-xs text-kumo-subtle">
                        <Mono>Result&lt;</Mono>
                        <IdLink id={input.result.ok}>{shortId(input.result.ok)}</IdLink>
                        <Mono>,</Mono>
                        <IdLink id={input.result.err.schema}>{shortId(input.result.err.schema)}</IdLink>
                        {input.result.err.disposition !== "unspecified" && (
                          <Mono>{input.result.err.disposition}</Mono>
                        )}
                        <Mono>&gt;</Mono>
                      </span>
                    </>
                  ) : (
                    <Badge variant="neutral">
                      {input.messages.kind === "all"
                        ? "all topic messages"
                        : input.messages.schemas.map(shortId).join(", ")}
                    </Badge>
                  )}
                </span>
              </Table.Cell>
              <Table.Cell>
                <span className="flex flex-wrap items-center gap-1.5">
                  <Realization opId={opId} inputId={inputId} kind={input.kind} />
                </span>
              </Table.Cell>
            </Table.Row>
          );
        })}
      </Table.Body>
    </Table>
  );
}

// ---------------------------------------------------------------------------
// Page
// ---------------------------------------------------------------------------

export function OperationView({ id }: { id: string }) {
  const { model, navigateTo, obligations } = useApp();
  const op = model.operations[id];

  if (!op) {
    return (
      <div className="flex h-full items-center justify-center">
        <Empty size="sm" icon={<GraphIcon size={32} className="text-kumo-inactive" />} title={`unknown operation ${id}`} />
      </div>
    );
  }

  const reqs = op.requirements;
  const requirementCount = reqs.serialization.length + reqs.ordering.length + reqs.idempotency.length + reqs.recoverability.length;
  const inputCount = Object.keys(op.inputs).length;
  const transactions = operationTransactions(op);
  const transactionCount = transactions.length;
  const stepCount = walkProgram(op.program).length;
  const machines = [...new Set(
    transactions.flatMap((tx) => tx.steps.flatMap((s) => (s.kind === "transition" ? [s.machine] : []))),
  )];

  return (
    <div className="h-full overflow-auto">
      <div className="mx-auto max-w-[1240px] space-y-6 p-6">
        <header className="space-y-4 border-b border-kumo-hairline pb-5">
          <div className="flex flex-wrap items-center gap-x-4 gap-y-2">
            <Text variant="heading2" as="h1">{shortId(id)}</Text>
            <ClipboardText text={id} size="sm" tooltip={{ text: "Copy id", copiedText: "Copied" }} />
          </div>
          {op.description && <p className="max-w-3xl text-sm leading-relaxed text-kumo-default">{op.description}</p>}
          <dl className="flex flex-wrap gap-x-8 gap-y-3">
            <Fact label="service"><IdLink id={op.service}>{shortId(op.service)}</IdLink></Fact>
            <Fact label="transactions">{transactionCount}</Fact>
            <Fact label="program steps">{stepCount}</Fact>
            {machines.length > 0 && (
              <Fact label="state machines">
                {machines.map((m) => (
                  <Button key={m} variant="ghost" size="xs" icon={ArrowSquareOutIcon} onClick={() => navigateTo(hashes.machine(m))}>
                    {shortId(m)}
                  </Button>
                ))}
              </Fact>
            )}
            {(obligations.get(id) ?? []).length > 0 && <Fact label="verdicts"><StatusChips obKey={id} /></Fact>}
          </dl>
        </header>

        <SectionCard title="Requirements" count={requirementCount} hint="proof obligations on every invocation">
          <div className="overflow-x-auto">
            <RequirementsTable id={id} op={op} />
          </div>
        </SectionCard>

        <SectionCard title="Inputs" count={inputCount} hint="what starts an invocation">
          <div className="overflow-x-auto">
            <InputsTable opId={id} op={op} />
          </div>
        </SectionCard>

        <SectionCard
          title="Program"
          count={stepCount}
          hint="the operation's one causal control structure — a decision's arms are alternatives, and every path ends at a terminal"
          bodyClassName="@container space-y-4 p-4"
        >
          {op.program.steps.length ? (
            <ProgramBlock opId={id} op={op} block={op.program} hops={[]} />
          ) : (
            <Empty size="sm" title="operation declares no program steps" />
          )}
        </SectionCard>
      </div>
    </div>
  );
}
