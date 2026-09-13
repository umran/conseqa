import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";

import { modelBindings, type ModelBindings } from "../lib/bindings";
import { citedIds } from "../lib/citations";
import { indexTransactionProofs, type TransactionProofIndex } from "../lib/transactionProofs";
import { buildIndex, type ModelIndex } from "../lib/index";
import { pageHash } from "../lib/navigation";
import { buildObligationIndex, reportRejection, type ObligationIndex } from "../lib/obligations";
import { runtimeFacts, type RuntimeFacts } from "../lib/runtime";
import { hashes, impliedSubject, navigate, routeKey, routeSubject, useRoute, type Route } from "../lib/route";
import type { Graph } from "../types/graph";
import type { Id, Model, RequirementKind } from "../types/model";
import type { PageData } from "../types/page";
import type { Obligation, ProverReport } from "../types/report";

export interface DetailContext {
  /** A declared requirement: an operation family is addressed by the
   *  operation and an index; a transaction family also names the
   *  inline transaction that declares it. */
  req?: { prop: RequirementKind; index: number; transaction?: Id };
  txStep?: { op: Id; tx: Id; index: number };
  /** A program step, by its location in the operation's program. */
  step?: { op: Id; location: string };
  edge?: boolean;
  /** A data-access edge: one operation's access to one object. */
  access?: { operation: Id; object: Id };
}

export interface DetailTarget {
  id: string;
  ctx: DetailContext;
}

/** The selection key of a requirement row on the operation page: the
 *  family and index, with the declaring transaction for a transaction
 *  family. Shared by the table that draws the rows and the obligation
 *  focus that lands on one. */
export function requirementKey(prop: string, index: number, transaction?: Id): string {
  return transaction !== undefined ? `req:${prop}:${transaction}:${index}` : `req:${prop}:${index}`;
}

export type Theme = "dark" | "light";

interface AppState {
  data: PageData;
  model: Model;
  graph: Graph;
  /** The report, once it is one this build can render. A refused report
   *  is null here and named by `reportIssue`, so no surface can count
   *  verdicts another surface is not showing. */
  report: ProverReport | null;
  /** Why a loaded report is not being rendered, when one is not. */
  reportIssue: string | null;
  index: ModelIndex;
  /** Every id the model declares, L0 and L1 — what a verdict's prose is
   *  matched against when its citations are resolved. */
  knownIds: ReadonlySet<Id>;
  obligations: ObligationIndex;
  /** Obligations by the declaration their reasoning names — how a
   *  runtime-dependent proof is found from the L1 fact it rests on. */
  citations: ObligationIndex;
  /** The declared L1 realization, as the views draw it. */
  runtime: RuntimeFacts;
  /** The transaction proofs — the serializability and ordering
   *  arguments — indexed by the obligation and the requirement they
   *  belong to. */
  transactionProofs: TransactionProofIndex;
  /** Every binding each program defines and where it uses it, and every
   *  definition by name — how a name is drawn the same way at both ends. */
  bindings: ModelBindings;
  route: Route;

  selection: string | null;
  detail: DetailTarget | null;
  expandedTx: ReadonlySet<string>;
  search: string;
  obligationsOpen: boolean;
  /** Whether the navigator — the model as a tree of pages — is shown
   *  beside the canvas. */
  navOpen: boolean;
  /** Whether the L1 realization is drawn. L0 is always drawn: the
   *  application machine is the model, and the realization is a layer
   *  over it. */
  showRuntime: boolean;
  /** Whether the system view draws the transaction conflict overlay:
   *  an arc between two operations whose transactions may conflict,
   *  coloured by whether every dependency between them is commit-ordered
   *  by a declared fact. An L0 reading of the model — never topology. */
  showConflicts: boolean;
  theme: Theme;
  /** False when a host owns the colour mode, so the app offers no
   *  control of its own. */
  themeControllable: boolean;
  fitRequest: number;

