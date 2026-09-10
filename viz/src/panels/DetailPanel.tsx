import { Button } from "@cloudflare/kumo/components/button";
import { Text } from "@cloudflare/kumo/components/text";
import { ArrowSquareOutIcon, XIcon } from "@phosphor-icons/react";
import type { ReactNode } from "react";

import { pathText, shortId } from "../lib/ids";
import {
  artifactRetention, commitGuarantee, delivery, externalIdempotency, externalResult, inheritedResult,
  isolation, messageIdentity, requestIdentity, requestResult, resultBinding,
  memberAssignment, memberConcurrency, requestRouting, subscriptionRouting,
  transactionOutput, transportGrouping, transportOrdering,
} from "../lib/explain";
import {
  effectDef, effectResultType, effectSummary, findTransaction, intentExecutors, operationEffects,
  walkProgram, type IndexEntry, type LocatedStep,
} from "../lib/index";
import { propertyMatchesRequirement } from "../lib/obligations";
import { accessKeysToPartition, objectAccesses, partitionKeyOf } from "../lib/runtime";
import { hashes } from "../lib/route";
import { conditionText } from "../lib/text";
import { useApp, useCitations, useObligationsAt, type DetailTarget } from "../state/AppState";
import { CLIENT_NODE_ID, EXTERNAL_PREFIX, type Edge } from "../types/graph";
import type { Id, IdempotencyKeyPropagation, OperationBlock, RequirementKind, ResultType } from "../types/model";
import { ObligationCard } from "./ObligationCard";
import {
  DerivationView, FactNote, IdLink, KeyComponents, KeyValue, List, Mono, Muted, NavLink,
  PredicateView, RefText, Section, Tag, TypeView,
} from "./parts";

/** Chrome shared by every detail: kind label, close button, title block. */
function Frame({
  kind, title, subtitle, description, children,
}: { kind: string; title: ReactNode; subtitle?: ReactNode; description?: ReactNode; children?: ReactNode }) {
  const { closeDetail } = useApp();
  return (
    <div className="flex h-full flex-col">
      <header className="flex shrink-0 items-center justify-between border-b border-kumo-hairline px-4 py-2">
        <span className="uppercase tracking-wider">
          <Text variant="secondary" size="xs" as="span">
            {kind}
          </Text>
        </span>
        <Button variant="ghost" size="xs" shape="square" icon={XIcon} aria-label="Close" onClick={closeDetail} />
      </header>
      <div className="flex-1 space-y-3 overflow-y-auto px-4 py-3">
        <div className="space-y-1">
          <div className="break-all font-mono text-[13px] font-semibold text-kumo-strong">{title}</div>
          {subtitle && <div className="text-sm text-kumo-subtle">{subtitle}</div>}
          {description && <div className="text-sm leading-relaxed text-kumo-default">{description}</div>}
        </div>
        {children}
      </div>
    </div>
  );
}

function Obligations({ obKey, filter }: { obKey: string; filter?: (ob: ReturnType<typeof useObligationsAt>[number]) => boolean }) {
  const all = useObligationsAt(obKey);
  const obs = filter ? all.filter(filter) : all;
  if (!obs.length) return null;
  return (
    <Section title="prover obligations" count={obs.length}>
      {obs.map((ob) => (
        <ObligationCard key={ob.id} ob={ob} />
      ))}
    </Section>
  );
}

/** The verdicts whose reasoning names this declaration.
 *
 *  On an L1 fact this is the list a change to the topology would put
 *  back in question — the reason proof scope is recorded at all. */
function Citations({ id }: { id: Id }) {
  const obs = useCitations(id);
  if (!obs.length) return null;
  return (
    <Section title="proofs resting on this" count={obs.length}>
      <p className="text-xs leading-relaxed text-kumo-subtle">
        Each of these cites this declaration in its reasoning. Change it and they are the verdicts
        to re-examine.
      </p>
      {obs.map((ob) => (
        <ObligationCard key={ob.id} ob={ob} />
      ))}
    </Section>
  );
}

function Propagation({ items }: { items: IdempotencyKeyPropagation[] }) {
  if (!items.length) return null;
  return (
    <Section title="idempotency key propagation" count={items.length}>
      <List
        items={items.map((p, i) => (
          <div key={i} className="space-y-0.5">
            <div><Muted>from</Muted> <KeyComponents value={p.source} /></div>
            <div><Muted>to</Muted> <KeyComponents value={p.target} /></div>
          </div>
        ))}
      />
    </Section>
  );
}

/** A Result<ok, err> contract as two schema links, with the error's
 *  declared disposition when it declares one. */
function ResultContract({ result }: { result: ResultType }) {
  return (
    <span className="inline-flex flex-wrap items-center gap-1">
      <Mono className="text-kumo-subtle">Result&lt;</Mono>
      <IdLink id={result.ok}>{shortId(result.ok)}</IdLink>
      <Mono className="text-kumo-subtle">,</Mono>
      <IdLink id={result.err.schema}>{shortId(result.err.schema)}</IdLink>
      {result.err.disposition !== "unspecified" && (
        <Mono className="text-kumo-subtle">{result.err.disposition}</Mono>
      )}
      <Mono className="text-kumo-subtle">&gt;</Mono>
    </span>
  );
}

export function DetailPanel() {
  const { detail } = useApp();
  if (!detail) return null;
  return <Dispatch target={detail} />;
}

function Dispatch({ target }: { target: DetailTarget }) {
  const { index, graph } = useApp();
  const { id, ctx } = target;

  if (ctx.req) return <RequirementDetail opId={id} prop={ctx.req.prop} reqIndex={ctx.req.index} />;
  if (ctx.txStep) return <TxStepDetail opId={ctx.txStep.op} txId={ctx.txStep.tx} stepIndex={ctx.txStep.index} />;
  if (ctx.step) return <StepDetail opId={ctx.step.op} location={ctx.step.location} />;
  if (ctx.edge) {
    const edge = graph.edges.find((e) => e.id === id);
    if (edge) return <EdgeDetail edge={edge} />;
  }
  if (ctx.access) return <AccessDetail operation={ctx.access.operation} object={ctx.access.object} />;
  if (id === CLIENT_NODE_ID) return <ClientDetail />;
  if (id.startsWith(EXTERNAL_PREFIX)) return <ExternalDetail name={id.slice(EXTERNAL_PREFIX.length)} />;

  const entry = index.get(id);
  if (!entry) return <Frame kind="unknown" title={id} />;

  switch (entry.kind) {
    case "service": return <ServiceDetail id={id} />;
    case "operation": return <OperationDetail id={id} />;
    case "topic": return <TopicDetail id={id} />;
    case "schema": return <SchemaDetail id={id} />;
    case "data_model": return <DataModelDetail id={id} />;
    case "object": return <ObjectDetail dmId={entry.dataModel} id={id} />;
    case "outbox": return <OutboxDetail dmId={entry.dataModel} id={id} />;
    case "machine": return <MachineDetail id={id} />;
    case "state": return <StateDetail mId={entry.machine} id={id} />;
    case "transition": return <TransitionDetail mId={entry.machine} id={id} />;
    case "input": return <InputDetail opId={entry.op} id={id} />;
    case "effect": return <EffectDetail id={id} />;
    case "intent": return <IntentDetail entry={entry} id={id} />;
    case "output": return <OutputDetail entry={entry} id={id} />;
    case "binding": return <BindingDetail opId={entry.op} effectId={entry.effect} location={entry.location} id={id} />;
    case "handle": return <HandleDetail opId={entry.op} effectId={entry.effect} location={entry.location} id={id} />;
    case "transaction": return <TransactionDetail opId={entry.op} id={id} />;
    case "pool": return <PoolDetail id={id} />;
    case "router": return <RouterDetail id={id} />;
    case "storage_layout": return <StorageLayoutDetail id={id} />;
  }
}

function ServiceDetail({ id }: { id: Id }) {
  const { model, graph } = useApp();
  const svc = model.services[id];
  const ops = graph.operations.filter((o) => o.service === id);
  return (
    <Frame kind="service" title={id} subtitle={<span><Tag>{svc.kind}</Tag> boundary of {ops.length} operation{ops.length === 1 ? "" : "s"}</span>}>
      <Section title="operations" count={ops.length}>
        <List items={ops.map((o) => <NavLink key={o.id} hash={hashes.op(o.id)}>{o.id}</NavLink>)} />
      </Section>
    </Frame>
  );
}

