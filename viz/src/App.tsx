import { TooltipProvider } from "@cloudflare/kumo/components/tooltip";

import { Navigator } from "./chrome/Navigator";
import { TopBar } from "./chrome/TopBar";
import { MachineView } from "./graph/MachineView";
import { OperationView } from "./graph/OperationView";
import { SystemView } from "./graph/SystemView";
import { EntityPage } from "./pages/EntityPage";
import { RuntimePage } from "./pages/RuntimePage";
import { TransactionPage } from "./pages/TransactionPage";
import { DetailPanel } from "./panels/DetailPanel";
import { ObligationsPanel } from "./panels/ObligationsPanel";
import type { Route } from "./lib/route";
import { AppStateProvider, useApp, type Theme } from "./state/AppState";
import type { PageData } from "./types/page";

export interface AppProps {
  data: PageData;

  /** Colour mode, when a host owns it. Given one, the app follows it and
   *  offers no toggle of its own; left out, it manages its own, as the
   *  self-contained page `conseqa-viz` writes does. */
  theme?: Theme;
}

export function App({ data, theme }: AppProps) {
  return (
    <TooltipProvider>
      <AppStateProvider data={data} theme={theme}>
        <Shell />
      </AppStateProvider>
    </TooltipProvider>
  );
}

/**
 * The layout. The canvas in the middle shows one page at a time — the
 * system graph by default, else the entity the address bar names — and
 * is the primary focus. The navigator on the left is the model as a
 * tree of those pages; the inspector on the right is the detail of
 * whatever is selected on the page; the obligations panel lists the
 * report. Below the `xl` breakpoint the side panels overlay the canvas
 * rather than squeezing it.
 */
function Shell() {
  const { route, detail, obligationsOpen, navOpen, report } = useApp();
  const showObligations = obligationsOpen && !!report;

  return (
    <div className="relative flex h-full bg-kumo-canvas text-kumo-default">
      {navOpen && (
        <aside className="absolute inset-y-0 left-0 z-10 w-[256px] shrink-0 border-r border-kumo-hairline bg-kumo-base shadow-xl xl:static xl:shadow-none">
          <Navigator />
        </aside>
      )}
      <main className="flex min-w-0 flex-1 flex-col">
        <TopBar />
        <div className="relative min-h-0 flex-1">
          <Page route={route} />
        </div>
      </main>
      {(detail || showObligations) && (
        <>
          {detail && (
            <aside
              className={`absolute inset-y-0 z-10 w-[380px] shrink-0 overflow-hidden border-l border-kumo-hairline bg-kumo-base shadow-xl xl:static xl:shadow-none ${showObligations ? "right-[400px]" : "right-0"}`}
            >
              <DetailPanel />
            </aside>
          )}
          {showObligations && (
            <aside className="absolute inset-y-0 right-0 z-10 w-[400px] shrink-0 overflow-hidden border-l border-kumo-hairline bg-kumo-base shadow-xl xl:static xl:shadow-none">
              <ObligationsPanel />
            </aside>
          )}
        </>
      )}
    </div>
  );
}

/** The page the route names. A component of its own at module scope —
 *  declared inside the shell it would be a new component type on every
 *  render, and React would remount the whole page, camera and all, at
 *  every selection. */
function Page({ route }: { route: Route }) {
  switch (route.view) {
    case "system": return <SystemView />;
    case "runtime": return <RuntimePage />;
    case "op": return <OperationView id={route.id} />;
    case "machine": return <MachineView id={route.id} highlight={route.highlight} />;
    case "tx": return <TransactionPage id={route.id} req={route.req} />;
    case "entity": return <EntityPage id={route.id} />;
  }
}