  /** Selects a graph element and, when given, shows its detail. */
  select: (key: string | null, detail?: DetailTarget) => void;
  openDetail: (id: string, ctx?: DetailContext) => void;
  /** Shows an entity: in the main canvas when it has a page of its own,
   *  else in the inspector on the page it belongs to. */
  openEntity: (id: string) => void;
  closeDetail: () => void;
  toggleTx: (key: string) => void;
  setSearch: (value: string) => void;
  setObligationsOpen: (value: boolean) => void;
  setNavOpen: (value: boolean) => void;
  setShowRuntime: (value: boolean) => void;
  setShowConflicts: (value: boolean) => void;
  setTheme: (value: Theme) => void;
  requestFit: () => void;
  /** Navigates to a view, applying a selection once it has rendered. */
  navigateTo: (hash: string, selection?: string) => void;
  focusSubject: (obligation: Obligation) => void;
}

const Context = createContext<AppState | null>(null);

const THEME_KEY = "conseqa-viz-theme";
const NAV_KEY = "conseqa-viz-nav";

function initialTheme(): Theme {
  const stored = window.localStorage.getItem(THEME_KEY);
  return stored === "light" ? "light" : "dark";
}

/** The navigator starts open where there is room for it beside the
 *  canvas — the tree is how a reader learns what the model contains —
 *  and closed in a pane too narrow to hold both, where it would cover
 *  the drawing. A reader's own choice is remembered either way. */
function initialNavOpen(): boolean {
  try {
    const stored = window.localStorage.getItem(NAV_KEY);
    if (stored === "open") return true;
    if (stored === "closed") return false;
  } catch {
    // Storage may be unavailable; fall through to the width rule.
  }
  return window.innerWidth >= 1280;
}

export interface AppStateProviderProps {
  data: PageData;

  /**
   * Colour mode, when a host owns it.
   *
   * The self-contained page `conseqa-viz` writes has no host, so the
   * app manages the mode itself: it restores the stored choice, sets
   * `data-mode` on the document, and offers a toggle. Embedded in an
   * application that has a colour mode of its own, that would be a
   * second, competing control — so passing `theme` hands ownership over:
   * the app reads the mode from the host, touches neither the document
   * nor storage, and shows no toggle.
   */
  theme?: Theme;

  children: ReactNode;
}