/** One block of the program as a nested ordered list. Each step is one
 *  row: kind label, principal id, and arms indented beneath decisions. */
function ProgramSummary({ opId, block, depth = 0 }: { opId: Id; block: OperationBlock; depth?: number }) {
  const { model, index } = useApp();
  const rows: ReactNode[] = block.steps.map((s, i) => {
    const number = <span className="shrink-0"><Tag>{i + 1}</Tag></span>;
    switch (s.kind) {
      case "transaction": {
        const n = s.steps.length;
        return (
          <li key={i} className="flex items-start gap-2 text-xs">
            {number}
            <div className="min-w-0">
              <span className="text-kumo-subtle">transaction </span>
              <IdLink id={s.id}>{shortId(s.id)}</IdLink>
              <span className="ml-1.5 text-kumo-inactive">{n} step{n === 1 ? "" : "s"}</span>
            </div>
          </li>
        );
      }
      case "execute_effect":
        return (
          <li key={i} className="flex items-start gap-2 text-xs">
            {number}
            <div className="min-w-0">
              <span className="text-kumo-subtle">execute effect </span>
              <IdLink id={s.effect_id}>{shortId(s.effect_id)}</IdLink>
              {s.bind && <span className="ml-1.5 text-kumo-subtle">binds <IdLink id={s.bind}>{s.bind}</IdLink></span>}
              <div className="text-kumo-inactive">{effectSummary(model, index, s.effect_id)}</div>
            </div>
          </li>
        );
      case "execute_effect_intent":
        return (
          <li key={i} className="flex items-start gap-2 text-xs">
            {number}
            <div className="min-w-0">
              <span className="text-kumo-subtle">execute intent </span>
              <IdLink id={s.intent}>{shortId(s.intent)}</IdLink>
              {s.bind && <span className="ml-1.5 text-kumo-subtle">binds <IdLink id={s.bind}>{s.bind}</IdLink></span>}
            </div>
          </li>
        );
      case "execute_effect_async":
        return (
          <li key={i} className="flex items-start gap-2 text-xs">
            {number}
            <div className="min-w-0">
              <span className="text-kumo-subtle">launch async </span>
              <IdLink id={s.effect_id}>{shortId(s.effect_id)}</IdLink>
              <span className="ml-1.5 text-kumo-subtle">binds handle <IdLink id={s.handle}>{s.handle}</IdLink></span>
              <div className="text-kumo-inactive">{effectSummary(model, index, s.effect_id)}</div>
            </div>
          </li>
        );
      case "execute_effect_intent_async":
        return (
          <li key={i} className="flex items-start gap-2 text-xs">
            {number}
            <div className="min-w-0">
              <span className="text-kumo-subtle">launch intent async </span>
              <IdLink id={s.intent}>{shortId(s.intent)}</IdLink>
              <span className="ml-1.5 text-kumo-subtle">binds handle <IdLink id={s.handle}>{s.handle}</IdLink></span>
            </div>
          </li>
        );
      case "join_all":
        return (
          <li key={i} className="flex items-start gap-2 text-xs">
            {number}
            <div className="min-w-0">
              <span className="text-kumo-subtle">join_all </span>
              {s.handles.map((entry, j) => (
                <span key={entry.handle}>
                  {j > 0 && <span className="text-kumo-inactive">, </span>}
                  <IdLink id={entry.handle}>{entry.handle}</IdLink>
                  {entry.bind && <span className="text-kumo-subtle"> binds <IdLink id={entry.bind}>{entry.bind}</IdLink></span>}
                </span>
              ))}
            </div>
          </li>
        );
      case "race":
        return (
          <li key={i} className="flex items-start gap-2 text-xs">
            {number}
            <div className="min-w-0">
              <span className="text-kumo-subtle">race </span>
              {s.handles.map((h, j) => (
                <span key={h}>
                  {j > 0 && <span className="text-kumo-inactive">, </span>}
                  <IdLink id={h}>{h}</IdLink>
                </span>
              ))}
              {s.bind && <span className="ml-1.5 text-kumo-subtle">winner binds <IdLink id={s.bind}>{s.bind}</IdLink></span>}
            </div>
          </li>
        );
      case "match_result":
        return (
          <li key={i} className="flex items-start gap-2 text-xs">
            {number}
            <div className="min-w-0 flex-1">
              <span className="text-kumo-subtle">match </span>
              <IdLink id={s.result}>{shortId(s.result)}</IdLink>
              <div className="mt-1 space-y-1 border-l border-kumo-hairline pl-2">
                <div><Tag variant="success">ok</Tag></div>
                <ProgramSummary opId={opId} block={s.ok} depth={depth + 1} />
                <div><Tag variant="warning">err</Tag></div>
                <ProgramSummary opId={opId} block={s.err} depth={depth + 1} />
              </div>
            </div>
          </li>
        );
      case "branch":
        return (
          <li key={i} className="flex items-start gap-2 text-xs">
            {number}
            <div className="min-w-0 flex-1">
              <span className="text-kumo-subtle">branch on </span>
              <Mono className="text-kumo-subtle">{conditionText(s.condition)}</Mono>
              <div className="mt-1 space-y-1 border-l border-kumo-hairline pl-2">
                <div><Tag>then</Tag></div>
                <ProgramSummary opId={opId} block={s.then} depth={depth + 1} />
                <div><Tag>otherwise</Tag></div>
                {s.otherwise
                  ? <ProgramSummary opId={opId} block={s.otherwise} depth={depth + 1} />
                  : <Muted>falls through</Muted>}
              </div>
            </div>
          </li>
        );
      case "return":
        return (
          <li key={i} className="flex items-start gap-2 text-xs">
            {number}
            <div className="min-w-0">
              <span className="text-kumo-subtle">return </span>
              <Tag variant={s.outcome.kind === "ok" ? "success" : "warning"}>{s.outcome.kind}</Tag>
              <span className="ml-1.5 text-kumo-subtle">for <IdLink id={s.request}>{shortId(s.request)}</IdLink></span>
            </div>
          </li>
        );
      case "complete":
        return (
          <li key={i} className="flex items-start gap-2 text-xs">
            {number}
            <span className="text-kumo-subtle">complete</span>
          </li>
        );
    }
  });

  if (!rows.length) return <Muted>empty</Muted>;

  return <ol className="space-y-1.5">{rows}</ol>;
}

