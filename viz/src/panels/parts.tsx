import { Badge } from "@cloudflare/kumo/components/badge";
import { LayerCard } from "@cloudflare/kumo/components/layer-card";
import { Tooltip } from "@cloudflare/kumo/components/tooltip";

import { bindingKind, type Explanation } from "../lib/explain";
import { Collapsible } from "@cloudflare/kumo/components/collapsible";
import { Text } from "@cloudflare/kumo/components/text";
import { Fragment, useState, type ReactNode } from "react";

import {
  definedAtLabel, producerSelection, refBinding, stepSelection,
  type BindingDef, type BindingKind, type BindingUse, type ProgramSelection,
} from "../lib/bindings";
import { splitCitations } from "../lib/citations";
import { pathText, shortId } from "../lib/ids";
import { walkProgram } from "../lib/index";
import { STATUS_GLYPH, statusCounts } from "../lib/obligations";
import { hashes } from "../lib/route";
import { typeText } from "../lib/text";
import { useApp, useObligationsAt } from "../state/AppState";
import type {
  Condition,
  Derivation,
  Id,
  IdempotencyKey,
  ResultVariant,
  SelectorPredicate,
  SelectorValue,
  TypeRef,
  ValueRef,
} from "../types/model";
import type { Status } from "../types/report";

/** A clickable model id that opens its detail. */
export function IdLink({ id, children }: { id: string; children?: ReactNode }) {
  const { openDetail } = useApp();
  return (
    <button
      type="button"
      className="cursor-pointer break-all text-left font-mono text-[12px] text-kumo-link hover:underline"
      onClick={() => openDetail(id)}
    >
      {children ?? id}
    </button>
  );
}

/** Prose with the declarations it names made followable.
 *
 *  A verdict's reasoning already names its facts by id — that is what
 *  §61 asks a proof to record. Rendering those names as links is the
 *  difference between being told a proof rests on `pool.notifier_workers`
 *  and being able to go and look at it. */
export function CitedText({ text }: { text: string }) {
  const { knownIds } = useApp();
  const runs = splitCitations(text, knownIds);
  return (
    <>
      {runs.map((run, i) =>
        run.kind === "id" ? (
          <IdLink key={i} id={run.id}>
            {shortId(run.id)}
          </IdLink>
        ) : (
          <span key={i}>{run.text}</span>
        ),
      )}
    </>
  );
}

/** A navigation action into another view, optionally applying a selection there. */
export function NavLink({ hash, selection, children }: { hash: string; selection?: string; children: ReactNode }) {
  const { navigateTo } = useApp();
  return (
    <button
      type="button"
      className="cursor-pointer text-left text-sm text-kumo-link hover:underline"
      onClick={() => navigateTo(hash, selection)}
    >
      {children}
    </button>
  );
}

/** A titled, collapsible block of the panel. Uncontrolled by default;
 *  a parent that must open it on the reader's behalf — a selection made
 *  in a drawing that lands inside it — passes `open` and `onOpenChange`. */
export function Section({
  title, count, children, defaultOpen = true, open: controlled, onOpenChange,
}: {
  title: string; count?: number; children: ReactNode; defaultOpen?: boolean;
  open?: boolean; onOpenChange?: (open: boolean) => void;
}) {
  const [own, setOwn] = useState(defaultOpen);
  const open = controlled ?? own;
  const setOpen = (next: boolean) => {
    if (controlled === undefined) setOwn(next);
    onOpenChange?.(next);
  };
  return (
    <Collapsible.Root open={open} onOpenChange={setOpen} className="border-t border-kumo-hairline pt-2">
      <Collapsible.DefaultTrigger className="w-full text-xs font-semibold uppercase tracking-wider text-kumo-subtle">
        <span className="flex items-center gap-2">
          {title}
          {count !== undefined && <Badge variant="neutral">{count}</Badge>}
        </span>
      </Collapsible.DefaultTrigger>
      <Collapsible.DefaultPanel>
        <div className="space-y-2 pb-1 pt-1">{children}</div>
      </Collapsible.DefaultPanel>
    </Collapsible.Root>
  );
}

