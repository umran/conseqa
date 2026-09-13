import { Badge } from "@cloudflare/kumo/components/badge";
import { Button } from "@cloudflare/kumo/components/button";
import { ClipboardText } from "@cloudflare/kumo/components/clipboard-text";
import { Collapsible } from "@cloudflare/kumo/components/collapsible";
import { Empty } from "@cloudflare/kumo/components/empty";
import { Flow } from "@cloudflare/kumo/components/flow";
import { Table } from "@cloudflare/kumo/components/table";
import { Text } from "@cloudflare/kumo/components/text";
import { Tooltip } from "@cloudflare/kumo/components/tooltip";
import { ArrowSquareOutIcon, CaretRightIcon, GraphIcon } from "@phosphor-icons/react";
import { Fragment, useEffect, type CSSProperties, type ComponentPropsWithRef, type ReactElement, type ReactNode } from "react";

import { definedAtLabel, usedAtLabel, type BindingKind } from "../lib/bindings";
import {
  bindingKind,
  commitGuarantee,
  delivery,
  intrinsicRedrive,
  isolation,
  memberAssignment,
  memberConcurrency,
  noRuntimeDeclared,
  orderingRequirement,
  outboxRouting,
  requestIdentity,
  requestRouting,
  serializabilityRequirement,
  subscriptionRouting,
  transactionRejection,
} from "../lib/explain";
import { proofSummary } from "../lib/consistency";
import { pathText, shortId } from "../lib/ids";
import {
  effectDef, effectSummary, errArm, locationLabel, operationTransactions, stepRejects, walkProgram,
  type Arm, type StepHop,
  blockTerminates,
} from "../lib/index";
import { propertyMatchesRequirement, worstStatus } from "../lib/obligations";
import { hashes } from "../lib/route";
import { requirementKey, useApp, type DetailContext } from "../state/AppState";
import {
  BindingChip, BindingKindTag, BindingRoots, ConditionView, Fact, FactBadge, IdLink, KeyComponents, Mono, Muted,
  PredicateView, RefText, SectionCard, StatusBadge, StatusChips, selectableRow, useProgramNavigation,
} from "../panels/parts";
import type {
  Effect, Id, Operation, OperationBlock, RequirementKind, ResultType, SelectorPredicate,
  TransactionStep, TransitionSideEffect,
} from "../types/model";

type EffectKind = (Effect | TransitionSideEffect)["kind"];

const EFFECT_BADGE: Record<EffectKind, { variant: "purple" | "orange" | "warning"; label: string }> = {
  publication: { variant: "purple", label: "publication" },
  request: { variant: "orange", label: "request" },
  external: { variant: "warning", label: "external" },
  outbox_write: { variant: "purple", label: "outbox write" },
};

/** Left-edge stripes reuse the system graph's edge colours, so a step's
 *  kind reads the same way here as its edge does there. */
const STEP_STRIPE: Record<string, string> = {
  tx: "var(--arch-edge-request)",
  effect: "var(--arch-edge-publish)",
  intent: "var(--arch-edge-publish)",
  decision: "var(--arch-edge-subscribe)",
  terminal: "var(--arch-edge-client)",
  sync: "var(--arch-text-subtle)",
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
      data-selkey={selKey}
    >
      {children}
    </div>
  );
}

function StepTitle({ children }: { children: ReactNode }) {
  return <div className="mt-1.5 font-mono text-[13px] font-semibold text-kumo-strong">{children}</div>;
}

/** One name a step makes available to later control, said as an
 *  assignment: the defining chip — the kind of binding and the full
 *  bound name, exactly the name later value references and
 *  synchronizations use — and what produces it. Every producing step
 *  renders one of these, so a reader can scan the flow for what each
 *  step binds. */
function BindingRow({ name, kind, from }: { name: Id; kind: BindingKind; from?: ReactNode }) {
  return (
    <div className="flex flex-wrap items-center gap-x-1.5 gap-y-0.5 rounded-md border border-kumo-hairline bg-kumo-tint px-2 py-1">
      <BindingChip role="defines" name={name} kind={kind} />
      {from && (
        <span className="inline-flex min-w-0 flex-wrap items-center gap-1 text-xs text-kumo-subtle">
          <span className="text-kumo-inactive">←</span>
          {from}
        </span>
      )}
    </div>
  );
}

/** The binding rows of one step, stacked under its facts. */
function Bindings({ children }: { children: ReactNode }) {
  return <div className="mt-2 space-y-1">{children}</div>;
}