function OperationDetail({ id }: { id: Id }) {
  const { model, graph, index, navigateTo } = useApp();
  const op = model.operations[id];
  const node = graph.operations.find((o) => o.id === id);
  const inputs = Object.entries(op.inputs);
  const effects = operationEffects(op).map(([eid]) => eid);
  const reqs = op.requirements;
  const reqRows: ReactNode[] = [];
  reqs.serialization.forEach((r, i) => reqRows.push(
    <div key={`s${i}`} className="flex flex-wrap items-center gap-1.5"><Tag variant="blue">serialization</Tag><RefText value={r.key} /></div>));
  reqs.ordering.forEach((r, i) => reqRows.push(
    <div key={`o${i}`} className="flex flex-wrap items-center gap-1.5"><Tag variant="purple">ordering</Tag><RefText value={r.key} /></div>));
  reqs.idempotency.forEach((r, i) => reqRows.push(
    <div key={`i${i}`} className="flex flex-wrap items-center gap-1.5">
      <Tag variant="orange">idempotency</Tag>{r.result === "replay_consistent" && <Tag variant="info">replay_consistent</Tag>}
      <KeyComponents value={r.key} />
    </div>));
  reqs.recoverability.forEach((r, i) => reqRows.push(
    <div key={`r${i}`} className="flex flex-wrap items-center gap-1.5">
      <Tag variant="success">recoverability</Tag><Tag>{r.completion}</Tag><KeyComponents value={r.key} />
    </div>));

  return (
    <Frame
      kind="operation"
      title={
        <button
          type="button"
          className="inline-flex max-w-full cursor-pointer items-center gap-1.5 text-left text-kumo-link hover:underline"
          title="Open the operation page"
          onClick={() => navigateTo(hashes.op(id))}
        >
          <span className="break-all">{id}</span>
          <ArrowSquareOutIcon size={13} className="shrink-0" />
        </button>
      }
      subtitle={<span>operation on <IdLink id={op.service} /></span>}
      description={op.description}
    >
      <Button variant="secondary" size="xs" icon={ArrowSquareOutIcon} onClick={() => navigateTo(hashes.op(id))}>
        open operation page
      </Button>
      <Section title="program" count={walkProgram(op.program).length}>
        <div className="rounded-md border border-kumo-hairline bg-kumo-elevated/40 p-2.5">
          <ProgramSummary opId={id} block={op.program} />
        </div>
      </Section>
      {inputs.length > 0 && (
        <Section title="inputs" count={inputs.length}>
          <List items={inputs.map(([iid, input]) => (
            <span key={iid} className="flex flex-wrap items-center gap-1.5">
              <IdLink id={iid} />
              {input.kind === "request"
                ? <Tag variant="info">request</Tag>
                : input.kind === "subscription"
                  ? <Tag variant="blue">sub ← {shortId(input.topic)}</Tag>
                  : <Tag variant="purple">outbox ← {shortId(input.outbox)}</Tag>}
            </span>
          ))} />
        </Section>
      )}
      {effects.length > 0 && (
        <Section title="inline effects" count={effects.length}>
          <List items={effects.map((eid) => (
            <div key={eid}><IdLink id={eid} /><div className="text-xs text-kumo-subtle">{effectSummary(model, index, eid)}</div></div>
          ))} />
        </Section>
      )}
      {node && node.machines.length > 0 && (
        <Section title="state machines" count={node.machines.length}>
          <List items={node.machines.map((m) => <NavLink key={m} hash={hashes.machine(m)}>{m}</NavLink>)} />
        </Section>
      )}
      {reqRows.length > 0 && (
        <Section title="requirements" count={reqRows.length}>
          <List items={reqRows} />
        </Section>
      )}
      <Obligations obKey={id} />
    </Frame>
  );
}

function TopicDetail({ id }: { id: Id }) {
  const { model, graph } = useApp();
  const topic = model.topics[id];
  const topicRuntime = model.runtime?.topics?.[id];
  const pubs = graph.edges.filter((e): e is Extract<Edge, { kind: "publish" }> => e.kind === "publish" && e.to === id);
  const subs = graph.edges.filter((e): e is Extract<Edge, { kind: "subscribe" }> => e.kind === "subscribe" && e.from === id);
  return (
    <Frame kind="topic" title={id} subtitle={<span>topic</span>}>
      <FactNote fact={transportGrouping(topicRuntime?.grouping)} />
      <FactNote fact={transportOrdering(topicRuntime?.ordering)} />
      <FactNote fact={messageIdentity(topic.message_identity)} />
      <Section title="message schemas" count={topic.messages.length}>
        <List items={topic.messages.map((s) => <IdLink key={s} id={s} />)} />
      </Section>
      {topicRuntime?.grouping && (
        <Section title="transport grouping key mapping">
          <KeyValue rows={Object.entries(topicRuntime.grouping).map(([schema, tuple]) => [shortId(schema), <Mono key={schema}>{tuple.map(pathText).join(", ")}</Mono>])} />
        </Section>
      )}
      {topic.message_identity.kind === "keyed" && (
        <Section title="message identity mapping">
          <KeyValue rows={Object.entries(topic.message_identity.mapping).map(([schema, tuple]) => [shortId(schema), <Mono key={schema}>{tuple.map(pathText).join(", ")}</Mono>])} />
        </Section>
      )}
      {pubs.length > 0 && (
        <Section title="publishers" count={pubs.length}>
          <List items={pubs.map((e) => <span key={e.id} className="flex flex-wrap items-center gap-1.5"><IdLink id={e.operation} /><Tag variant="purple">{shortId(e.schema)}</Tag></span>)} />
        </Section>
      )}
      {subs.length > 0 && (
        <Section title="subscribers" count={subs.length}>
          <List items={subs.map((e) => <span key={e.id} className="flex flex-wrap items-center gap-1.5"><IdLink id={e.operation} /><Tag>{e.delivery}</Tag>{e.pool && <Tag>{shortId(e.pool)}</Tag>}</span>)} />
        </Section>
      )}
      <Obligations obKey={id} />
      <Citations id={id} />
    </Frame>
  );
}


function OutboxDetail({ dmId, id }: { dmId: Id; id: Id }) {
  const { model, graph } = useApp();
  const outbox = model.data_models[dmId]?.outboxes?.[id];
  if (!outbox) return <Frame kind="outbox" title={id} />;
  const writes = graph.edges.filter((e): e is Extract<Edge, { kind: "outbox_write" }> => e.kind === "outbox_write" && e.to === id);
  const consumers = graph.edges.filter((e): e is Extract<Edge, { kind: "outbox_consume" }> => e.kind === "outbox_consume" && e.from === id);
  return (
    <Frame
      kind="outbox"
      title={id}
      subtitle={<span>outbox of <IdLink id={dmId} /></span>}
      description={<span>A transactional message collection: a transaction on <IdLink id={dmId} /> may mutate its objects and admit messages here in one atomic commit.</span>}
    >
      <FactNote fact={messageIdentity(outbox.message_identity)} />
      <Section title="message schemas" count={outbox.messages.length}>
        <List items={outbox.messages.map((s) => <IdLink key={s} id={s} />)} />
      </Section>
      {outbox.message_identity.kind === "keyed" && (
        <Section title="message identity mapping">
          <KeyValue rows={Object.entries(outbox.message_identity.mapping).map(([schema, tuple]) => [shortId(schema), <Mono key={schema}>{tuple.map(pathText).join(", ")}</Mono>])} />
        </Section>
      )}
      {writes.length > 0 && (
        <Section title="writers" count={writes.length}>
          <List items={writes.map((e) => (
            <span key={e.id} className="flex flex-wrap items-center gap-1.5">
              <IdLink id={e.operation} />
              <Tag variant="purple">{shortId(e.schema)}</Tag>
              {e.transaction && <Tag>atomic with {shortId(e.transaction)}</Tag>}
            </span>
          ))} />
        </Section>
      )}
      {consumers.length > 0 && (
        <Section title="consumers" count={consumers.length}>
          <List items={consumers.map((e) => (
            <span key={e.id} className="flex flex-wrap items-center gap-1.5">
              <IdLink id={e.operation} />
              <Tag>{e.delivery}</Tag>
              {e.ordering && <Tag>ordering: {e.ordering}</Tag>}
              {e.pool && <Tag>{shortId(e.pool)}</Tag>}
            </span>
          ))} />
        </Section>
      )}
      <Obligations obKey={id} />
    </Frame>
  );
}

function SchemaDetail({ id }: { id: Id }) {
  const { model } = useApp();
  const schema = model.schemas[id];
  if (schema.kind === "canonical") {
    const fields = Object.entries(schema.fields);
    return (
      <Frame kind="schema" title={id} subtitle={<span>canonical schema · <Tag>{schema.completeness}</Tag></span>} description={schema.description}>
        <Section title="fields" count={fields.length}>
          <List items={fields.map(([name, f]) => (
            <span key={name} className="flex flex-wrap items-center gap-1.5">
              <Mono>{name}</Mono><span className="text-kumo-inactive">:</span><TypeView ty={f.ty} />
              {f.optional && <Tag>optional</Tag>}
            </span>
          ))} />
        </Section>
      </Frame>
    );
  }
  return (
    <Frame kind="schema" title={id} subtitle={<span>fragment of <IdLink id={schema.source} /></span>}>
      <Section title="mapping">
        <KeyValue rows={Object.entries(schema.mapping).map(([name, path]) => [name, <Mono key={name}>{pathText(path)}</Mono>])} />
      </Section>
    </Frame>
  );
}

function DataModelDetail({ id }: { id: Id }) {
  const { model } = useApp();
  const objects = Object.keys(model.data_models[id].objects);
  return (
    <Frame kind="data model" title={id} subtitle="transactional state boundary">
      <Section title="objects" count={objects.length}>
        <List items={objects.map((o) => <IdLink key={o} id={o} />)} />
      </Section>
    </Frame>
  );
}

