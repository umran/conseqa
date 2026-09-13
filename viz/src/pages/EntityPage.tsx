import { Empty } from "@cloudflare/kumo/components/empty";
import { GraphIcon } from "@phosphor-icons/react";

import { pageKindOf } from "../lib/navigation";
import { DetailBody } from "../panels/DetailPanel";
import { FrameModeContext } from "../panels/frameMode";
import { useApp } from "../state/AppState";

/**
 * The page of an entity that has no view of its own: a service, a
 * topic, an outbox, a data model, an object, a schema, an L1
 * declaration, the clients, an external system. It shows what the
 * inspector would — the same facts, sections, and links — as a page in
 * the canvas, with room, so a reader can arrive at any entity by its
 * address and drill on from it.
 */
export function EntityPage({ id }: { id: string }) {
  const { index } = useApp();
  if (!pageKindOf(id, index)) {
    return (
      <div className="flex h-full items-center justify-center">
        <Empty size="sm" icon={<GraphIcon size={32} className="text-kumo-inactive" />} title={`nothing at ${id}`} />
      </div>
    );
  }
  return (
    <FrameModeContext.Provider value="page">
      <DetailBody target={{ id, ctx: {} }} />
    </FrameModeContext.Provider>
  );
}