function TxStepRow({ step, index, txId, opId }: { step: TransactionStep; index: number; txId: Id; opId: Id }) {
  const { selection, select, navigateTo } = useApp();
  const selKey = `ts:${txId}:${index}`;
  const selected = selection === selKey;

  // A binding step leads with its defining chip — the same chip the
  // program-level producers render — and its note reads as the
  // producer, after an arrow. A step that consumes a binding renders the
  // binding's using chip in its note. A commit guard — a step that can
  // reject the whole transaction — is marked as one.
  const where = (target: { predicate: SelectorPredicate }) => (
    <span className="inline-flex flex-wrap items-center gap-1">
      <span>where</span>
      <PredicateView predicate={target.predicate} />
    </span>
  );
  let kind: string;
  let title: ReactNode;
  let note: ReactNode;
  switch (step.kind) {
    case "read":
      kind = "read";
      title = <BindingChip role="defines" name={step.bind} kind="read" />;
      note = (
        <>
          <span className="text-kumo-inactive">←</span>
          <Mono>{shortId(step.target.object)}</Mono>
          {where(step.target)}
          <span className="text-kumo-inactive">· transaction-local</span>
        </>
      );
      break;
    case "write":
      kind = "write"; title = <Mono className="text-kumo-strong">{shortId(step.target.object)}</Mono>;
      note = <><span>{step.fields.map(pathText).join(", ")} · {step.values.kind}</span><BindingRoots value={step.values} /></>;
      break;
    case "insert":
      kind = "insert"; title = <Mono className="text-kumo-strong">{shortId(step.object)}</Mono>;
      note = <><span>values: {step.values.kind}</span><BindingRoots value={step.values} /></>;
      break;
    case "delete":
      kind = "delete"; title = <Mono className="text-kumo-strong">{shortId(step.target.object)}</Mono>;
      note = where(step.target);
      break;
    case "lock":
      kind = "lock"; title = <Mono className="text-kumo-strong">{shortId(step.target.object)}</Mono>;
      note = <span>{step.mode} · order {step.order.kind}</span>;
      break;
    case "transition": {
      kind = "transition"; title = <Mono className="text-kumo-strong">{shortId(step.transition)}</Mono>;
      const intents = Object.values(step.effect_intents);
      const admissions = Object.entries(step.effects ?? {});
      note = (
        <>
          <span>{shortId(step.machine)}</span>
          {intents.map((intent) => (
            <Fragment key={intent.bind}>
              <span className="text-kumo-inactive">· binds</span>
              <BindingChip role="defines" name={intent.bind} kind="intent" />
              <BindingRoots value={intent.values} />
            </Fragment>
          ))}
          {admissions.length > 0 && (
            <span className="text-kumo-inactive">· admits {admissions.length} outbox message{admissions.length === 1 ? "" : "s"}</span>
          )}
          {admissions.map(([effectId, application]) => (
            <BindingRoots key={effectId} value={application.values} />
          ))}
        </>
      );
      break;
    }
    case "establish_effect_intent":
      kind = "establish intent";
      title = <BindingChip role="defines" name={step.bind} kind="intent" />;
      note = (
        <>
          <span><span className="text-kumo-inactive">←</span> captures <Mono>{shortId(step.effect_id)}</Mono> · values: {step.values.kind}</span>
          <BindingRoots value={step.values} />
        </>
      );
      break;
    case "establish_transaction_output":
      kind = "establish output";
      title = <BindingChip role="defines" name={step.bind} kind="output" />;
      note = (
        <>
          <span><span className="text-kumo-inactive">←</span> a <Mono>{shortId(step.schema)}</Mono> value · values: {step.values.kind}</span>
          <BindingRoots value={step.values} />
        </>
      );
      break;
    case "write_outbox":
      kind = "write outbox"; title = <Mono className="text-kumo-strong">{shortId(step.effect.outbox)}</Mono>;
      note = (
        <>
          <span>admits {shortId(step.effect.schema)} atomically with the commit · values: {step.values.kind}</span>
          <BindingRoots value={step.values} />
        </>
      );
      break;
    case "validate_version":
      kind = "validate version"; title = <Mono className="text-kumo-strong">{shortId(step.target.object)}</Mono>;
      note = <><span>expects</span><RefText value={step.expected} /><span>at commit ·</span>{where(step.target)}</>;
      break;
    case "bump_version":
      kind = "bump version"; title = <Mono className="text-kumo-strong">{shortId(step.target.object)}</Mono>;
      note = <><span>version + 1 atomically with the commit ·</span>{where(step.target)}</>;
      break;
    case "advance_cursor":
      kind = "advance cursor"; title = <Mono className="text-kumo-strong">{shortId(step.target.object)}.{pathText(step.field)}</Mono>;
      note = <><span className="text-kumo-inactive">←</span><RefText value={step.incoming} /><span>· {step.rule} ·</span>{where(step.target)}</>;
      break;
    case "fence":
      kind = "fence"; title = <Mono className="text-kumo-strong">{shortId(step.target.object)}.{pathText(step.field)}</Mono>;
      note = <><span>token</span><RefText value={step.token} /><span>·</span>{where(step.target)}</>;
      break;
  }
  const guard = stepRejects(step);

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
      data-selkey={selKey}
    >
      <Badge variant="neutral">{index + 1}</Badge>
      <div className="min-w-0 flex-1">
        <div className="flex flex-wrap items-center gap-1.5">
          <span className="text-[11px] uppercase tracking-wider text-kumo-subtle">{kind}</span>
          {title}
          {guard && <Badge variant="warning">commit guard</Badge>}
        </div>
        <div className="flex flex-wrap items-center gap-x-1 gap-y-0.5 text-xs text-kumo-subtle">{note}</div>
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

type ArmTone = "outline" | "warning" | "success";

const ARM_BOX: Record<ArmTone, string> = {
  outline: "border-kumo-hairline bg-kumo-elevated/30",
  warning: "border-kumo-warning/40 bg-kumo-warning-tint/60",
  success: "border-kumo-success/40 bg-kumo-success-tint/60",
};

/** One box of a set of alternatives — a decision's arm, a transaction's
 *  outcome: its label, an optional note beside it, an optional caption
 *  under it, and whatever the alternative holds. */
function ArmBox({ label, tone = "outline", note, caption, children }: {
  label: string; tone?: ArmTone; note?: ReactNode; caption?: ReactNode; children: ReactNode;
}) {
  return (
    <div className={`min-w-0 space-y-2 rounded-md border p-2 ${ARM_BOX[tone]}`}>
      <div className="flex flex-wrap items-center gap-1.5">
        <Badge variant={tone}>{label}</Badge>
        {note && <span className="text-xs text-kumo-subtle">{note}</span>}
      </div>
      {caption && <div className="text-[11px] leading-snug text-kumo-subtle">{caption}</div>}
      {children}
    </div>
  );
}

/** One arm of a decision — or the rejection block of a transaction
 *  step: its label, as the checker spells it in a step location, and
 *  its block, rendered recursively. */
function DecisionArm({ opId, op, label, block, hops, tone = "outline", caption, note, startIndex = 0 }: {
  opId: Id; op: Operation; label: string; block: OperationBlock | null; hops: StepHop[];
  tone?: ArmTone; caption?: ReactNode; note?: ReactNode;
  /** The index in `hops`' block of the arm's first step, when the arm
   *  holds the tail of that block rather than a block of its own — a
   *  transaction's committed continuation. */
  startIndex?: number;
}) {
  return (
    <ArmBox label={label} tone={tone} caption={caption} note={note}>
      {block ? (
        block.steps.length ? (
          <ProgramBlock opId={opId} op={op} block={block} hops={hops} startIndex={startIndex} nested />
        ) : (
          <Muted>empty arm</Muted>
        )
      ) : (
        <Muted>falls through</Muted>
      )}
    </ArmBox>
  );
}

/** One block of the program as a vertical sequence of step cards. The
 *  top-level block is a Kumo Flow with connectors; nested arm blocks are
 *  plain stacks, so arbitrary nesting stays legible. */
function ProgramBlock({ opId, op, block, hops, nested, startIndex = 0 }: {
  opId: Id; op: Operation; block: OperationBlock; hops: StepHop[]; nested?: boolean;
  /** The index in the enclosing block of `block.steps[0]`, so a tail
   *  rendered inside a transaction's committed arm keeps the locations
   *  the checker names its steps by. */
  startIndex?: number;
}) {
  const { model, index, expandedTx, toggleTx } = useApp();
  const effectKind = (effectId: Id): EffectKind | null => effectDef(model, index, effectId)?.effect.kind ?? null;

  const stepCtx = (location: string): DetailContext => ({ step: { op: opId, location } });

  // A transaction that can reject forks the path: the steps after it
  // are the committed path's and nothing else's (a rejected block that
  // terminates never reaches them; one that falls through rejoins them
  // and says so). They are drawn inside its committed arm, beside the
  // rejected arm, so the two outcomes read as the alternatives they
  // are — never as "commit, then reject". The block's own sequence ends
  // at that transaction.
  const forkAt = block.steps.findIndex((step) => step.kind === "transaction" && step.rejected !== undefined);
  const ownSteps = forkAt === -1 ? block.steps : block.steps.slice(0, forkAt + 1);

  const nodes: { key: string; element: ReactElement }[] = ownSteps.map((step, offset) => {
    const si = startIndex + offset;
    const ownHops: StepHop[] = [...hops, { step: si }];
    const location = locationLabel(ownHops);
    const under = (arm: Arm): StepHop[] => [...hops, { step: si, arm }];

    switch (step.kind) {
      case "transaction": {
        const tx = step.transaction;
        const expanded = expandedTx.has(location);
        const rejects = step.rejected !== undefined;

        // The transaction-local names: bound by read steps, consumed
        // only inside the body, never available after it. Listed on the
        // collapsed card so the names are visible without expanding.
        const reads = tx.steps.flatMap((inner) => (inner.kind === "read" ? [inner.bind] : []));

        // The artifacts a committed execution establishes — the
        // bindings later control consumes. Bound names lead: the flow
        // must say what each step binds without expanding it.
        const established: ReactNode[] = tx.steps.flatMap((inner, ti) => {
          switch (inner.kind) {
            case "establish_effect_intent":
              return [
                <BindingRow key={ti} kind="intent" name={inner.bind}
                  from={<span>captures <Mono>{shortId(inner.effect_id)}</Mono></span>} />,
              ];
            case "establish_transaction_output":
              return [
                <BindingRow key={ti} kind="output" name={inner.bind}
                  from={<span>a <Mono>{shortId(inner.schema)}</Mono> value</span>} />,
              ];
            case "transition":
              return Object.entries(inner.effect_intents).map(([effectId, intent]) => (
                <BindingRow key={`${ti}:${effectId}`} kind="intent" name={intent.bind}
                  from={<span>side effect <Mono>{shortId(effectId)}</Mono></span>} />
              ));
            default:
              return [];
          }
        });

        // What a commit makes available to the steps that follow —
        // the caption of the committed arm, or of the card when the
        // transaction cannot reject.
        const available = established.length ? (
          <div className="space-y-1">
            {established}
            <div>available from here on</div>
          </div>
        ) : (
          "establishes no binding"
        );

        // The committed continuation: the rest of this block, drawn
        // inside the arm with its own locations. Empty inside an arm
        // means the committed path falls through to the enclosing join.
        const continuation: OperationBlock | null = step.rejected
          ? { steps: block.steps.slice(offset + 1) }
          : null;
        const continuationEmpty = continuation !== null && continuation.steps.length === 0;

        // A rejected block that does not terminate rejoins the
        // committed path after this step; say where.
        const rejoins = step.rejected && !blockTerminates(step.rejected)
          ? continuationEmpty
            ? "falls through · rejoins the committed path at the end of this block"
            : `falls through · rejoins the committed path at step ${locationLabel([...hops, { step: si + 1 }])}`
          : null;

        return {
          key: location,
          element: (
            <StepCard selKey={`tx:${tx.id}`} detailId={tx.id} stripe={STEP_STRIPE.tx}>
              <div className="flex items-center justify-between gap-2">
                <Badge variant="neutral">transaction</Badge>
                <StatusChips obKey={`${opId}/${tx.id}`} />
              </div>
              <StepTitle>{shortId(tx.id)}</StepTitle>
              <div className="mt-1.5 flex flex-wrap items-center gap-1.5 text-xs text-kumo-subtle">
                <FactBadge fact={commitGuarantee(tx.idempotency)} />
                {tx.idempotency.kind === "deduplicated_by" && (
                  <span>by <KeyComponents value={tx.idempotency.key} /></span>
                )}
              </div>
              <div className="mt-1 flex flex-wrap items-center gap-1.5 text-xs text-kumo-subtle">
                <FactBadge fact={isolation(tx.isolation)} />
                {tx.data_model && <span>on {shortId(tx.data_model)}</span>}
                <FactBadge fact={transactionRejection(rejects)} />
              </div>
              {(tx.requirements.serializability.length > 0 || tx.requirements.ordering.length > 0) && (
                <div className="mt-1 flex flex-wrap items-center gap-1.5 text-xs text-kumo-subtle">
                  {tx.requirements.serializability.map((r, i) => (
                    <FactBadge key={`s${i}`} fact={serializabilityRequirement(r.key)} />
                  ))}
                  {tx.requirements.ordering.map((r, i) => (
                    <FactBadge key={`o${i}`} fact={orderingRequirement(r.key, r.position)} />
                  ))}
                </div>
              )}
              {!expanded && reads.length > 0 && (
                <div className="mt-2 flex flex-wrap items-center gap-1.5 text-xs text-kumo-subtle">
                  <span>binds inside</span>
                  {reads.map((r) => <BindingChip key={r} role="defines" name={r} kind="read" />)}
                  <span className="text-kumo-inactive">· transaction-local</span>
                </div>
              )}
              <Collapsible.Root open={expanded} onOpenChange={() => toggleTx(location)}>
                <Collapsible.Trigger
                  className="mt-2 flex w-full cursor-pointer items-center gap-1 text-xs text-kumo-link hover:underline"
                  onClick={(e) => e.stopPropagation()}
                >
                  <CaretRightIcon size={12} className={`transition-transform ${expanded ? "rotate-90" : ""}`} />
                  {tx.steps.length} step{tx.steps.length === 1 ? "" : "s"}
                </Collapsible.Trigger>
                <Collapsible.Panel>
                  <div className="mt-1.5 space-y-0.5 rounded-md border border-kumo-hairline bg-kumo-elevated/40 p-1">
                    {tx.steps.map((ts, ti) => (
                      <TxStepRow key={ti} step={ts} index={ti} txId={tx.id} opId={opId} />
                    ))}
                  </div>
                </Collapsible.Panel>
              </Collapsible.Root>
              {/* The two outcomes of an attempt, side by side like a
                  decision's arms — never "commit, then reject". Left,
                  what a commit establishes and where control continues;
                  right, the block control enters when a commit guard
                  rejects: nothing committed, no artifact established,
                  its steps located beneath this one as `n.rejected.m`.
                  A body with no guard cannot reject, so it gets one
                  full-width committed strip and no arm to pretend
                  otherwise. */}
              {step.rejected && continuation ? (
                <div className="mt-2 grid gap-2 sm:grid-cols-2">
                  <DecisionArm
                    opId={opId} op={op} label="committed" tone="success"
                    block={continuationEmpty ? null : continuation} hops={hops} startIndex={si + 1}
                    caption={available}
                  />
                  <DecisionArm
                    opId={opId} op={op} label="rejected" tone="warning" block={step.rejected} hops={under("rejected")}
                    caption={rejoins ? <>nothing committed · no binding above is available here<br />{rejoins}</> : "nothing committed · no binding above is available here"}
                  />
                </div>
              ) : (
                <div className="mt-2">
                  <ArmBox label="committed" tone="success" note="always commits" caption={available}>{null}</ArmBox>
                </div>
              )}
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
              </div>
              <StepTitle>{shortId(step.effect_id)}</StepTitle>
              <div className="mt-1 text-xs text-kumo-subtle">{effectSummary(model, index, step.effect_id)}</div>
              <div className="mt-1 flex flex-wrap items-center gap-1 text-xs text-kumo-subtle">
                instance: <Badge variant={step.values.kind === "deterministic" ? "info" : "warning"}>{step.values.kind}</Badge>
                <BindingRoots value={step.values} />
              </div>
              {step.bind && (
                <Bindings>
                  <BindingRow kind="result" name={step.bind}
                    from={<span>this execution's <Mono>Result</Mono></span>} />
                </Bindings>
              )}
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
              </div>
              <StepTitle><BindingChip role="uses" name={step.intent} kind="intent" /></StepTitle>
              <div className="mt-1 text-xs text-kumo-subtle">{eff ? effectSummary(model, index, eff) : "unresolved intent"}</div>
              {step.bind && (
                <Bindings>
                  <BindingRow kind="result" name={step.bind}
                    from={<span>this execution's <Mono>Result</Mono></span>} />
                </Bindings>
              )}
            </StepCard>
          ),
        };
      }

      case "execute_effect_async":
        return {
          key: location,
          element: (
            <StepCard selKey={`fx:${location}:${step.effect_id}`} detailId={step.effect_id} stripe={STEP_STRIPE.effect}>
              <div className="flex flex-wrap items-center gap-1.5">
                <Badge variant="neutral">launch async</Badge>
                <EffectKindBadge kind={step.effect.kind} />
              </div>
              <StepTitle>{shortId(step.effect_id)}</StepTitle>
              <div className="mt-1 text-xs text-kumo-subtle">{effectSummary(model, index, step.effect_id)}</div>
              <div className="mt-1 flex flex-wrap items-center gap-1 text-xs text-kumo-subtle">
                instance: <Badge variant={step.values.kind === "deterministic" ? "info" : "warning"}>{step.values.kind}</Badge>
                <BindingRoots value={step.values} />
                <span className="ml-1">· control does not wait</span>
              </div>
              <Bindings>
                <BindingRow kind="handle" name={step.handle} from={<span>this launch — no result until a barrier</span>} />
              </Bindings>
            </StepCard>
          ),
        };

      case "execute_effect_intent_async": {
        const intent = index.get(step.intent);
        const eff = intent?.kind === "intent" ? intent.effect : null;
        const via = intent?.kind === "intent" && intent.via !== undefined;
        const effKind = eff ? effectKind(eff) : null;
        return {
          key: location,
          element: (
            <StepCard selKey={`fi:${location}:${step.intent}`} detailId={step.intent} stripe={STEP_STRIPE.intent} dashed>
              <div className="flex flex-wrap items-center gap-1.5">
                <Badge variant="neutral">launch intent async</Badge>
                <EffectKindBadge kind={effKind} />
                {via && <Badge variant="info">via transition</Badge>}
              </div>
              <StepTitle><BindingChip role="uses" name={step.intent} kind="intent" /></StepTitle>
              <div className="mt-1 text-xs text-kumo-subtle">{eff ? effectSummary(model, index, eff) : "unresolved intent"}</div>
              <div className="mt-1 text-xs text-kumo-subtle">control does not wait</div>
              <Bindings>
                <BindingRow kind="handle" name={step.handle} from={<span>this launch — no result until a barrier</span>} />
              </Bindings>
            </StepCard>
          ),
        };
      }

      case "join_all":
        return {
          key: location,
          element: (
            <StepCard selKey={`step:${location}`} detailId={opId} ctx={stepCtx(location)} stripe={STEP_STRIPE.sync}>
              <div className="flex flex-wrap items-center gap-1.5">
                <Badge variant="neutral">join_all</Badge>
                <Badge variant="outline">step {location}</Badge>
                <Badge variant="blue">waits for all {step.handles.length}</Badge>
              </div>
              <Bindings>
                {step.handles.map((entry) =>
                  entry.bind ? (
                    <BindingRow key={entry.handle} kind="result" name={entry.bind}
                      from={<span className="inline-flex items-center gap-1">completion of <BindingChip role="uses" name={entry.handle} kind="handle" /></span>} />
                  ) : (
                    <div key={entry.handle} className="flex flex-wrap items-center gap-1.5 px-2 text-xs">
                      <BindingChip role="uses" name={entry.handle} kind="handle" />
                      <span className="text-kumo-inactive">awaited · no result bound</span>
                    </div>
                  ),
                )}
              </Bindings>
              <div className="mt-1 text-xs text-kumo-subtle">no order among the joined effects</div>
            </StepCard>
          ),
        };

      case "race":
        return {
          key: location,
          element: (
            <StepCard selKey={`step:${location}`} detailId={opId} ctx={stepCtx(location)} stripe={STEP_STRIPE.sync}>
              <div className="flex flex-wrap items-center gap-1.5">
                <Badge variant="neutral">race</Badge>
                <Badge variant="outline">step {location}</Badge>
                <Badge variant="blue">first of {step.handles.length}</Badge>
              </div>
              <div className="mt-1.5 flex flex-wrap items-center gap-1.5">
                {step.handles.map((h) => (
                  <BindingChip key={h} role="uses" name={h} kind="handle" />
                ))}
              </div>
              {step.bind && (
                <Bindings>
                  <BindingRow kind="result" name={step.bind}
                    from={<span>the winner's <Mono>Result</Mono> — first completion, whichever candidate</span>} />
                </Bindings>
              )}
              <div className="mt-1 text-xs text-kumo-subtle">first completion, not first success · losers are not cancelled</div>
            </StepCard>
          ),
        };

      case "match_result":
        return {
          key: location,
          element: (
            <StepCard selKey={`step:${location}`} detailId={opId} ctx={stepCtx(location)} stripe={STEP_STRIPE.decision}>
              <div className="flex flex-wrap items-center gap-1.5">
                <Badge variant="neutral">match result</Badge>
                <Badge variant="outline">step {location}</Badge>
              </div>
              <StepTitle><BindingChip role="uses" name={step.result} kind="result" /></StepTitle>
              {/* One arm per outcome the contract declares: ok, then an
                  arm per error class, labelled as the checker locates
                  its steps. */}
              <div className="mt-2 grid gap-2 sm:grid-cols-2">
                <DecisionArm opId={opId} op={op} label="ok" block={step.ok} hops={under("ok")} />
                {Object.entries(step.errors).map(([error, arm]) => (
                  <DecisionArm key={error} opId={opId} op={op} label={errArm(error)} block={arm} hops={under(errArm(error))} />
                ))}
              </div>
              {Object.keys(step.errors).length === 0 && (
                <div className="mt-1 text-xs text-kumo-subtle">the contract declares no error class</div>
              )}
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
                <span className="break-words font-normal text-kumo-subtle"><ConditionView condition={step.condition} /></span>
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
                <Badge variant={step.outcome.kind === "ok" ? "success" : "warning"}>
                  {step.outcome.kind === "ok" ? "ok" : errArm(step.outcome.error)}
                </Badge>
              </div>
              <StepTitle>{shortId(step.request)}</StepTitle>
              <div className="mt-1 flex flex-wrap items-center gap-1 text-xs text-kumo-subtle">
                payload: <Badge variant={step.outcome.values.kind === "deterministic" ? "info" : "warning"}>{step.outcome.values.kind}</Badge>
                <BindingRoots value={step.outcome.values} />
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
// Bindings
// ---------------------------------------------------------------------------

const BINDING_KINDS: BindingKind[] = ["read", "output", "intent", "result", "handle"];

/** Where each kind of binding is available, in a few words: the
 *  legend's caption beside the kind's coloured tag. */
const BINDING_SCOPE: Record<BindingKind, string> = {
  read: "transaction-local",
  output: "program · committed path",
  intent: "program · committed path",
  result: "program · from the step on",
  handle: "program · from the launch on",
};

/** A program location as a link into the flow. */
function LocationLink({ onClick, children }: { onClick: () => void; children: ReactNode }) {
  return (
    <button
      type="button"
      className="cursor-pointer whitespace-nowrap font-mono text-[12px] text-kumo-link hover:underline"
      onClick={(e) => {
        e.stopPropagation();
        onClick();
      }}
    >
      {children}
    </button>
  );
}

/** Every name the program binds, in program order: its defining chip,
 *  kind, scope, where it is bound, and every step that uses it — each
 *  location a link into the flow. The cross-reference the chips in the
 *  flow are read against. */
function BindingsTable({ id }: { id: Id }) {
  const { bindings } = useApp();
  const { toProducer, toUse } = useProgramNavigation();
  const own = bindings.byOp.get(id);
  const defs = own ? [...own.defs.values()] : [];

  if (!defs.length) return <Muted>the program binds no names</Muted>;

  return (
    <Table>
      <Table.Header variant="compact">
        <Table.Row>
          <Table.Head>binding</Table.Head>
          <Table.Head>kind</Table.Head>
          <Table.Head>scope</Table.Head>
          <Table.Head>bound at</Table.Head>
          <Table.Head>used at</Table.Head>
        </Table.Row>
      </Table.Header>
      <Table.Body>
        {defs.map((def) => {
          const uses = own?.uses.get(def.name) ?? [];
          return (
            <Table.Row key={def.name}>
              <Table.Cell className="whitespace-nowrap"><BindingChip role="defines" name={def.name} kind={def.kind} /></Table.Cell>
              <Table.Cell className="whitespace-nowrap">
                <Tooltip content={bindingKind(def.kind).summary} render={<span className="cursor-help text-kumo-default">{def.kind}</span>} />
              </Table.Cell>
              <Table.Cell className="whitespace-nowrap">
                {def.scope === "transaction" && def.transaction !== undefined ? (
                  <span className="inline-flex items-center gap-1">
                    <span className="text-kumo-subtle">transaction</span>
                    <IdLink id={def.transaction}>{shortId(def.transaction)}</IdLink>
                  </span>
                ) : (
                  <span className="text-kumo-subtle">program</span>
                )}
              </Table.Cell>
              <Table.Cell className="whitespace-nowrap">
                <span className="inline-flex flex-wrap items-center gap-1.5">
                  <LocationLink onClick={() => toProducer(def)}>{definedAtLabel(def)}</LocationLink>
                  <span className="text-xs text-kumo-inactive">{def.producer}</span>
                </span>
              </Table.Cell>
              <Table.Cell>
                {uses.length ? (
                  <span className="inline-flex flex-wrap items-center gap-x-1 gap-y-0.5">
                    {uses.map((u, i) => (
                      <Fragment key={i}>
                        {i > 0 && <span className="text-kumo-inactive">,</span>}
                        <span className="inline-flex items-center gap-1">
                          <LocationLink onClick={() => toUse(id, u)}>{usedAtLabel(u)}</LocationLink>
                          <span className="text-[11px] text-kumo-inactive">{u.how}</span>
                        </span>
                      </Fragment>
                    ))}
                  </span>
                ) : (
                  <span className="text-kumo-inactive">unused</span>
                )}
              </Table.Cell>
            </Table.Row>
          );
        })}
      </Table.Body>
    </Table>
  );
}

/** The five kinds, each in its colour, with where it is available. */
function BindingsLegend() {
  return (
    <div className="flex flex-wrap items-center gap-x-4 gap-y-1 border-b border-kumo-hairline px-3 py-2 text-xs text-kumo-subtle">
      {BINDING_KINDS.map((kind) => (
        <span key={kind} className="inline-flex items-center gap-1.5">
          <BindingKindTag kind={kind} legend />
          <span>{BINDING_SCOPE[kind]}</span>
        </span>
      ))}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Requirements and inputs
// ---------------------------------------------------------------------------

/** Every requirement the operation carries: the serializability and
 *  ordering requirements each of its transactions declares — rows of
 *  the transaction's own, in program order, as the checker enumerates
 *  them — then the operation's idempotency and recoverability. A
 *  transaction row's verdicts are the obligations anchored to that
 *  transaction and its requirement index. */
function RequirementsTable({ id, op }: { id: Id; op: Operation }) {
  const { obligations, selection, select, consistency } = useApp();
  const reqs = op.requirements;

  // A transaction row ends with what its argument rests on — the
  // closure and the route, or the guard — so the shape of the proof is
  // readable before the row is opened.
  const argument = (tx: Id, prop: RequirementKind, i: number): ReactNode => {
    const proof = consistency.proofForRequirement(id, tx, prop, i);
    return proof ? <span className="text-xs text-kumo-inactive">{proofSummary(proof)}</span> : null;
  };

  const rows: { prop: RequirementKind; i: number; tx?: Id; label: string; declares: ReactNode }[] = [];
  for (const tx of operationTransactions(op)) {
    tx.requirements.serializability.forEach((r, i) =>
      rows.push({
        prop: "transaction_serializability",
        i,
        tx: tx.id,
        label: "serializability",
        declares: (
          <>
            <span className="text-xs text-kumo-subtle">key</span>
            <RefText value={r.key} />
            {argument(tx.id, "transaction_serializability", i)}
          </>
        ),
      }));
    tx.requirements.ordering.forEach((r, i) =>
      rows.push({
        prop: "transaction_ordering",
        i,
        tx: tx.id,
        label: "ordering",
        declares: (
          <>
            <span className="text-xs text-kumo-subtle">key</span>
            <RefText value={r.key} />
            <span className="text-xs text-kumo-subtle">position</span>
            <RefText value={r.position} />
            {argument(tx.id, "transaction_ordering", i)}
          </>
        ),
      }));
  }
  reqs.idempotency.forEach((r, i) =>
    rows.push({
      prop: "idempotency",
      i,
      label: "idempotency",
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
      label: "recoverability",
      declares: (
        <>
          <KeyComponents value={r.key} />
          <Badge variant={r.completion === "guaranteed" ? "success" : "neutral"}>{r.completion} completion</Badge>
        </>
      ),
    }));

  if (!rows.length) return <Muted>the operation and its transactions declare no requirements</Muted>;

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
          const key = requirementKey(row.prop, row.i, row.tx);
          const obs = row.tx !== undefined
            ? (obligations.get(`${id}/${row.tx}`) ?? []).filter(
                (ob) => ob.subject.kind === "transaction" && ob.subject.transaction === row.tx &&
                  ob.subject.requirement === row.i && propertyMatchesRequirement(ob.property, row.prop))
            : (obligations.get(id) ?? []).filter(
                (ob) => ob.subject.kind === "operation" && ob.subject.requirement === row.i &&
                  propertyMatchesRequirement(ob.property, row.prop));
          const status = obs.length ? worstStatus(obs) : null;
          return (
            <Table.Row
              key={key}
              className={selectableRow(selection === key)}
              onClick={() => select(key, { id, ctx: { req: { prop: row.prop, index: row.i, transaction: row.tx } } })}
            >
              <Table.Cell className="whitespace-nowrap">
                <span className="font-medium text-kumo-strong">{row.label}</span>
                {row.tx !== undefined && (
                  <span className="ml-1.5 text-kumo-subtle">· <Mono>{row.tx}</Mono></span>
                )}
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
function Realization({ opId, inputId, kind }: { opId: Id; inputId: Id; kind: "request" | "subscription" | "outbox" }) {
  const { model } = useApp();

  if (kind === "outbox") {
    const runtime = model.runtime?.outboxes?.[opId]?.[inputId];
    if (!runtime) {
      return (
        <>
          <FactBadge fact={noRuntimeDeclared()} />
          <FactBadge fact={intrinsicRedrive()} />
        </>
      );
    }
    const pool = model.runtime?.execution_pools?.[runtime.dispatch.pool];
    return (
      <>
        <FactBadge fact={intrinsicRedrive()} />
        <Badge variant="neutral">
          {runtime.partitioning.kind === "keyed" ? "keyed partitions" : "unpartitioned"}
        </Badge>
        <Badge variant="neutral">{`ordering: ${runtime.ordering}`}</Badge>
        <FactBadge fact={outboxRouting(runtime.dispatch.routing?.key)} />
        {runtime.dispatch.routing && (
          <FactBadge fact={memberAssignment(runtime.dispatch.routing.member_assignment)} />
        )}
        {runtime.dispatch.batching && (
          <Badge variant="neutral">{`batching: ${runtime.dispatch.batching.ordering}`}</Badge>
        )}
        {pool && <FactBadge fact={memberConcurrency(pool.member_concurrency)} />}
      </>
    );
  }

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

/** A result contract inline: the ok schema, then every error class as
 *  `class: schema [disposition]`. */
function ResultContractInline({ result }: { result: ResultType }) {
  const classes = Object.entries(result.errors);
  return (
    <span className="inline-flex flex-wrap items-center gap-1 text-xs text-kumo-subtle">
      <Mono>Result&lt;</Mono>
      <IdLink id={result.ok}>{shortId(result.ok)}</IdLink>
      {classes.map(([cls, c]) => (
        <span key={cls} className="inline-flex items-center gap-1">
          <Mono>,</Mono>
          <Mono className="text-kumo-strong">{cls}:</Mono>
          <IdLink id={c.schema}>{shortId(c.schema)}</IdLink>
          {c.disposition !== "unspecified" && <Mono>[{c.disposition}]</Mono>}
        </span>
      ))}
      <Mono>&gt;</Mono>
    </span>
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
                ) : input.kind === "subscription" ? (
                  <span className="inline-flex items-center gap-1.5">
                    <span className="text-xs text-kumo-subtle">topic</span>
                    <IdLink id={input.topic}>{shortId(input.topic)}</IdLink>
                  </span>
                ) : (
                  <span className="inline-flex items-center gap-1.5">
                    <span className="text-xs text-kumo-subtle">outbox</span>
                    <IdLink id={input.outbox}>{shortId(input.outbox)}</IdLink>
                  </span>
                )}
              </Table.Cell>
              <Table.Cell>
                <span className="flex flex-wrap items-center gap-1.5">
                  {input.kind === "request" ? (
                    <>
                      <FactBadge fact={requestIdentity(input.identity)} />
                      <ResultContractInline result={input.result} />
                    </>
                  ) : input.kind === "subscription" ? (
                    <>
                      <Badge variant="neutral">
                        {input.messages.kind === "all"
                          ? "all topic messages"
                          : input.messages.schemas.map(shortId).join(", ")}
                      </Badge>
                      {input.acknowledge_on_success != null && (
                        <Badge variant="outline">
                          {input.acknowledge_on_success ? "ack on success" : "no ack on success"}
                        </Badge>
                      )}
                    </>
                  ) : (
                    <>
                      <Badge variant="neutral">all outbox messages</Badge>
                      <Badge variant="outline">exclusive consumer</Badge>
                    </>
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
  const { model, navigateTo, obligations, selection, bindings } = useApp();
  const op = model.operations[id];

  // A selection made from elsewhere — a binding's chip, the bindings
  // table, the detail panel — must be visible to have happened: the
  // selected card is brought into view. A card already on screen does
  // not move.
  useEffect(() => {
    if (!selection) return;
    const card = document.querySelector<HTMLElement>(`[data-selkey="${CSS.escape(selection)}"]`);
    card?.scrollIntoView({ block: "nearest", behavior: "smooth" });
  }, [selection]);

  if (!op) {
    return (
      <div className="flex h-full items-center justify-center">
        <Empty size="sm" icon={<GraphIcon size={32} className="text-kumo-inactive" />} title={`unknown operation ${id}`} />
      </div>
    );
  }

  const reqs = op.requirements;
  const transactions = operationTransactions(op);
  const requirementCount =
    transactions.reduce((n, tx) => n + tx.requirements.serializability.length + tx.requirements.ordering.length, 0) +
    reqs.idempotency.length + reqs.recoverability.length;
  const inputCount = Object.keys(op.inputs).length;
  const transactionCount = transactions.length;
  const stepCount = walkProgram(op.program).length;
  const bindingCount = bindings.byOp.get(id)?.defs.size ?? 0;
  const machines = [...new Set(
    transactions.flatMap((tx) => tx.steps.flatMap((s) => (s.kind === "transition" ? [s.machine] : []))),
  )];

  return (
    <div className="h-full overflow-auto">
      <div className="mx-auto max-w-[1240px] space-y-6 p-6">
        <header className="space-y-4 border-b border-kumo-hairline pb-5">
          <div className="flex flex-wrap items-center gap-x-4 gap-y-2">
            <Text variant="heading" size="lg" as="h1">{shortId(id)}</Text>
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

        <SectionCard title="Requirements" count={requirementCount} hint="proof obligations on each transaction's committed history and on every invocation">
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
          hint="the operation's one causal control structure — a decision's arms and a transaction's rejected block are alternatives, and every path ends at a terminal"
          bodyClassName="@container space-y-4 p-4"
        >
          {op.program.steps.length ? (
            <ProgramBlock opId={id} op={op} block={op.program} hops={[]} />
          ) : (
            <Empty size="sm" title="operation declares no program steps" />
          )}
        </SectionCard>

        <SectionCard
          title="Bindings"
          count={bindingCount}
          hint="every name a step introduces for later steps — ≔ where it is bound, ↑ where it is used"
        >
          <BindingsLegend />
          <div className="overflow-x-auto">
            <BindingsTable id={id} />
          </div>
        </SectionCard>
      </div>
    </div>
  );
}