function ObjectDetail({ dmId, id }: { dmId: Id; id: Id }) {
  const { model } = useApp();
  const obj = model.data_models[dmId].objects[id];
  // A storage layout is L1: it says how this object is partitioned, which
  // is neither its identity nor a routing key. Absent means no such fact.
  const layout = Object.entries(model.runtime?.storage_layouts ?? {}).find(
    ([, l]) => l.object.object === id,
  );
  const touchers = [...(objectAccesses(model).get(id)?.keys() ?? [])];
  return (
    <Frame kind="data object" title={id} subtitle={<span>persistent object in <IdLink id={dmId} /></span>}>
      <KeyValue rows={[["schema", <IdLink key="s" id={obj.schema} />], ["identity", <Mono key="i">{obj.identity.map(pathText).join(", ")}</Mono>]]} />
      <Section title="L1 · storage">
        {layout ? (
          <>
            <KeyValue rows={[
              ["layout", <IdLink key="l" id={layout[0]} />],
              ["partition key", <Mono key="k">{layout[1].partition_key.map(pathText).join(", ")}</Mono>],
            ]} />
            <p className="text-xs leading-relaxed text-kumo-subtle">
              Partitioned. A partition key says where rows live; it is not the object's identity and
              not a routing key, and no serialization or ordering proof rests on it.
            </p>
          </>
        ) : (
          <p className="text-xs leading-relaxed text-kumo-subtle">
            No storage layout declared. Drawn unpartitioned because that is all the model says — not
            a claim that the store is unpartitioned.
          </p>
        )}
      </Section>
      {touchers.length > 0 && (
        <Section title="accessed by" count={touchers.length}>
          <List items={touchers.map((op) => <IdLink key={op} id={op} />)} />
        </Section>
      )}
      <Obligations obKey={`${dmId}/${id}`} />
    </Frame>
  );
}

function MachineDetail({ id }: { id: Id }) {
  const { model } = useApp();
  const m = model.state_machines[id];
  const transitions = Object.keys(m.transitions);
  return (
    <Frame kind="state machine" title={id} subtitle={<span>governs <IdLink id={m.subject.object} /> · field <Mono>{pathText(m.subject.state)}</Mono></span>}>
      <NavLink hash={hashes.machine(id)}>open state graph →</NavLink>
      <Section title="states" count={m.states.length}>
        <List items={m.states.map((s) => <span key={s} className="flex items-center gap-1.5"><IdLink id={s} />{s === m.initial && <Tag variant="success">initial</Tag>}</span>)} />
      </Section>
      <Section title="transitions" count={transitions.length}>
        <List items={transitions.map((t) => <NavLink key={t} hash={hashes.machine(id, t)} selection={`t:${t}`}>{t}</NavLink>)} />
      </Section>
      <Obligations obKey={id} />
    </Frame>
  );
}

function StateDetail({ mId, id }: { mId: Id; id: Id }) {
  const { model } = useApp();
  const m = model.state_machines[mId];
  const into: Id[] = [];
  const outOf: Id[] = [];
  for (const [tId, t] of Object.entries(m.transitions)) {
    if (t.to === id) into.push(tId);
    if (t.from.includes(id)) outOf.push(tId);
  }
  return (
    <Frame kind="state" title={id} subtitle={<span>state of <IdLink id={mId} />{id === m.initial && <> · <Tag variant="success">initial</Tag></>}</span>}>
      {outOf.length > 0 && <Section title="transitions out" count={outOf.length}><List items={outOf.map((t) => <IdLink key={t} id={t} />)} /></Section>}
      {into.length > 0 && <Section title="transitions in" count={into.length}><List items={into.map((t) => <IdLink key={t} id={t} />)} /></Section>}
    </Frame>
  );
}

function TransitionDetail({ mId, id }: { mId: Id; id: Id }) {
  const { model, graph } = useApp();
  const t = model.state_machines[mId].transitions[id];
  const fx = Object.entries(t.side_effects);
  const refs = graph.transition_refs[`${mId}/${id}`] ?? [];
  return (
    <Frame kind="transition" title={id} subtitle={<span>transition of <IdLink id={mId} /></span>}>
      <KeyValue rows={[["from", <Mono key="f">{t.from.join(", ")}</Mono>], ["to", <Mono key="t">{t.to}</Mono>]]} />
      {fx.length > 0 && (
        <Section title="side effects" count={fx.length}>
          <List items={fx.map(([eid, e]) => (
            <div key={eid} className="space-y-0.5">
              <IdLink id={eid} />
              <div className="text-xs text-kumo-subtle">
                {e.kind === "publication" ? <>publish <IdLink id={e.schema} /> → <IdLink id={e.topic} /></> : <>request → <IdLink id={e.target.operation} /></>}
              </div>
              {intentExecutors(model, eid).map((x) => (
                <div key={x.intent} className="text-xs text-kumo-subtle">executed by <IdLink id={x.op} /> via <IdLink id={x.intent} /></div>
              ))}
            </div>
          ))} />
        </Section>
      )}
      {refs.length > 0 && (
        <Section title="taken by transactions" count={refs.length}>
          <List items={refs.map((r, i) => (
            <span key={i}><IdLink id={r.transaction} /> step {r.step + 1} in <NavLink hash={hashes.op(r.operation)} selection={`tx:${r.transaction}`}>{shortId(r.operation)}</NavLink></span>
          ))} />
        </Section>
      )}
      <Obligations obKey={`${mId}/${id}`} />
    </Frame>
  );
}

function InputDetail({ opId, id }: { opId: Id; id: Id }) {
  const { model } = useApp();
  const input = model.operations[opId].inputs[id];
  if (input.kind === "request") {
    const routers = Object.entries(model.runtime?.routers ?? {});
    const routed = routers.find(([, r]) => r.boundary.operation === opId && r.boundary.input === id);
    const routerPool = routed ? model.runtime?.execution_pools?.[routed[1].pool] : undefined;

    return (
      <Frame kind="input" title={id} subtitle={<span>request input of <IdLink id={opId} /></span>}>
        <KeyValue rows={[
          ["schema", <IdLink key="s" id={input.schema} />],
          ["result", <ResultContract key="r" result={input.result} />],
        ]} />
        <FactNote fact={requestIdentity(input.identity)} />
        <FactNote fact={requestResult()} />
        {routed && (
          <Section title="L1 · realization">
            <KeyValue rows={[
              ["router", <IdLink key="r" id={routed[0]} />],
              ["pool", <IdLink key="p" id={routed[1].pool} />],
            ]} />
            <FactNote fact={requestRouting(routed[1].routing?.key)} />
            {routed[1].routing && (
              <FactNote fact={memberAssignment(routed[1].routing.member_assignment)} />
            )}
            {routerPool && <FactNote fact={memberConcurrency(routerPool.member_concurrency)} />}
          </Section>
        )}
        <Citations id={id} />
      </Frame>
    );
  }
  if (input.kind === "outbox") {
    const schemas = input.messages.kind === "all" ? null : input.messages.schemas;
    const runtime = model.runtime?.outboxes?.[opId]?.[id];
    const pool = runtime ? model.runtime?.execution_pools?.[runtime.dispatch.pool] : undefined;

    return (
      <Frame kind="input" title={id} subtitle={<span>outbox input of <IdLink id={opId} /></span>}>
        <KeyValue rows={[
          ["outbox", <IdLink key="o" id={input.outbox} />],
          ["acknowledge on success", <Mono key="a">{String(input.acknowledge_on_success)}</Mono>],
        ]} />
        <Section title="consumed messages">
          {schemas ? <List items={schemas.map((s) => <IdLink key={s} id={s} />)} /> : <Tag>all outbox messages</Tag>}
        </Section>
        {runtime ? (
          <Section title="L1 · realization">
            <KeyValue rows={[
              ["pool", <IdLink key="p" id={runtime.dispatch.pool} />],
              ["partitioning", <Mono key="pt">{runtime.partitioning.kind}</Mono>],
              ["ordering", <Mono key="or">{runtime.ordering}</Mono>],
              ["batching", <Mono key="b">{runtime.dispatch.batching ? runtime.dispatch.batching.ordering : "none declared"}</Mono>],
            ]} />
            <FactNote fact={delivery(runtime.delivery)} />
            <FactNote fact={memberAssignment(runtime.dispatch.member_assignment)} />
            {pool && <FactNote fact={memberConcurrency(pool.member_concurrency)} />}
          </Section>
        ) : (
          <FactNote fact={delivery("unspecified")} />
        )}
        <Citations id={id} />
      </Frame>
    );
  }

  const schemas = input.messages.kind === "all" ? null : input.messages.schemas;
  const runtime = model.runtime?.subscriptions?.[opId]?.[id];
  const pool = runtime ? model.runtime?.execution_pools?.[runtime.dispatch.pool] : undefined;

  return (
    <Frame kind="input" title={id} subtitle={<span>subscription of <IdLink id={opId} /></span>}>
      <KeyValue rows={[["topic", <IdLink key="t" id={input.topic} />]]} />
      <Section title="consumed messages">
        {schemas ? <List items={schemas.map((s) => <IdLink key={s} id={s} />)} /> : <Tag>all topic messages</Tag>}
      </Section>
      {runtime ? (
        <Section title="L1 · realization">
          <KeyValue rows={[["pool", <IdLink key="p" id={runtime.dispatch.pool} />]]} />
          <FactNote fact={delivery(runtime.delivery)} />
          <FactNote fact={subscriptionRouting(runtime.dispatch.routing?.key)} />
          {runtime.dispatch.routing && (
            <FactNote fact={memberAssignment(runtime.dispatch.routing.member_assignment)} />
          )}
          {pool && <FactNote fact={memberConcurrency(pool.member_concurrency)} />}
        </Section>
      ) : (
        <FactNote fact={delivery("unspecified")} />
      )}
      <Citations id={id} />
    </Frame>
  );
}