export function AppStateProvider({ data, theme: hostTheme, children }: AppStateProviderProps) {
  const route = useRoute();
  const key = routeKey(route);
  const implied = impliedSubject(route);

  const index = useMemo(() => buildIndex(data.model), [data.model]);
  const runtime = useMemo(() => runtimeFacts(data.model, data.graph), [data.model, data.graph]);
  const transactionProofs = useMemo(() => indexTransactionProofs(data), [data]);
  const bindings = useMemo(() => modelBindings(data.model), [data.model]);

  // A report this build cannot read is dropped here, once, rather than
  // being half-rendered: the panel would list verdicts the graph could
  // not colour, and the top bar would tally verdicts no view showed.
  const knownIds = useMemo(() => new Set(index.keys()), [index]);
  const reportIssue = useMemo(() => reportRejection(data.report), [data.report]);
  const report = reportIssue ? null : data.report;
  const obligations = useMemo(() => buildObligationIndex(report), [report]);

  // The declarations each verdict names, indexed the other way round: an
  // L1 fact answers which proofs would have to be re-examined if it
  // changed (§61).
  const citations = useMemo(() => {
    const map: ObligationIndex = new Map();
    if (!report) return map;
    for (const ob of report.obligations) {
      for (const id of citedIds(ob, knownIds)) {
        const list = map.get(id);
        if (list) list.push(ob);
        else map.set(id, [ob]);
      }
    }
    return map;
  }, [report, knownIds]);

  const [selection, setSelection] = useState<string | null>(null);
  const [detail, setDetail] = useState<DetailTarget | null>(null);
  const [expandedTx, setExpandedTx] = useState<ReadonlySet<string>>(() => new Set());
  const [search, setSearch] = useState("");
  const [obligationsOpen, setObligationsOpen] = useState(false);
  const [navOpen, setNavOpenState] = useState(initialNavOpen);
  // Drawn by default wherever there is anything to draw: the hierarchy is
  // the model, and a layer hidden until asked for reads as an extra.
  const [showRuntime, setShowRuntimeState] = useState(runtime.declared);
  // On by default for the same reason: a declared argument is part of
  // what the model says, and the overlay is how the system view says it.
  const [showConflicts, setShowConflictsState] = useState(true);
  const [ownTheme, setOwnTheme] = useState<Theme>(initialTheme);
  const [fitRequest, setFitRequest] = useState(0);

  // A host that supplies the mode owns it entirely; `setTheme` is inert
  // and the toggle is not offered, so nothing can drive the two apart.
  const themeControllable = hostTheme === undefined;
  const theme = hostTheme ?? ownTheme;

  const pendingSelection = useRef<string | null>(null);

  // A route change resets the selection to whatever is pending from a
  // cross-view focus, else to the subject the route itself names. The
  // detail panel survives so links keep their context — except when it
  // is showing the very entity the new page is about, which the page
  // now shows in full — and when the route names a subject an open
  // panel is retargeted to it, so history navigation and deep links
  // show what the address bar says.
  const subject = routeSubject(route);
  useEffect(() => {
    const pending = pendingSelection.current;
    pendingSelection.current = null;
    setSelection(pending ?? (implied ? `t:${implied}` : null));
    if (subject !== null) {
      setDetail((current) => (current && current.id === subject && !current.ctx.txStep && !current.ctx.req ? null : current));
    }
    if (pending === null && implied) {
      setDetail((current) => (current ? { id: implied, ctx: {} } : current));
    }
  }, [key, implied, subject]);

  useEffect(() => {
    try {
      window.localStorage.setItem(NAV_KEY, navOpen ? "open" : "closed");
    } catch {
      // Storage may be unavailable; the choice then lasts the session.
    }
  }, [navOpen]);

  // The document belongs to whoever owns the mode: a host that supplies
  // one has already dressed the page, and writing `data-mode` or the
  // stored choice from here would fight it.
  useEffect(() => {
    if (!themeControllable) return;
    const root = document.documentElement;
    if (ownTheme === "dark") root.setAttribute("data-mode", "dark");
    else root.removeAttribute("data-mode");
    window.localStorage.setItem(THEME_KEY, ownTheme);
  }, [ownTheme, themeControllable]);

  useEffect(() => {
    document.title = `${data.title} · conseqa`;
  }, [data.title]);

  const setTheme = useCallback(
    (next: Theme) => {
      if (themeControllable) setOwnTheme(next);
    },
    [themeControllable],
  );

  const select = useCallback((next: string | null, target?: DetailTarget) => {
    setSelection(next);
    if (target) setDetail(target);
  }, []);

  const openDetail = useCallback((id: string, ctx: DetailContext = {}) => {
    setDetail({ id, ctx });
  }, []);

  const closeDetail = useCallback(() => {
    setDetail(null);
    setSelection(null);
    // The address bar must not keep naming a selection the page no
    // longer shows.
    if (route.view === "machine" && route.highlight) navigate(hashes.machine(route.id));
  }, [route]);

  const toggleTx = useCallback((txKey: string) => {
    setExpandedTx((current) => {
      const next = new Set(current);
      if (next.has(txKey)) next.delete(txKey);
      else next.add(txKey);
      return next;
    });
  }, []);

  const requestFit = useCallback(() => setFitRequest((n) => n + 1), []);

  // The navigator shares the row with the canvas, so showing or hiding
  // it changes the canvas's width and the drawing is re-fitted to it.
  const setNavOpen = useCallback((value: boolean) => {
    setNavOpenState(value);
    setFitRequest((n) => n + 1);
  }, []);

  // Showing or hiding a layer changes how much drawing there is, so the
  // view is re-fitted to it: a band that appears off-screen has not
  // appeared.
  const setShowRuntime = useCallback((value: boolean) => {
    setShowRuntimeState(value);
    setFitRequest((n) => n + 1);
  }, []);

  // The overlay's arcs rise above the cards, so the drawing's extent
  // changes with it and the view is re-fitted the same way.
  const setShowConflicts = useCallback((value: boolean) => {
    setShowConflictsState(value);
    setFitRequest((n) => n + 1);
  }, []);

  const navigateTo = useCallback((hash: string, nextSelection?: string) => {
    if (window.location.hash === hash) {
      // Already there: no route change will apply the selection for us.
      if (nextSelection !== undefined) setSelection(nextSelection);
      return;
    }
    pendingSelection.current = nextSelection ?? null;
    navigate(hash);
  }, []);

  const openEntity = useCallback(
    (id: string) => {
      const hash = pageHash(id, index);
      if (hash) navigateTo(hash);
      else openDetail(id);
    },
    [index, navigateTo, openDetail],
  );

  const focusSubject = useCallback(
    (ob: Obligation) => {
      const s = ob.subject;
      switch (s.kind) {
        case "operation": {
          const prop = ob.property.kind === "result_replay" ? "idempotency" : ob.property.kind;
          navigateTo(
            hashes.op(s.operation),
            s.requirement !== undefined ? requirementKey(prop, s.requirement) : undefined,
          );
          break;
        }
        case "transaction": {
          // A transaction's page carries its requirements with their
          // arguments drawn in full; an obligation on one requirement
          // opens the page on that requirement.
          const prop = ob.property.kind;
          const req =
            s.requirement !== undefined && (prop === "transaction_serializability" || prop === "transaction_ordering")
              ? { prop, index: s.requirement }
              : null;
          navigateTo(hashes.tx(s.transaction, req));
          break;
        }
        case "state_machine":
          navigateTo(hashes.machine(s.machine, s.transition));
          break;
        case "topic":
          navigateTo(hashes.entity("topic", s.topic));
          break;
        case "object":
          navigateTo(hashes.entity("object", s.object));
          break;
      }
    },
    [navigateTo],
  );

  const value = useMemo<AppState>(
    () => ({
      data,
      model: data.model,
      graph: data.graph,
      report,
      reportIssue,
      index,
      knownIds,
      obligations,
      citations,
      runtime,
      transactionProofs,
      bindings,
      route,
      selection,
      detail,
      expandedTx,
      search,
      obligationsOpen,
      navOpen,
      showRuntime,
      showConflicts,
      theme,
      themeControllable,
      fitRequest,
      select,
      openDetail,
      openEntity,
      closeDetail,
      toggleTx,
      setSearch,
      setObligationsOpen,
      setNavOpen,
      setShowRuntime,
      setShowConflicts,
      setTheme,
      requestFit,
      navigateTo,
      focusSubject,
    }),
    [
      data, report, reportIssue, index, knownIds, obligations, citations, runtime, transactionProofs, bindings, route,
      selection, detail, expandedTx, search, obligationsOpen, navOpen, showRuntime, showConflicts, theme,
      themeControllable, fitRequest, select, openDetail, openEntity, closeDetail, toggleTx,
      setTheme, setNavOpen, setShowRuntime, setShowConflicts, requestFit, navigateTo, focusSubject,
    ],
  );

  return <Context.Provider value={value}>{children}</Context.Provider>;
}

export function useApp(): AppState {
  const value = useContext(Context);
  if (!value) throw new Error("useApp must be used within AppStateProvider");
  return value;
}

/** Obligations anchored to a graph entity, or none without a report. */
export function useObligationsAt(key: string): Obligation[] {
  const { obligations } = useApp();
  return obligations.get(key) ?? [];
}

/** Obligations whose reasoning names this declaration. For an L1 fact
 *  these are the proofs that rest on it, and so the proofs a change to
 *  the runtime topology would put back in question. */
export function useCitations(id: string): Obligation[] {
  const { citations } = useApp();
  return citations.get(id) ?? [];
}
