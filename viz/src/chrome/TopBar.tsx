import { Badge } from "@cloudflare/kumo/components/badge";
import { Breadcrumbs } from "@cloudflare/kumo/components/breadcrumbs";
import { Button } from "@cloudflare/kumo/components/button";
import { Input } from "@cloudflare/kumo/components/input";
import { Switch } from "@cloudflare/kumo/components/switch";
import { Tooltip } from "@cloudflare/kumo/components/tooltip";
import { ListChecksIcon, MoonIcon, SunIcon } from "@phosphor-icons/react";

import { STATUS_GLYPH, statusCounts } from "../lib/obligations";
import { hashes } from "../lib/route";
import { useApp } from "../state/AppState";

/** The top bar sits in the content column and uses the pages' container,
 *  so its edges line up with the page headers below it.
 *
 *  Width is given away in order of how little it costs: the breadcrumbs
 *  go first (the page header below repeats where you are), then the
 *  model title truncates, then the filter narrows. The controls on the
 *  right keep their full size at every width — a control that cannot be
 *  reached is worse than one that is crowded. */
export function TopBar() {
  const app = useApp();
  const {
    data, model, report, reportIssue, route, search, obligationsOpen, runtime, showRuntime,
    theme, themeControllable,
  } = app;
  const counts = report ? statusCounts(report.obligations) : null;
  const tally = counts
    ? (["disproven", "unknown", "proven"] as const)
        .filter((s) => counts[s])
        .map((s) => `${STATUS_GLYPH[s]}${counts[s]}`)
        .join(" ")
    : "";

  return (
    // Container queries, not viewport ones: embedded in an application
    // the views often sit in a pane far narrower than the window, and a
    // bar that measured the window would lay itself out for room it does
    // not have.
    <header className="@container shrink-0 border-b border-kumo-hairline bg-kumo-base">
      <div className="mx-auto flex h-12 max-w-[1240px] items-center gap-3 px-4 @md:gap-4 @md:px-6">
        {/* The model, not the tool: the document title already reads
            "<model> · conseqa", and a host embedding these views has a
            name of its own in its chrome. */}
        <div className="flex min-w-0 items-baseline gap-2 text-sm">
          <span className="truncate font-medium text-kumo-strong">{data.title}</span>
          <span className="hidden shrink-0 text-xs text-kumo-subtle @sm:inline">
            rev {model.revision}
          </span>
          {report && report.model_revision !== null && report.model_revision !== model.revision && (
            <Badge variant="warning">report @ rev {report.model_revision}</Badge>
          )}
          {/* A refused report is said out loud. Silently dropping it left
              every verdict on the page reading indeterminate, which is
              the one thing a proof view must never do. */}
          {reportIssue && (
            <Tooltip
              content={`The loaded report is written in a vocabulary this build does not read (${reportIssue}). Regenerate it with this build of conseqa — no verdict is shown rather than one explained in terms the checker no longer uses.`}
              render={
                <span className="inline-flex shrink-0">
                  <Badge variant="error">report not rendered</Badge>
                </span>
              }
            />
          )}
        </div>

        <div className="hidden min-w-0 flex-1 items-center gap-3 overflow-hidden @3xl:flex">
          <Breadcrumbs size="sm">
            {route.view === "system" ? (
              <Breadcrumbs.Current>system</Breadcrumbs.Current>
            ) : (
              <>
                <Breadcrumbs.Link href={hashes.system()}>system</Breadcrumbs.Link>
                <Breadcrumbs.Separator />
                <Breadcrumbs.Current>{route.view === "op" ? "operation" : "machine"}</Breadcrumbs.Current>
                <Breadcrumbs.Separator />
                <Breadcrumbs.Current>{route.id}</Breadcrumbs.Current>
              </>
            )}
          </Breadcrumbs>
        </div>

        <div className="ml-auto flex min-w-0 items-center gap-2 @md:gap-3">
          {/* The filter dims vertices in the system graph; it has no effect
              on the other pages, so it only appears where it acts. It is
              the one control here that stays usable while narrowing, so
              it absorbs the squeeze. */}
          {route.view === "system" && (
            <Input
              size="sm"
              className="w-44 min-w-16 shrink"
              placeholder="filter ids…"
              value={search}
              onChange={(e) => app.setSearch(e.target.value)}
              aria-label="Filter ids"
            />
          )}
          {/* The layer control. L0 has no switch: the application machine
              is the model, not an overlay on it. */}
          {route.view === "system" && (
            <span className="flex shrink-0 items-center gap-2">
              <Tooltip
                content="L0 — the abstract application machine. Always drawn: it is what the model says exists."
                render={
                  <span className="inline-flex">
                    <Badge variant="neutral">L0</Badge>
                  </span>
                }
              />
              <Tooltip
                content={
                  runtime.declared
                    ? "L1 — one runtime realization of that machine: execution pools, routing, transport and storage topology. Hiding it hides the drawing, never the declaration."
                    : "This model declares no L1 facts. Absence is the absence of a fact, not a realization without these properties."
                }
                render={
                  <span className="inline-flex">
                    <Switch
                      size="sm"
                      label="L1"
                      controlFirst={false}
                      checked={showRuntime && runtime.declared}
                      disabled={!runtime.declared}
                      onCheckedChange={app.setShowRuntime}
                      aria-label="Draw the L1 runtime realization"
                    />
                  </span>
                }
              />
            </span>
          )}
          {report && (
            <Button
              className="shrink-0"
              variant={obligationsOpen ? "primary" : "secondary"}
              size="sm"
              icon={ListChecksIcon}
              aria-pressed={obligationsOpen}
              aria-expanded={obligationsOpen}
              title={obligationsOpen ? "Close the obligations panel" : "Open the obligations panel"}
              onClick={() => app.setObligationsOpen(!obligationsOpen)}
            >
              <span className="hidden @lg:inline">Obligations</span>
              {tally && (
                <span
                  className={`font-mono text-xs @lg:ml-1.5 ${obligationsOpen ? "opacity-80" : "text-kumo-subtle"}`}
                >
                  {tally}
                </span>
              )}
            </Button>
          )}
          {/* Absent when a host owns the colour mode: its control is the
              only one, so the two can never disagree. */}
          {themeControllable && (
            <Tooltip
              content={theme === "dark" ? "Switch to light mode" : "Switch to dark mode"}
              render={
                <Button
                  className="shrink-0"
                  variant="ghost"
                  size="sm"
                  shape="square"
                  icon={theme === "dark" ? SunIcon : MoonIcon}
                  aria-label="Toggle color mode"
                  onClick={() => app.setTheme(theme === "dark" ? "light" : "dark")}
                />
              }
            />
          )}
        </div>
      </div>
    </header>
  );
}