function EffectDetail({ id }: { id: Id }) {
  const { model, index } = useApp();
  const def = effectDef(model, index, id);
  if (!def) return <Frame kind="effect" title={id} />;
  const e = def.effect;
  const owner = def.owner.op !== undefined
    ? <span>declared by <IdLink id={def.owner.op} /></span>
    : <span>owned by transition <IdLink id={def.owner.transition!} /> of <IdLink id={def.owner.machine!} /></span>;
  const executors = intentExecutors(model, id);
  const inherited = e.kind === "request" ? effectResultType(model, index, id) : null;
  return (
    <Frame kind="effect" title={id} subtitle={owner}>
      {e.kind === "publication" && (
        <>
          <KeyValue rows={[["kind", <Tag key="k" variant="purple">publication</Tag>], ["topic", <IdLink key="t" id={e.topic} />], ["schema", <IdLink key="s" id={e.schema} />]]} />
          <Propagation items={e.idempotency_key_propagation} />
        </>
      )}
      {e.kind === "request" && (
        <>
          <KeyValue rows={[["kind", <Tag key="k" variant="orange">request</Tag>], ["operation", <IdLink key="o" id={e.target.operation} />], ["input", <IdLink key="i" id={e.target.input} />], ["schema", <IdLink key="s" id={e.schema} />], ["retry", <Tag key="r">{e.retry}</Tag>]]} />
          <FactNote fact={inheritedResult()}>
            {inherited && <ResultContract result={inherited} />}
          </FactNote>
          <Propagation items={e.idempotency_key_propagation} />
        </>
      )}
      {e.kind === "external" && (
        <>
          <KeyValue rows={[["kind", <Tag key="k" variant="warning">external</Tag>], ["name", <Mono key="n">{e.name}</Mono>]]} />
          <FactNote fact={externalIdempotency(e.idempotency)}>
            {e.idempotency.kind === "deduplicated_by" && (
              <span className="text-xs text-kumo-subtle">by <KeyComponents value={e.idempotency.key} /></span>
            )}
          </FactNote>
          <FactNote fact={externalResult(e.result, e.idempotency)}>
            {e.result && <ResultContract result={e.result} />}
          </FactNote>
        </>
      )}
      {e.kind === "outbox_write" && (
        <>
          <KeyValue rows={[["kind", <Tag key="k" variant="purple">outbox write</Tag>], ["outbox", <IdLink key="o" id={e.outbox} />], ["schema", <IdLink key="s" id={e.schema} />]]} />
          <div className="text-xs text-kumo-subtle">
            Executes only as a transaction step; the message is admitted atomically with that transaction's commit, and the write binds no result.
          </div>
          <Propagation items={e.idempotency_key_propagation} />
        </>
      )}
      {executors.length > 0 && (
        <Section title="executed via intents" count={executors.length}>
          <List items={executors.map((x) => <span key={x.intent}><IdLink id={x.intent} /> in <IdLink id={x.op} /></span>)} />
        </Section>
      )}
    </Frame>
  );
}

function IntentDetail({ entry, id }: { entry: Extract<IndexEntry, { kind: "intent" }>; id: Id }) {
  const { model, index } = useApp();
  const tx = findTransaction(model.operations[entry.op], entry.transaction);
  return (
    <Frame kind="effect intent" title={id} subtitle={<span>intent binding of <IdLink id={entry.op} /></span>}
      description={entry.via ? <>The effect is owned by transition <IdLink id={entry.via.transition} />; applying it establishes this bound intent.</> : undefined}>
      <KeyValue rows={[
        ["effect", <IdLink key="e" id={entry.effect} />],
        ["resolves to", effectSummary(model, index, entry.effect)],
        ["established by", <IdLink key="t" id={entry.transaction} />],
      ]} />
      {tx && <FactNote fact={artifactRetention(tx.idempotency)} />}
    </Frame>
  );
}

function OutputDetail({ entry, id }: { entry: Extract<IndexEntry, { kind: "output" }>; id: Id }) {
  const { model } = useApp();
  const tx = findTransaction(model.operations[entry.op], entry.transaction);
  return (
    <Frame kind="transaction output" title={id} subtitle={<span>typed export of <IdLink id={entry.op} /></span>}>
      <KeyValue rows={[
        ["schema", <IdLink key="s" id={entry.schema} />],
        ["established by", <IdLink key="t" id={entry.transaction} />],
      ]} />
      <FactNote fact={transactionOutput()} />
      {tx && <FactNote fact={artifactRetention(tx.idempotency)} />}
    </Frame>
  );
}

function BindingDetail({ opId, effectId, location, id }: { opId: Id; effectId: Id; location: string; id: Id }) {
  const { model, index } = useApp();
  const contract = effectResultType(model, index, effectId);
  return (
    <Frame kind="effect result" title={id} subtitle={<span>result binding in <IdLink id={opId} /></span>}>
      <KeyValue rows={[
        ["observes", <IdLink key="e" id={effectId} />],
        ["which is", effectSummary(model, index, effectId)],
        ["bound at step", <Mono key="l">{location}</Mono>],
        ["contract", contract ? <ResultContract key="c" result={contract} /> : <Muted key="c">no synchronous result</Muted>],
      ]} />
      <FactNote fact={resultBinding()} />
    </Frame>
  );
}

/** Detail for a program step selected on the operation page: decisions
 *  and terminals, which have no id of their own. */