/** A titled page section: a heading row over a single Kumo surface. The
 *  surface carries no header of its own, so a table inside it has exactly
 *  one header — its column row. */
export function SectionCard({
  title, count, hint, aside, bodyClassName, children,
}: { title: string; count?: number; hint?: string; aside?: ReactNode; bodyClassName?: string; children: ReactNode }) {
  return (
    <section className="space-y-2">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
        <h2 className="text-sm font-semibold text-kumo-default">{title}</h2>
        {count !== undefined && <Badge variant="neutral">{count}</Badge>}
        {hint && <span className="text-xs text-kumo-inactive">{hint}</span>}
        {aside && <span className="ml-auto flex items-center gap-2">{aside}</span>}
      </div>
      <LayerCard className={bodyClassName}>{children}</LayerCard>
    </section>
  );
}

/** One label/value pair in a page header's fact strip. */
export function Fact({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="min-w-0">
      <dt className="text-[11px] font-medium uppercase tracking-wider text-kumo-inactive">{label}</dt>
      <dd className="mt-0.5 flex flex-wrap items-center gap-1.5 text-sm text-kumo-default">{children}</dd>
    </div>
  );
}

/** Row classes for single-select tables. Kumo's zebra striping stays as a
 *  reading aid, so the selected row is marked by a brand accent bar and
 *  tint rather than the same tint the even rows already carry. */
export function selectableRow(selected: boolean): string {
  return selected
    ? "cursor-pointer [&>td]:bg-kumo-brand/10 [&>td:first-child]:shadow-[inset_3px_0_0_0_var(--color-kumo-brand)]"
    : "cursor-pointer [&:hover>td]:bg-kumo-contrast/5";
}

/** A declared fact as a badge, with its implication as a tooltip. */
export function FactBadge({ fact }: { fact: Explanation }) {
  return (
    <Tooltip
      content={fact.summary}
      render={
        <span className="inline-flex">
          <Badge variant={fact.tone}>{fact.label}</Badge>
        </span>
      }
    />
  );
}

/** A declared fact as a block: badge, the declaration itself, and what it
 *  implies in plain words. */
export function FactNote({ fact, children }: { fact: Explanation; children?: ReactNode }) {
  return (
    <div className="space-y-1 rounded-md border border-kumo-hairline bg-kumo-elevated/40 p-2.5">
      <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
        <Badge variant={fact.tone}>{fact.label}</Badge>
        {children}
      </div>
      <p className="text-xs leading-relaxed text-kumo-subtle">{fact.summary}</p>
    </div>
  );
}

export function KeyValue({ rows }: { rows: [string, ReactNode | null | undefined][] }) {
  return (
    <dl className="grid grid-cols-[max-content_1fr] gap-x-4 gap-y-1.5 text-sm">
      {rows
        .filter(([, v]) => v !== null && v !== undefined)
        .map(([k, v]) => (
          <div key={k} className="contents">
            <dt className="text-xs uppercase tracking-wide text-kumo-inactive">{k}</dt>
            <dd className="min-w-0 break-words text-kumo-default">{v}</dd>
          </div>
        ))}
    </dl>
  );
}

export function List({ items }: { items: ReactNode[] }) {
  return (
    <ul className="divide-y divide-kumo-hairline rounded-md border border-kumo-hairline bg-kumo-elevated/40 text-sm">
      {items.map((item, i) => (
        <li key={i} className="px-2.5 py-1.5">
          {item}
        </li>
      ))}
    </ul>
  );
}

export function Mono({ children, className }: { children: ReactNode; className?: string }) {
  return <span className={`font-mono text-[12px] ${className ?? ""}`}>{children}</span>;
}

export function Muted({ children }: { children: ReactNode }) {
  return (
    <Text variant="secondary" size="sm" as="span">
      {children}
    </Text>
  );
}

export function Tag({ children, variant = "neutral" }: { children: ReactNode; variant?: "neutral" | "warning" | "info" | "success" | "purple" | "blue" | "orange" }) {
  return <Badge variant={variant}>{children}</Badge>;
}

