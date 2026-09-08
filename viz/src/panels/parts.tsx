import { Badge } from "@cloudflare/kumo/components/badge";
import { LayerCard } from "@cloudflare/kumo/components/layer-card";
import { Tooltip } from "@cloudflare/kumo/components/tooltip";

import type { Explanation } from "../lib/explain";
import { Collapsible } from "@cloudflare/kumo/components/collapsible";
import { Text } from "@cloudflare/kumo/components/text";
import { useState, type ReactNode } from "react";

import { splitCitations } from "../lib/citations";
import { pathText, shortId } from "../lib/ids";
import { STATUS_GLYPH, statusCounts } from "../lib/obligations";
import { predicateText, typeText } from "../lib/text";
import { useApp, useObligationsAt } from "../state/AppState";
import type {
  Derivation,
  IdempotencyKey,
  SelectorPredicate,
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

/** A titled, collapsible block of the panel. */
export function Section({
  title, count, children, defaultOpen = true,
}: { title: string; count?: number; children: ReactNode; defaultOpen?: boolean }) {
  const [open, setOpen] = useState(defaultOpen);
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

/** `kind(source).path`. The source's kind prefix is already spelled out,
 *  so the link shows the short id; the whole reference stays on one line. */
/** A value reference, `kind(source).path`.
 *
 *  It holds no spaces, so left to itself it is one unbreakable word: in
 *  a panel this narrow it overflowed its row and the tail was simply
 *  clipped, with nothing to scroll to reach it. The seams — after the
 *  source kind, and after the source — are where a reader would break
 *  the expression anyway, so they are marked as the places to break
 *  first; `break-words` catches a segment that still does not fit. */
export function RefText({ value }: { value: ValueRef }) {
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

export function PredicateView({ predicate }: { predicate: SelectorPredicate }) {
  return <Mono className="text-kumo-subtle">{predicateText(predicate)}</Mono>;
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