function StepDetail({ opId, location }: { opId: Id; location: string }) {
  const { model } = useApp();
  const op = model.operations[opId];
  const located: LocatedStep | undefined = walkProgram(op.program).find((s) => s.location === location);
  const sub = <span>step {location} of <IdLink id={opId} />'s program</span>;

  if (!located) return <Frame kind="program step" title={`step ${location}`} subtitle={sub} />;

  const step = located.step;
  const arms = (label: string, block: OperationBlock | null) =>
    [label, block ? `${block.steps.length} step${block.steps.length === 1 ? "" : "s"}` : "falls through"] as [string, ReactNode];

  switch (step.kind) {
    case "match_result":
      return (
        <Frame kind="program step" title={`match · ${shortId(step.result)}`} subtitle={sub}
          description="Destructures the bound result: exactly one arm executes, and each variant payload is available only inside its own arm.">
          <KeyValue rows={[
            ["result", <IdLink key="r" id={step.result} />],
            arms("ok arm", step.ok),
            arms("err arm", step.err),
          ]} />
          <Obligations obKey={opId} />
        </Frame>
      );
    case "branch":
      return (
        <Frame kind="program step" title="branch" subtitle={sub}
          description={step.condition.kind === "unspecified"
            ? "The condition declares no fact about how the decision is made, so a retry is not established to take the same arm."
            : "An ordinary control decision over modeled values; a retry takes the same arm exactly when the condition's roots are replay-stable."}>
          <KeyValue rows={[
            ["condition", <Mono key="c" className="text-kumo-subtle">{conditionText(step.condition)}</Mono>],
            arms("then arm", step.then),
            arms("otherwise arm", step.otherwise),
          ]} />
        </Frame>
      );
    case "return":
      return (
        <Frame kind="program step" title={`return ${step.outcome.kind}`} subtitle={sub}
          description="Terminates the execution by constructing the request input's declared result.">
          <KeyValue rows={[
            ["request", <IdLink key="r" id={step.request} />],
            ["variant", <Tag key="v" variant={step.outcome.kind === "ok" ? "success" : "warning"}>{step.outcome.kind}</Tag>],
          ]} />
          <Section title="payload provenance"><DerivationView value={step.outcome.values} /></Section>
        </Frame>
      );
    case "complete":
      return (
        <Frame kind="program step" title="complete" subtitle={sub}
          description="Terminates the execution without a returned value, as is natural for a subscription-driven operation." />
      );
    case "transaction":
      return (
        <Frame kind="program step" title={`transaction · ${shortId(step.id)}`} subtitle={sub}>
          <KeyValue rows={[["transaction", <IdLink key="t" id={step.id} />]]} />
        </Frame>
      );
    case "execute_effect":
      return (
        <Frame kind="program step" title={`execute effect · ${shortId(step.effect_id)}`} subtitle={sub}>
          <KeyValue rows={[
            ["effect", <IdLink key="e" id={step.effect_id} />],
            ["binds", step.bind ? <IdLink key="b" id={step.bind} /> : <Muted key="b">nothing — the result is ignored</Muted>],
          ]} />
          <Section title="instance provenance"><DerivationView value={step.values} /></Section>
        </Frame>
      );
    case "execute_effect_intent":
      return (
        <Frame kind="program step" title={`execute intent · ${shortId(step.intent)}`} subtitle={sub}>
          <KeyValue rows={[
            ["intent", <IdLink key="i" id={step.intent} />],
            ["binds", step.bind ? <IdLink key="b" id={step.bind} /> : <Muted key="b">nothing — the result is ignored</Muted>],
          ]} />
        </Frame>
      );
    case "execute_effect_async":
      return (
        <Frame kind="program step" title={`launch async · ${shortId(step.effect_id)}`} subtitle={sub}
          description="Constructs and initiates the effect instance without waiting for it to complete; only the handle is bound, and the result becomes available only at a join_all or race.">
          <KeyValue rows={[
            ["effect", <IdLink key="e" id={step.effect_id} />],
            ["handle", <IdLink key="h" id={step.handle} />],
          ]} />
          <Section title="instance provenance"><DerivationView value={step.values} /></Section>
        </Frame>
      );
    case "execute_effect_intent_async":
      return (
        <Frame kind="program step" title={`launch intent async · ${shortId(step.intent)}`} subtitle={sub}
          description="Initiates the exact instance the intent captured, without waiting for it to complete; only the handle is bound.">
          <KeyValue rows={[
            ["intent", <IdLink key="i" id={step.intent} />],
            ["handle", <IdLink key="h" id={step.handle} />],
          ]} />
        </Frame>
      );
    case "join_all":
      return (
        <Frame kind="program step" title="join_all" subtitle={sub}
          description="An all-completion barrier: the continuation follows completion of every referenced execution, with no order established among them. Each entry may bind its effect's ordinary result; an Err is still a completed interaction, so the barrier never short-circuits on one.">
          <Section title="waits for" count={step.handles.length}>
            <List items={step.handles.map((entry) => (
              <span key={entry.handle} className="flex flex-wrap items-center gap-1.5">
                <IdLink id={entry.handle} />
                {entry.bind
                  ? <span className="text-xs text-kumo-subtle">binds <IdLink id={entry.bind} /></span>
                  : <Muted>no result bound</Muted>}
              </span>
            ))} />
          </Section>
        </Frame>
      );
    case "race":
      return (
        <Frame kind="program step" title="race" subtitle={sub}
          description="A first-completion barrier: the continuation follows whichever candidate completes first — first completion, not first success, so a winning Err is what a result-binding race observes. Losing executions are not cancelled and remain part of the operation's side-effect blast radius.">
          <Section title="candidates" count={step.handles.length}>
            <List items={step.handles.map((h) => <IdLink key={h} id={h} />)} />
          </Section>
          <KeyValue rows={[
            ["winner binds", step.bind ? <IdLink key="b" id={step.bind} /> : <Muted key="b">nothing — heterogeneous candidates may race unbound</Muted>],
          ]} />
        </Frame>
      );
  }
}

/** An async handle: the operation-local synchronization artifact one
 *  launch bound, consumable only by join_all and race. */
function HandleDetail({ opId, effectId, location, id }: { opId: Id; effectId: Id | null; location: string; id: Id }) {
  const { model, index } = useApp();
  return (
    <Frame kind="async handle" title={id} subtitle={<span>async handle in <IdLink id={opId} /></span>}
      description="Identifies one asynchronous execution occurrence for later synchronization. It is not application data: it has no schema, cannot be persisted or returned, and does not identify the logical effect itself — the effect id does.">
      <KeyValue rows={[
        ["launches", effectId ? <IdLink key="e" id={effectId} /> : <Muted key="e">unresolved intent</Muted>],
        ["which is", effectId ? effectSummary(model, index, effectId) : "—"],
        ["launched at step", <Mono key="l">{location}</Mono>],
      ]} />
    </Frame>
  );
}

function TransactionDetail({ opId, id }: { opId: Id; id: Id }) {
  const { model, openDetail } = useApp();
  const tx = findTransaction(model.operations[opId], id);
  if (!tx) return <Frame kind="transaction" title={id} subtitle={<span>inline transaction of <IdLink id={opId} /></span>} />;
  return (
    <Frame kind="transaction" title={id} subtitle={<span>inline transaction of <IdLink id={opId} /></span>}>
      <KeyValue rows={[
        ["data model", tx.data_model ? <IdLink key="d" id={tx.data_model} /> : <Muted key="d">none (framework artifacts only)</Muted>],
      ]} />
      <FactNote fact={commitGuarantee(tx.idempotency)}>
        {tx.idempotency.kind === "deduplicated_by" && (
          <span className="text-xs text-kumo-subtle">by <KeyComponents value={tx.idempotency.key} /></span>
        )}
      </FactNote>
      <FactNote fact={isolation(tx.isolation)} />
      <Section title="steps" count={tx.steps.length}>
        <List items={tx.steps.map((s, i) => (
          <button key={i} type="button" className="flex w-full cursor-pointer items-center gap-2 text-left hover:underline"
            onClick={() => openDetail(id, { txStep: { op: opId, tx: id, index: i } })}>
            <Tag>{i + 1}</Tag>
            <span className="text-sm">{s.kind.replace(/_/g, " ")}</span>
            {s.kind === "read" && <Mono className="text-kumo-subtle">{shortId(s.bind)}</Mono>}
            {s.kind === "transition" && <Mono className="text-kumo-subtle">{shortId(s.transition)}</Mono>}
            {(s.kind === "write" || s.kind === "delete" || s.kind === "lock") && <Mono className="text-kumo-subtle">{shortId(s.target.object)}</Mono>}
            {s.kind === "insert" && <Mono className="text-kumo-subtle">{shortId(s.object)}</Mono>}
            {s.kind === "establish_transaction_output" && <Mono className="text-kumo-subtle">{shortId(s.bind)}</Mono>}
            {s.kind === "establish_effect_intent" && <Mono className="text-kumo-subtle">{shortId(s.bind)}</Mono>}
          </button>
        ))} />
      </Section>
      <Obligations obKey={`${opId}/${id}`} />
    </Frame>
  );
}