export function StatusBadge({ status }: { status: Status }) {
  const variant = status === "proven" ? "success" : status === "disproven" ? "error" : "warning";
  return (
    <Badge variant={variant} appearance="dot">
      {`${STATUS_GLYPH[status]} ${status}`}
    </Badge>
  );
}

/** Compact per-status counts for the obligations anchored to an entity. */
export function StatusChips({ obKey }: { obKey: string }) {
  const obs = useObligationsAt(obKey);
  if (!obs.length) return null;
  const counts = statusCounts(obs);
  return (
    <span className="inline-flex items-center gap-1">
      {(["disproven", "unknown", "proven"] as const).map((s) =>
        counts[s] ? (
          <Badge key={s} variant={s === "proven" ? "success" : s === "disproven" ? "error" : "warning"} appearance="dot">
            {`${STATUS_GLYPH[s]}${counts[s]}`}
          </Badge>
        ) : null,
      )}
    </span>
  );
}

// ---------------------------------------------------------------------
// Bindings
// ---------------------------------------------------------------------

/** Moves the reader to a card of an operation's program. On that
 *  operation's page the card is selected in place; from anywhere else
 *  the page is opened with the selection pending. A transaction body is
 *  expanded first when the target sits inside it, so the row the panel
 *  describes is on screen. */
export function useProgramNavigation() {
  const { model, route, select, navigateTo, expandedTx, toggleTx } = useApp();

  const go = (op: Id, sel: ProgramSelection, reveal?: string) => {
    if (reveal !== undefined && !expandedTx.has(reveal)) toggleTx(reveal);
    select(sel.key, { id: sel.id, ctx: sel.ctx });
    if (!(route.view === "op" && route.id === op)) navigateTo(hashes.op(op), sel.key);
  };
  const locate = (op: Id, location: string) =>
    walkProgram(model.operations[op]?.program ?? { steps: [] }).find((s) => s.location === location) ?? null;

  return {
    /** The card that binds a name. */
    toProducer: (def: BindingDef) =>
      go(def.op, producerSelection(def), def.txStep !== undefined ? def.location : undefined),
    /** The card of a program step, by location. */
    toStep: (op: Id, location: string) => {
      const located = locate(op, location);
      if (located) go(op, stepSelection(op, location, located.step));
    },
    /** The step that consumes a binding: the transaction step row when
     *  the use is inside a transaction body, else the step's card. */
    toUse: (op: Id, use: BindingUse) => {
      const located = locate(op, use.location);
      if (!located) return;
      if (use.txStep !== undefined && located.step.kind === "transaction") {
        const tx = located.step.transaction.id;
        go(op, { key: `ts:${tx}:${use.txStep}`, id: tx, ctx: { txStep: { op, tx, index: use.txStep } } }, use.location);
      } else {
        go(op, stepSelection(op, use.location, located.step));
      }
    },
  };
}

/** The kind of a binding as a small coloured tag. On its own — in a
 *  legend — it explains the kind on hover. */
export function BindingKindTag({ kind, legend }: { kind: BindingKind; legend?: boolean }) {
  const tag = <span className={`binding-kind binding-${kind} ${legend ? "legend" : ""}`}>{kind}</span>;
  if (!legend) return tag;
  const fact = bindingKind(kind);
  return <Tooltip content={fact.summary} render={<span className="inline-flex">{tag}</span>} />;
}

/** One binding, named the way the program names it, in the colour of its
 *  kind.
 *
 *  `defines` is rendered at the step that introduces the name: a filled
 *  chip, `≔ name`, tagged with the kind, that opens the binding's detail.
 *  `uses` is rendered wherever a later step consumes it: an outlined
 *  chip, `↑ name`, whose tooltip says where and by what the name was
 *  bound, and which selects that producing card. A result reference
 *  also says which arm's payload it reads. */
