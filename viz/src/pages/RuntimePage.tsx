import { Badge } from "@cloudflare/kumo/components/badge";
import { Empty } from "@cloudflare/kumo/components/empty";
import { Table } from "@cloudflare/kumo/components/table";
import { Text } from "@cloudflare/kumo/components/text";
import { StackIcon } from "@phosphor-icons/react";

import { shortId } from "../lib/ids";
import { IdLink, Mono, SectionCard, SectionEmpty } from "../panels/parts";
import { useApp } from "../state/AppState";
import type { Edge } from "../types/graph";

/**
 * The runtime realization, L1, as one page: every pool, router, storage
 * layout, and transport fact the model declares, each a row that leads
 * to its own page. What is not here is as important as what is — an
 * absent declaration is an absent fact, never a default — and nothing
 * here is commit-order evidence.
 */
export function RuntimePage() {
  const { graph, runtime } = useApp();
  const { execution_pools: pools, routers, storage_layouts: layouts } = graph.runtime;
  const topics = graph.topics.filter((t) => t.topic_scoped_transport);
  const subscriptions = graph.edges.filter(
    (e): e is Extract<Edge, { kind: "subscribe" }> => e.kind === "subscribe" && e.pool !== null,
  );
  const outboxes = graph.edges.filter(
    (e): e is Extract<Edge, { kind: "outbox_consume" }> => e.kind === "outbox_consume" && e.pool !== null,
  );

  if (!runtime.declared) {
    return (
      <div className="flex h-full items-center justify-center">
        <Empty
          size="sm"
          icon={<StackIcon size={32} className="text-kumo-inactive" />}
          title="this model declares no runtime facts"
          description="Absence is the absence of a fact, not a realization without these properties."
        />
      </div>
    );
  }

  const none = <span className="text-kumo-inactive">—</span>;

  return (
    <div className="h-full overflow-auto">
      <div className="mx-auto max-w-[1240px] space-y-6 p-6">
        <header className="space-y-3 border-b border-kumo-hairline pb-5">
          <div className="text-[11px] font-semibold uppercase tracking-wider text-kumo-subtle">runtime · L1</div>
          <Text variant="heading" size="lg" as="h1">runtime realization</Text>
          <p className="max-w-3xl text-sm leading-relaxed text-kumo-default">
            One realization of the application machine: where invocations execute, how messages travel, and how
            persistent data is laid out. L1 describes placement, transport, grouping, precedence, and runtime
            capacity. It provides no serializability or ordering guarantee — no L1 fact is commit-order
            evidence — and a proof that consumed one of these facts is marked runtime-dependent.
          </p>
        </header>

        <SectionCard title="Execution pools" count={pools.length} hint="populations of interchangeable members; cardinality is never a fact here">
          {pools.length ? (
            <div className="overflow-x-auto">
              <Table>
                <Table.Header variant="compact">
                  <Table.Row>
                    <Table.Head>pool</Table.Head>
                    <Table.Head>member concurrency</Table.Head>
                    <Table.Head>boundaries assigned</Table.Head>
                  </Table.Row>
                </Table.Header>
                <Table.Body>
                  {pools.map((p) => (
                    <Table.Row key={p.id}>
                      <Table.Cell className="whitespace-nowrap"><IdLink id={p.id}>{shortId(p.id)}</IdLink></Table.Cell>
                      <Table.Cell><Badge variant="neutral">{p.member_concurrency}</Badge></Table.Cell>
                      <Table.Cell>
                        {p.assigned.length ? (
                          <ul className="space-y-0.5">
                            {p.assigned.map((b) => (
                              <li key={`${b.operation}/${b.input}`} className="whitespace-nowrap text-xs">
                                <IdLink id={b.operation}>{shortId(b.operation)}</IdLink>
                                <span className="text-kumo-inactive"> · </span>
                                <IdLink id={b.input}>{shortId(b.input)}</IdLink>
                              </li>
                            ))}
                          </ul>
                        ) : none}
                      </Table.Cell>
                    </Table.Row>
                  ))}
                </Table.Body>
              </Table>
            </div>
          ) : <SectionEmpty>none declared</SectionEmpty>}
        </SectionCard>

        <SectionCard title="Routers" count={routers.length} hint="how a request boundary's invocations are placed on a pool's members — affinity at most, never order">
          {routers.length ? (
            <div className="overflow-x-auto">
              <Table>
                <Table.Header variant="compact">
                  <Table.Row>
                    <Table.Head>router</Table.Head>
                    <Table.Head>boundary</Table.Head>
                    <Table.Head>pool</Table.Head>
                    <Table.Head>routing key</Table.Head>
                    <Table.Head>member assignment</Table.Head>
                  </Table.Row>
                </Table.Header>
                <Table.Body>
                  {routers.map((r) => (
                    <Table.Row key={r.id}>
                      <Table.Cell className="whitespace-nowrap"><IdLink id={r.id}>{shortId(r.id)}</IdLink></Table.Cell>
                      <Table.Cell className="whitespace-nowrap text-xs">
                        <IdLink id={r.operation}>{shortId(r.operation)}</IdLink>
                        <span className="text-kumo-inactive"> · </span>
                        <IdLink id={r.input}>{shortId(r.input)}</IdLink>
                      </Table.Cell>
                      <Table.Cell className="whitespace-nowrap"><IdLink id={r.pool}>{shortId(r.pool)}</IdLink></Table.Cell>
                      <Table.Cell>{r.routing_key.length ? <Mono>{r.routing_key.join(", ")}</Mono> : <span className="text-xs text-kumo-inactive">none declared</span>}</Table.Cell>
                      <Table.Cell>{r.member_assignment ? <Badge variant="neutral">{r.member_assignment}</Badge> : none}</Table.Cell>
                    </Table.Row>
                  ))}
                </Table.Body>
              </Table>
            </div>
          ) : <SectionEmpty>none declared</SectionEmpty>}
        </SectionCard>

        <SectionCard title="Subscription dispatch" count={subscriptions.length} hint="how a subscription's deliveries reach a pool — delivery, transport, and affinity facts">
          {subscriptions.length ? (
            <div className="overflow-x-auto">
              <Table>
                <Table.Header variant="compact">
                  <Table.Row>
                    <Table.Head>topic → operation</Table.Head>
                    <Table.Head>input</Table.Head>
                    <Table.Head>delivery</Table.Head>
                    <Table.Head>grouping</Table.Head>
                    <Table.Head>ordering</Table.Head>
                    <Table.Head>routing</Table.Head>
                    <Table.Head>pool</Table.Head>
                  </Table.Row>
                </Table.Header>
                <Table.Body>
                  {subscriptions.map((e) => (
                    <Table.Row key={e.id}>
                      <Table.Cell className="whitespace-nowrap text-xs">
                        <IdLink id={e.from}>{shortId(e.from)}</IdLink>
                        <span className="text-kumo-inactive"> → </span>
                        <IdLink id={e.operation}>{shortId(e.operation)}</IdLink>
                      </Table.Cell>
                      <Table.Cell className="whitespace-nowrap"><IdLink id={e.input}>{shortId(e.input)}</IdLink></Table.Cell>
                      <Table.Cell><Badge variant="neutral">{e.delivery}</Badge></Table.Cell>
                      <Table.Cell><Badge variant="neutral">{e.grouping}</Badge></Table.Cell>
                      <Table.Cell><Badge variant="neutral">{e.ordering}</Badge></Table.Cell>
                      <Table.Cell>
                        <Badge variant="neutral">{e.routing ?? "none"}</Badge>
                        {e.member_assignment && <span className="ml-1 text-xs text-kumo-subtle">{e.member_assignment}</span>}
                      </Table.Cell>
                      <Table.Cell className="whitespace-nowrap">{e.pool ? <IdLink id={e.pool}>{shortId(e.pool)}</IdLink> : none}</Table.Cell>
                    </Table.Row>
                  ))}
                </Table.Body>
              </Table>
            </div>
          ) : <SectionEmpty>none declared</SectionEmpty>}
        </SectionCard>

        <SectionCard title="Outbox consumption" count={outboxes.length} hint="how an outbox's exclusive consumer is realized — partitioning, ordering, routing, batching">
          {outboxes.length ? (
            <div className="overflow-x-auto">
              <Table>
                <Table.Header variant="compact">
                  <Table.Row>
                    <Table.Head>outbox → operation</Table.Head>
                    <Table.Head>partitioning</Table.Head>
                    <Table.Head>ordering</Table.Head>
                    <Table.Head>routing</Table.Head>
                    <Table.Head>batching</Table.Head>
                    <Table.Head>pool</Table.Head>
                  </Table.Row>
                </Table.Header>
                <Table.Body>
                  {outboxes.map((e) => (
                    <Table.Row key={e.id}>
                      <Table.Cell className="whitespace-nowrap text-xs">
                        <IdLink id={e.from}>{shortId(e.from)}</IdLink>
                        <span className="text-kumo-inactive"> → </span>
                        <IdLink id={e.operation}>{shortId(e.operation)}</IdLink>
                      </Table.Cell>
                      <Table.Cell><Badge variant="neutral">{e.partitioning ?? "—"}</Badge></Table.Cell>
                      <Table.Cell><Badge variant="neutral">{e.ordering ?? "—"}</Badge></Table.Cell>
                      <Table.Cell>
                        <Badge variant="neutral">{e.routing ?? "none"}</Badge>
                        {e.member_assignment && <span className="ml-1 text-xs text-kumo-subtle">{e.member_assignment}</span>}
                      </Table.Cell>
                      <Table.Cell><Badge variant="neutral">{e.batching ?? "—"}</Badge></Table.Cell>
                      <Table.Cell className="whitespace-nowrap">{e.pool ? <IdLink id={e.pool}>{shortId(e.pool)}</IdLink> : none}</Table.Cell>
                    </Table.Row>
                  ))}
                </Table.Body>
              </Table>
            </div>
          ) : <SectionEmpty>none declared</SectionEmpty>}
        </SectionCard>

        <SectionCard title="Topic transport" count={topics.length} hint="grouping and precedence a topic's transport provides, when declared on the topic">
          {topics.length ? (
            <div className="overflow-x-auto">
              <Table>
                <Table.Header variant="compact">
                  <Table.Row>
                    <Table.Head>topic</Table.Head>
                    <Table.Head>grouping</Table.Head>
                    <Table.Head>ordering</Table.Head>
                  </Table.Row>
                </Table.Header>
                <Table.Body>
                  {topics.map((t) => (
                    <Table.Row key={t.id}>
                      <Table.Cell className="whitespace-nowrap"><IdLink id={t.id}>{shortId(t.id)}</IdLink></Table.Cell>
                      <Table.Cell><Badge variant="neutral">{t.grouping}</Badge></Table.Cell>
                      <Table.Cell><Badge variant="neutral">{t.ordering}</Badge></Table.Cell>
                    </Table.Row>
                  ))}
                </Table.Body>
              </Table>
            </div>
          ) : <SectionEmpty>no topic-scoped transport declared</SectionEmpty>}
        </SectionCard>

        <SectionCard title="Storage layouts" count={layouts.length} hint="how an object is partitioned — where rows live, never an identity or a routing key">
          {layouts.length ? (
            <div className="overflow-x-auto">
              <Table>
                <Table.Header variant="compact">
                  <Table.Row>
                    <Table.Head>layout</Table.Head>
                    <Table.Head>data model</Table.Head>
                    <Table.Head>object</Table.Head>
                    <Table.Head>partition key</Table.Head>
                  </Table.Row>
                </Table.Header>
                <Table.Body>
                  {layouts.map((s) => (
                    <Table.Row key={s.id}>
                      <Table.Cell className="whitespace-nowrap"><IdLink id={s.id}>{shortId(s.id)}</IdLink></Table.Cell>
                      <Table.Cell className="whitespace-nowrap"><IdLink id={s.data_model}>{shortId(s.data_model)}</IdLink></Table.Cell>
                      <Table.Cell className="whitespace-nowrap"><IdLink id={s.object}>{shortId(s.object)}</IdLink></Table.Cell>
                      <Table.Cell><Mono>{s.partition_key.join(", ")}</Mono></Table.Cell>
                    </Table.Row>
                  ))}
                </Table.Body>
              </Table>
            </div>
          ) : <SectionEmpty>none declared</SectionEmpty>}
        </SectionCard>
      </div>
    </div>
  );
}