function RequirementDetail({ opId, prop, reqIndex }: { opId: Id; prop: RequirementKind; reqIndex: number }) {
  const { model } = useApp();
  const reqs = model.operations[opId].requirements;
  const rows: ReactNode[] = [];
  if (prop === "serialization" || prop === "ordering") {
    const r = reqs[prop][reqIndex];
    rows.push(<RefText key="k" value={r.key} />);
  } else if (prop === "idempotency") {
    const r = reqs.idempotency[reqIndex];
    rows.push(<KeyComponents key="k" value={r.key} />);
  } else {
    const r = reqs.recoverability[reqIndex];
    rows.push(<KeyComponents key="k" value={r.key} />);
  }
  const extra: [string, ReactNode][] = prop === "idempotency"
    ? [["result", <Tag key="r" variant={reqs.idempotency[reqIndex].result === "replay_consistent" ? "info" : "neutral"}>{reqs.idempotency[reqIndex].result}</Tag>]]
    : prop === "recoverability"
      ? [["completion", <Tag key="c">{reqs.recoverability[reqIndex].completion}</Tag>]]
      : [];
  return (
    <Frame kind="requirement" title={`${prop} #${reqIndex}`} subtitle={<span>declared on <IdLink id={opId} /></span>}>
      <Section title="key"><List items={rows} /></Section>
      {extra.length > 0 && <KeyValue rows={extra} />}
      <Obligations obKey={opId} filter={(ob) => ob.subject.kind === "operation" && ob.subject.requirement === reqIndex && propertyMatchesRequirement(ob.property, prop)} />
    </Frame>
  );
}

function TxStepDetail({ opId, txId, stepIndex }: { opId: Id; txId: Id; stepIndex: number }) {
  const { model, index } = useApp();
  const step = findTransaction(model.operations[opId], txId)?.steps[stepIndex];
  const sub = <span>step {stepIndex + 1} of <IdLink id={txId} /> in <IdLink id={opId} /></span>;
  if (!step) return <Frame kind="transaction step" title={`step ${stepIndex + 1}`} subtitle={sub} />;
  switch (step.kind) {
    case "read":
      return (
        <Frame kind="transaction step" title={`read · ${step.bind}`} subtitle={sub}>
          <KeyValue rows={[["object", <IdLink key="o" id={step.target.object} />], ["predicate", <PredicateView key="p" predicate={step.target.predicate} />]]} />
          <Section title="fields read">
            {step.fields.kind === "all" ? <Tag>all fields</Tag> : <List items={step.fields.fields.map((f, i) => <Mono key={i}>{pathText(f)}</Mono>)} />}
          </Section>
        </Frame>
      );
    case "write":
      return (
        <Frame kind="transaction step" title={`write · ${shortId(step.target.object)}`} subtitle={sub}>
          <KeyValue rows={[["object", <IdLink key="o" id={step.target.object} />], ["predicate", <PredicateView key="p" predicate={step.target.predicate} />]]} />
          <Section title="fields written"><List items={step.fields.map((f, i) => <Mono key={i}>{pathText(f)}</Mono>)} /></Section>
          <Section title="value provenance"><DerivationView value={step.values} /></Section>
        </Frame>
      );
    case "insert":
      return (
        <Frame kind="transaction step" title={`insert · ${shortId(step.object)}`} subtitle={sub}>
          <KeyValue rows={[["object", <IdLink key="o" id={step.object} />]]} />
          <Section title="value provenance"><DerivationView value={step.values} /></Section>
        </Frame>
      );
    case "delete":
      return (
        <Frame kind="transaction step" title={`delete · ${shortId(step.target.object)}`} subtitle={sub}>
          <KeyValue rows={[["object", <IdLink key="o" id={step.target.object} />], ["predicate", <PredicateView key="p" predicate={step.target.predicate} />]]} />
        </Frame>
      );
    case "lock":
      return (
        <Frame kind="transaction step" title={`lock · ${shortId(step.target.object)}`} subtitle={sub}>
          <KeyValue rows={[["object", <IdLink key="o" id={step.target.object} />], ["mode", <Tag key="m">{step.mode}</Tag>], ["order", <Tag key="r">{step.order.kind}</Tag>], ["predicate", <PredicateView key="p" predicate={step.target.predicate} />]]} />
        </Frame>
      );
    case "transition":
      return (
        <Frame kind="transaction step" title={`transition · ${shortId(step.transition)}`} subtitle={sub}>
          <KeyValue rows={[["machine", <IdLink key="m" id={step.machine} />], ["transition", <IdLink key="t" id={step.transition} />], ["subject", <IdLink key="s" id={step.subject.object} />], ["predicate", <PredicateView key="p" predicate={step.subject.predicate} />]]} />
          {Object.keys(step.effect_intents).length > 0 && (
            <Section title="bound intents">
              {Object.entries(step.effect_intents).map(([eid, intent]) => (
                <div key={eid} className="space-y-1">
                  <span className="flex flex-wrap items-center gap-1.5">
                    <IdLink id={eid} />
                    <span className="text-xs text-kumo-subtle">binds</span>
                    <IdLink id={intent.bind} />
                  </span>
                  <DerivationView value={intent.values} />
                </div>
              ))}
            </Section>
          )}
          <NavLink hash={hashes.machine(step.machine, step.transition)}>view in state machine →</NavLink>
        </Frame>
      );
    case "establish_effect_intent":
      return (
        <Frame kind="transaction step" title={`establish intent · ${shortId(step.bind)}`} subtitle={sub}>
          <KeyValue rows={[
            ["binds", <IdLink key="b" id={step.bind} />],
            ["effect", <IdLink key="e" id={step.effect_id} />],
            ["which is", effectSummary(model, index, step.effect_id)],
          ]} />
          <Section title="value provenance"><DerivationView value={step.values} /></Section>
        </Frame>
      );
    case "establish_transaction_output":
      return (
        <Frame kind="transaction step" title={`establish output · ${shortId(step.bind)}`} subtitle={sub}>
          <KeyValue rows={[
            ["binds", <IdLink key="b" id={step.bind} />],
            ["schema", <IdLink key="s" id={step.schema} />],
          ]} />
          <Section title="value provenance"><DerivationView value={step.values} /></Section>
        </Frame>
      );
  }
}

function EdgeDetail({ edge: e }: { edge: Edge }) {
  const asyncAt = "async_executed_at" in e ? new Set(e.async_executed_at) : new Set<string>();
  const allAsync = "executed_at" in e && e.executed_at.length > 0 && asyncAt.size === e.executed_at.length;
  const executed = "executed_at" in e ? (
    <Section title="executed at program steps" count={e.executed_at.length}>
      {e.executed_at.length
        ? (
          <>
            <List items={e.executed_at.map((loc, i) => (
              <span key={i} className="inline-flex items-center gap-1.5">
                <Mono>step {loc}</Mono>
                {asyncAt.has(loc) && <Tag variant="blue">async launch</Tag>}
              </span>
            ))} />
            {allAsync && (
              <Muted>every execution is an asynchronous launch: control never waits for this effect to complete; completion is established only where a join_all or race declares it</Muted>
            )}
          </>
        )
        : <span className="flex items-center gap-1.5"><Tag variant="warning">declared, not executed</Tag><Muted>no step of the program executes this effect</Muted></span>}
    </Section>
  ) : null;
  const via = "via_transition" in e && e.via_transition ? (
    <div className="text-sm text-kumo-subtle">
      Owned by transition <IdLink id={e.via_transition.transition} /> of <NavLink hash={hashes.machine(e.via_transition.machine)}>{shortId(e.via_transition.machine)}</NavLink>; it becomes intended when the transition commits.
    </div>
  ) : null;
  switch (e.kind) {
    case "publish":
      return (
        <Frame kind="publication edge" title={<span><IdLink id={e.operation} /> → <IdLink id={e.to} /></span>}>
          <KeyValue rows={[["effect", <IdLink key="e" id={e.effect} />], ["schema", <IdLink key="s" id={e.schema} />]]} />
          {via}{executed}
        </Frame>
      );
    case "subscribe":
      return (
        <Frame kind="subscription edge" title={<span><IdLink id={e.from} /> → <IdLink id={e.operation} /></span>}>
          <KeyValue rows={[["input", <IdLink key="i" id={e.input} />], ["delivery", <Tag key="d">{e.delivery}</Tag>], ["routing", <Tag key="r">{e.routing ?? "none"}</Tag>], ["pool", e.pool ? <IdLink key="p" id={e.pool} /> : "—"]]} />
          <Section title="consumed messages" count={e.schemas.length}><List items={e.schemas.map((s) => <IdLink key={s} id={s} />)} /></Section>
        </Frame>
      );
    case "request":
      return (
        <Frame kind="request edge" title={<span><IdLink id={e.operation} /> → <IdLink id={e.to} /></span>}>
          <KeyValue rows={[["effect", <IdLink key="e" id={e.effect} />], ["target input", <IdLink key="i" id={e.input} />], ["schema", <IdLink key="s" id={e.schema} />], ["retry", <Tag key="r">{e.retry}</Tag>]]} />
          {via}{executed}
        </Frame>
      );
    case "external":
      return (
        <Frame kind="external effect edge" title={<span><IdLink id={e.operation} /> → {e.to.slice(EXTERNAL_PREFIX.length)}</span>}
          description="The modeled system ends here; the checker cannot inspect the external implementation.">
          <KeyValue rows={[["effect", <IdLink key="e" id={e.effect} />], ["idempotency", <Tag key="i">{e.idempotency}</Tag>]]} />
          {executed}
        </Frame>
      );
    case "client":
      return (
        <Frame kind="client request" title={<span>clients → <IdLink id={e.operation} /></span>}
          description="No modeled operation issues this request; it enters the system from unmodeled callers.">
          <KeyValue rows={[["input", <IdLink key="i" id={e.input} />], ["schema", <IdLink key="s" id={e.schema} />]]} />
        </Frame>
      );
  }
}