export function BindingChip({ name, kind, role, arm }: {
  name: Id; kind: BindingKind; role: "defines" | "uses"; arm?: ResultVariant;
}) {
  const { bindings, openDetail } = useApp();
  const { toProducer } = useProgramNavigation();
  const def = bindings.defs.get(name);
  const k = def?.kind ?? kind;

  let tip: string;
  let onClick: () => void;
  if (role === "defines") {
    const fact = bindingKind(k);
    tip = `${fact.label} — ${fact.summary}`;
    if (k === "read" && def?.transaction) tip += ` Transaction-local: never available outside ${def.transaction}.`;
    onClick = () => openDetail(name);
  } else {
    tip = def
      ? `bound at step ${definedAtLabel(def)} by ${def.producer}`
      : "not bound by any step of this model";
    onClick = def ? () => toProducer(def) : () => openDetail(name);
  }

  return (
    <Tooltip
      content={tip}
      render={
        <button
          type="button"
          className={`binding-chip ${role} binding-${k}`}
          onClick={(e) => {
            e.stopPropagation();
            onClick();
          }}
          // A chip inside a card that activates on Enter or Space must
          // not activate the card too.
          onKeyDown={(e) => e.stopPropagation()}
        >
          <span className="glyph" aria-hidden="true">{role === "defines" ? "≔" : "↑"}</span>
          {/* A long name breaks after a dot before it breaks anywhere. */}
          <span className="name">
            {name.split(".").map((segment, i) => (
              <Fragment key={i}>
                {i > 0 && <>.<wbr /></>}
                {segment}
              </Fragment>
            ))}
          </span>
          {role === "defines" && <BindingKindTag kind={k} />}
          {role === "uses" && arm && <span className="binding-arm">{arm}</span>}
        </button>
      }
    />
  );
}

/** A value reference.
 *
 *  A reference to a binding is the binding's using chip followed by the
 *  field path read off it — the name is drawn exactly as it is where it
 *  was bound, and the chip leads there. An input reference is tagged as
 *  one, so it is never mistaken for a bound name; other sources — an
 *  effect's own fields, a machine subject — keep the `kind(source).path`
 *  spelling.
 *
 *  A reference holds no spaces, so left to itself it is one unbreakable
 *  word: in a panel this narrow it overflowed its row and the tail was
 *  simply clipped. The seams — after the source, before the path — are
 *  where a reader would break the expression anyway, so they are marked
 *  as the places to break first; `break-words` catches a segment that
 *  still does not fit. */
export function RefText({ value }: { value: ValueRef }) {
  const binding = refBinding(value);
  if (binding) {
    return (
      <Mono className="break-words">
        <BindingChip role="uses" name={binding.name} kind={binding.kind} arm={binding.arm} />
        <wbr />
        <span className="text-kumo-subtle">.{pathText(value.path)}</span>
      </Mono>
    );
  }
  if (value.source.kind === "input") {
    return (
      <Mono className="break-words">
        <span className="ref-tag">input</span>
        <IdLink id={value.source.id}>{shortId(value.source.id)}</IdLink>
        <wbr />
        <span className="text-kumo-subtle">.{pathText(value.path)}</span>
      </Mono>
    );
  }
  return (
    <Mono className="break-words">
      <span className="text-kumo-subtle">{value.source.kind}(</span>
      <wbr />
      <IdLink id={value.source.id}>{shortId(value.source.id)}</IdLink>
      <span className="text-kumo-subtle">).</span>
      <wbr />
      <span className="text-kumo-subtle">{pathText(value.path)}</span>
    </Mono>
  );
}

/** The binding roots of a derivation — the part of a provenance a reader
 *  tracks by name — as using chips with the paths read off them, and a
 *  count of the remaining roots (inputs, an effect's own fields). Null
 *  when no root is a binding, so a step that consumes only inputs stays
 *  as quiet as before. */
export function BindingRoots({ value }: { value: Derivation }) {
  if (value.kind !== "deterministic") return null;
  const bound = value.from.filter((r) => refBinding(r) !== null);
  if (!bound.length) return null;
  const others = value.from.length - bound.length;
  return (
    <span className="inline-flex flex-wrap items-center gap-1">
      <span className="text-kumo-inactive">←</span>
      {bound.map((r, i) => <RefText key={i} value={r} />)}
      {others > 0 && <span className="text-xs text-kumo-inactive">+ {others} other root{others === 1 ? "" : "s"}</span>}
    </span>
  );
}