function ClientDetail() {
  const { graph } = useApp();
  const edges = graph.edges.filter((e): e is Extract<Edge, { kind: "client" }> => e.kind === "client");
  return (
    <Frame kind="clients" title="unmodeled callers" description="Request inputs that no modeled operation invokes; they are the system's entry points.">
      <Section title="entry points" count={edges.length}>
        <List items={edges.map((e) => <span key={e.id} className="flex flex-wrap items-center gap-1.5"><IdLink id={e.operation} /><Tag>{shortId(e.schema)}</Tag></span>)} />
      </Section>
    </Frame>
  );
}

function ExternalDetail({ name }: { name: string }) {
  const { graph } = useApp();
  const edges = graph.edges.filter((e): e is Extract<Edge, { kind: "external" }> => e.kind === "external" && e.to === EXTERNAL_PREFIX + name);
  return (
    <Frame kind="external system" title={name} description="External dependency; the modeled system ends here.">
      <Section title="invoked by" count={edges.length}>
        <List items={edges.map((e) => <span key={e.id} className="flex flex-wrap items-center gap-1.5"><IdLink id={e.operation} /> via <IdLink id={e.effect} /><Tag>{e.idempotency}</Tag></span>)} />
      </Section>
    </Frame>
  );
}

// ---------------------------------------------------------------------
// L1 — the declared runtime realization
// ---------------------------------------------------------------------

function PoolDetail({ id }: { id: Id }) {
  const { model, graph } = useApp();
  const pool = model.runtime?.execution_pools?.[id];
  const node = graph.runtime.execution_pools.find((p) => p.id === id);
  if (!pool) return <Frame kind="execution pool" title={id} />;
  const assigned = node?.assigned ?? [];
  return (
    <Frame
      kind="execution pool"
      title={id}
      subtitle={<span>L1 · a population of interchangeable members</span>}
      description="How many members there are is not a conseqa fact: pool cardinality is an external scenario input, and every proof here is about one member's behaviour, not the population's size."
    >
      <FactNote fact={memberConcurrency(pool.member_concurrency)} />
      {assigned.length > 0 && (
        <Section title="boundaries assigned" count={assigned.length}>
          <p className="text-xs leading-relaxed text-kumo-subtle">
            These share an execution population. Sharing a pool relates their members, not their
            routing domains — no serialization follows from it on its own.
          </p>
          <List
            items={assigned.map((b) => (
              <span key={`${b.operation}/${b.input}`} className="flex flex-wrap items-center gap-1.5">
                <IdLink id={b.operation} />
                <span className="text-kumo-inactive">·</span>
                <IdLink id={b.input} />
              </span>
            ))}
          />
        </Section>
      )}
      <Citations id={id} />
    </Frame>
  );
}

function RouterDetail({ id }: { id: Id }) {
  const { model } = useApp();
  const router = model.runtime?.routers?.[id];
  if (!router) return <Frame kind="router" title={id} />;
  const pool = model.runtime?.execution_pools?.[router.pool];
  return (
    <Frame
      kind="router"
      title={id}
      subtitle={<span>L1 · realization of a request boundary</span>}
      description="A router decides which member of its pool owns an invocation domain. It does not make requests ordered, and it carries no queue: what it establishes is affinity, and only when it declares a routing key."
    >
      <KeyValue rows={[
        ["operation", <IdLink key="o" id={router.boundary.operation} />],
        ["input", <IdLink key="i" id={router.boundary.input} />],
        ["pool", <IdLink key="p" id={router.pool} />],
      ]} />
      <FactNote fact={requestRouting(router.routing?.key)} />
      {router.routing && <FactNote fact={memberAssignment(router.routing.member_assignment)} />}
      {pool && <FactNote fact={memberConcurrency(pool.member_concurrency)} />}
      <Citations id={id} />
    </Frame>
  );
}

function StorageLayoutDetail({ id }: { id: Id }) {
  const { model } = useApp();
  const layout = model.runtime?.storage_layouts?.[id];
  if (!layout) return <Frame kind="storage layout" title={id} />;
  const touchers = [...(objectAccesses(model).get(layout.object.object)?.keys() ?? [])];
  return (
    <Frame
      kind="storage layout"
      title={id}
      subtitle={<span>L1 · how one object is partitioned</span>}
      description="A partition key is neither the object's identity nor a routing key. It says where rows live, and nothing about which member executes an invocation or in what order — no proof of serialization or ordering rests on it."
    >
      <KeyValue rows={[
        ["data model", <IdLink key="d" id={layout.object.data_model} />],
        ["object", <IdLink key="o" id={layout.object.object} />],
        ["partition key", <Mono key="k">{layout.partition_key.map(pathText).join(", ")}</Mono>],
      ]} />
      {touchers.length > 0 && (
        <Section title="operations touching the object" count={touchers.length}>
          <List items={touchers.map((op) => <IdLink key={op} id={op} />)} />
        </Section>
      )}
      <Citations id={id} />
    </Frame>
  );
}

/** One operation's access to one object: whether it keys to the object's
 *  partition, which is the fact the access edge carries. */
function AccessDetail({ operation, object }: { operation: Id; object: Id }) {
  const { model } = useApp();
  const predicates = objectAccesses(model).get(object)?.get(operation) ?? [];
  const key = partitionKeyOf(model, object);
  const keyed = key !== null && accessKeysToPartition(predicates, key);
  return (
    <Frame
      kind="data access"
      title={<span><IdLink id={operation}>{shortId(operation)}</IdLink> → <IdLink id={object}>{shortId(object)}</IdLink></span>}
      subtitle={<span>{predicates.length} access{predicates.length === 1 ? "" : "es"} in <IdLink id={operation} /></span>}
    >
      <KeyValue rows={[
        ["object", <IdLink key="o" id={object} />],
        ["storage", key ? <Mono key="k">partitioned by {key.map(pathText).join(", ")}</Mono> : <Tag key="u">unpartitioned</Tag>],
        ["access", <Tag key="a" variant={keyed ? "success" : "warning"}>{keyed ? "keys to partition" : key ? "crosses partitions" : "no partition to key"}</Tag>],
      ]} />
      <p className="text-xs leading-relaxed text-kumo-subtle">
        {key === null
          ? "The object declares no storage layout, so there is no partition for the access to key to."
          : keyed
            ? "Every selector on this path pins the partition key, so the access stays within one partition."
            : "At least one selector on this path does not pin the partition key, so the access is not confined to one partition."}
      </p>
    </Frame>
  );
}