export function KeyComponents({ value }: { value: IdempotencyKey }) {
  if (!value.components.length) return <Muted>empty key</Muted>;
  return (
    <span className="inline-flex flex-wrap items-center gap-1">
      {value.components.map((c, i) => (
        <span key={i} className="inline-flex items-center gap-1">
          {i > 0 && <span className="text-kumo-inactive">+</span>}
          <RefText value={c} />
        </span>
      ))}
    </span>
  );
}

export function DerivationView({ value }: { value: Derivation }) {
  if (value.kind !== "deterministic") {
    return <Tag variant="warning">unspecified provenance</Tag>;
  }
  return (
    <div className="space-y-1.5">
      <Tag variant="info">deterministic</Tag>
      <List items={value.from.map((ref, i) => <RefText key={i} value={ref} />)} />
    </div>
  );
}

function SelectorValueView({ value }: { value: SelectorValue }) {
  return value.kind === "value"
    ? <RefText value={value.value} />
    : <Mono className="text-kumo-subtle">{JSON.stringify(value.value.value)}</Mono>;
}

/** A selector predicate, its values rendered as references — so a
 *  predicate over a bound name shows the binding's chip. */
export function PredicateView({ predicate }: { predicate: SelectorPredicate }) {
  switch (predicate.kind) {
    case "all":
      return <Mono className="text-kumo-subtle">all instances</Mono>;
    case "eq":
      return (
        <span className="inline-flex flex-wrap items-center gap-1">
          <Mono className="text-kumo-subtle">{pathText(predicate.field)} =</Mono>
          <SelectorValueView value={predicate.value} />
        </span>
      );
    case "and":
      return (
        <span className="inline-flex flex-wrap items-center gap-1">
          {predicate.predicates.map((p, i) => (
            <Fragment key={i}>
              {i > 0 && <Mono className="text-kumo-inactive">∧</Mono>}
              <PredicateView predicate={p} />
            </Fragment>
          ))}
        </span>
      );
  }
}

/** A branch condition, its values rendered as references. */
export function ConditionView({ condition }: { condition: Condition }) {
  switch (condition.kind) {
    case "unspecified":
      return <Mono className="text-kumo-subtle">unspecified</Mono>;
    case "eq":
      return (
        <span className="inline-flex flex-wrap items-center gap-1">
          <RefText value={condition.value} />
          <Mono className="text-kumo-subtle">=</Mono>
          <SelectorValueView value={condition.equals} />
        </span>
      );
    case "and":
      return (
        <span className="inline-flex flex-wrap items-center gap-1">
          {condition.conditions.map((c, i) => (
            <Fragment key={i}>
              {i > 0 && <Mono className="text-kumo-inactive">∧</Mono>}
              <ConditionView condition={c} />
            </Fragment>
          ))}
        </span>
      );
    case "not":
      return (
        <span className="inline-flex flex-wrap items-center gap-1">
          <Mono className="text-kumo-subtle">¬(</Mono>
          <ConditionView condition={condition.condition} />
          <Mono className="text-kumo-subtle">)</Mono>
        </span>
      );
    case "present":
      return (
        <span className="inline-flex flex-wrap items-center gap-1">
          <Mono className="text-kumo-subtle">present(</Mono>
          <RefText value={condition.value} />
          <Mono className="text-kumo-subtle">)</Mono>
        </span>
      );
  }
}

export function TypeView({ ty }: { ty: TypeRef }) {
  const plain = typeText(ty);
  if (plain !== null) return <Mono className="text-kumo-subtle">{plain}</Mono>;
  if (ty.kind === "schema") return <IdLink id={ty.value} />;
  if (ty.kind === "list") {
    return (
      <Mono>
        <span className="text-kumo-subtle">list&lt;</span>
        <TypeView ty={ty.value} />
        <span className="text-kumo-subtle">&gt;</span>
      </Mono>
    );
  }
  return <Mono className="text-kumo-subtle">{ty.value}</Mono>;
}
