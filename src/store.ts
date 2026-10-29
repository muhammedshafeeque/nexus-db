import { create } from "zustand";
import { toast } from "sonner";
import { api, errorText } from "./lib/api";
import { EMPTY_ENTRA, ENGINE_META, flavorOf } from "./lib/engines";
import { buildChanges, emptyEdits, pendingCount, reattach, withoutApplied } from "./lib/edits";
import type { ApplyReport, ConnectionConfig, EditInfo, ExecResponse, PendingEdits, ResultSet, SchemaDoc, TreeNode } from "./lib/types";

export interface Tab {
  id: string;
  kind: "query" | "structure" | "docs";
  title: string;
  connId: string;
  database?: string | null;
  schema?: string | null;
  query: string;
  path?: string[];
  result?: ExecResponse;
  structure?: ResultSet;
  docs?: SchemaDoc;
  error?: string;
  running: boolean;
  activeSet: number;
  /** table / collection the tab was opened from – makes its rows editable */
  source?: string[];
  editInfo?: EditInfo | null;
  edits?: PendingEdits;
  saving?: boolean;
}

export interface LiveState {
  status: "connecting" | "connected" | "error";
  version?: string;
  error?: string;
}

export interface HistoryItem {
  id: string;
  connId: string;
  query: string;
  at: number;
  ms?: number;
  ok: boolean;
  rows?: number;
}

type Theme = "dark" | "light";

interface State {
  connections: ConnectionConfig[];
  live: Record<string, LiveState>;
  tabs: Tab[];
  activeTabId: string | null;
  nodes: Record<string, TreeNode[]>;
  expanded: Record<string, boolean>;
  loading: Record<string, boolean>;
  history: HistoryItem[];
  theme: Theme;
  rowLimit: number;
  dialog: { open: boolean; editing: ConnectionConfig | null };
  paletteOpen: boolean;
  importDialog: { open: boolean; connId?: string; path?: string[] };
  dumpDialog: { open: boolean; mode: DumpMode; connId?: string; database?: string };

  loadConnections(): Promise<void>;
  saveConnection(c: ConnectionConfig): Promise<ConnectionConfig>;
  deleteConnection(id: string): Promise<void>;
  connect(id: string): Promise<boolean>;
  disconnect(id: string): Promise<void>;
  loadChildren(connId: string, path: string[], force?: boolean): Promise<TreeNode[]>;
  toggleNode(connId: string, path: string[]): Promise<void>;
  openQueryTab(connId: string, opts?: Partial<Pick<Tab, "query" | "database" | "schema" | "title" | "source">>, run?: boolean): string;
  openObject(connId: string, path: string[]): Promise<void>;
  openStructure(connId: string, path: string[]): Promise<void>;
  openDocs(connId: string, database?: string | null, schema?: string | null): Promise<void>;
  loadDocs(tabId: string): Promise<void>;
  closeTab(id: string): void;
  closeOtherTabs(id: string): void;
  setActive(id: string): void;
  updateTab(id: string, patch: Partial<Tab>): void;
  runTab(id: string, query?: string): Promise<void>;
  setTheme(t: Theme): void;
  setRowLimit(n: number): void;
  openDialog(editing?: ConnectionConfig | null): void;
  closeDialog(): void;
  setPalette(open: boolean): void;
  setEdits(tabId: string, edits: PendingEdits): void;
  discardEdits(tabId: string): void;
  saveEdits(tabId: string, dryRun?: boolean): Promise<ApplyReport | null>;
  openImport(connId?: string, path?: string[]): void;
  closeImport(): void;
  openDump(mode: DumpMode, connId?: string, database?: string): void;
  closeDump(): void;
}

export type DumpMode = "dump" | "restore";

export const nodeKey = (connId: string, path: string[]) => [connId, ...path].join("\u001f");

const uid = () => Math.random().toString(36).slice(2, 10);

function readLocal<T>(key: string, fallback: T): T {
  try {
    const v = localStorage.getItem(key);
    return v ? (JSON.parse(v) as T) : fallback;
  } catch {
    return fallback;
  }
}

function writeLocal(key: string, value: unknown) {
  try {
    localStorage.setItem(key, JSON.stringify(value));
  } catch {
    /* storage unavailable */
  }
}

let counter = 0;

export const useStore = create<State>((set, get) => ({
  connections: [],
  live: {},
  tabs: [],
  activeTabId: null,
  nodes: {},
  expanded: {},
  loading: {},
  history: readLocal<HistoryItem[]>("nexus.history", []),
  theme: readLocal<Theme>("nexus.theme", "dark"),
  rowLimit: readLocal<number>("nexus.rowLimit", 500),
  dialog: { open: false, editing: null },
  paletteOpen: false,
  importDialog: { open: false },
  dumpDialog: { open: false, mode: "dump" },

  async loadConnections() {
    try {
      set({ connections: await api.listConnections() });
    } catch (e) {
      toast.error("Could not load connections", { description: errorText(e) });
    }
  },

  async saveConnection(c) {
    const saved = await api.saveConnection(c);
    set((s) => {
      const exists = s.connections.some((x) => x.id === saved.id);
      const live = { ...s.live };
      delete live[saved.id];
      return {
        connections: exists ? s.connections.map((x) => (x.id === saved.id ? saved : x)) : [...s.connections, saved],
        live,
      };
    });
    return saved;
  },

  async deleteConnection(id) {
    await api.deleteConnection(id);
    set((s) => ({
      connections: s.connections.filter((c) => c.id !== id),
      tabs: s.tabs.filter((t) => t.connId !== id),
      activeTabId: s.tabs.find((t) => t.id === s.activeTabId)?.connId === id ? s.tabs.find((t) => t.connId !== id)?.id ?? null : s.activeTabId,
    }));
  },

  async connect(id) {
    if (get().live[id]?.status === "connected") return true;
    set((s) => ({ live: { ...s.live, [id]: { status: "connecting" } } }));
    try {
      const info = await api.connect(id);
      set((s) => ({ live: { ...s.live, [id]: { status: "connected", version: info.version } } }));
      return true;
    } catch (e) {
      const error = errorText(e);
      set((s) => ({ live: { ...s.live, [id]: { status: "error", error } } }));
      const name = get().connections.find((c) => c.id === id)?.name;
      toast.error(`Could not connect to ${name ?? "server"}`, { description: error });
      return false;
    }
  },

  async disconnect(id) {
    await api.disconnect(id);
    set((s) => {
      const live = { ...s.live };
      delete live[id];
      const prefix = id + "\u001f";
      const strip = <T,>(m: Record<string, T>) =>
        Object.fromEntries(Object.entries(m).filter(([k]) => k !== id && !k.startsWith(prefix)));
      return { live, nodes: strip(s.nodes), expanded: strip(s.expanded) };
    });
  },

  async loadChildren(connId, path, force) {
    const key = nodeKey(connId, path);
    const cached = get().nodes[key];
    if (cached && !force) return cached;
    if (!(await get().connect(connId))) return [];
    set((s) => ({ loading: { ...s.loading, [key]: true } }));
    try {
      const children = await api.listChildren(connId, path);
      set((s) => ({ nodes: { ...s.nodes, [key]: children } }));
      return children;
    } catch (e) {
      toast.error("Failed to load", { description: errorText(e) });
      return [];
    } finally {
      set((s) => ({ loading: { ...s.loading, [key]: false } }));
    }
  },

  async toggleNode(connId, path) {
    const key = nodeKey(connId, path);
    const open = !get().expanded[key];
    set((s) => ({ expanded: { ...s.expanded, [key]: open } }));
    if (open) await get().loadChildren(connId, path);
  },

  openQueryTab(connId, opts = {}, run = false) {
    const conn = get().connections.find((c) => c.id === connId);
    const id = uid();
    const tab: Tab = {
      id,
      kind: "query",
      title: opts.title ?? `Query ${++counter}`,
      connId,
      database: opts.database ?? (conn?.database || null),
      schema: opts.schema ?? null,
      query: opts.query ?? (conn ? ENGINE_META[conn.engine].starter : ""),
      running: false,
      activeSet: 0,
      source: opts.source,
    };
    set((s) => ({ tabs: [...s.tabs, tab], activeTabId: id }));
    if (run) void get().runTab(id);
    return id;
  },

  async openObject(connId, path) {
    try {
      if (!(await get().connect(connId))) return;
      const oq = await api.objectQuery(connId, path);
      get().openQueryTab(connId, { query: oq.query, database: oq.database, schema: oq.schema, title: path[path.length - 1], source: path }, true);
    } catch (e) {
      toast.error("Cannot open object", { description: errorText(e) });
    }
  },

  async openStructure(connId, path) {
    const id = uid();
    const tab: Tab = {
      id,
      kind: "structure",
      title: path[path.length - 1],
      connId,
      query: "",
      path,
      running: true,
      activeSet: 0,
    };
    set((s) => ({ tabs: [...s.tabs, tab], activeTabId: id }));
    try {
      const structure = await api.describe(connId, path);
      get().updateTab(id, { structure, running: false });
    } catch (e) {
      get().updateTab(id, { error: errorText(e), running: false });
    }
  },

  async openDocs(connId, database, schema) {
    const existing = get().tabs.find((t) => t.kind === "docs" && t.connId === connId && (t.database ?? null) === (database ?? null) && (t.schema ?? null) === (schema ?? null));
    if (existing) {
      set({ activeTabId: existing.id });
      return;
    }
    const conn = get().connections.find((c) => c.id === connId);
    const id = uid();
    const tab: Tab = {
      id,
      kind: "docs",
      title: `Docs · ${schema ?? database ?? conn?.name ?? "database"}`,
      connId,
      database: database ?? null,
      schema: schema ?? null,
      query: "",
      running: true,
      activeSet: 0,
    };
    set((s) => ({ tabs: [...s.tabs, tab], activeTabId: id }));
    await get().loadDocs(id);
  },

  async loadDocs(tabId) {
    const tab = get().tabs.find((t) => t.id === tabId);
    if (!tab) return;
    get().updateTab(tabId, { running: true, error: undefined });
    try {
      if (!(await get().connect(tab.connId))) {
        get().updateTab(tabId, { error: "Could not connect.", running: false });
        return;
      }
      const docs = await api.schemaDoc(tab.connId, tab.database, tab.schema);
      get().updateTab(tabId, { docs, running: false });
    } catch (e) {
      get().updateTab(tabId, { error: errorText(e), running: false });
    }
  },

  closeTab(id) {
    const tab = get().tabs.find((t) => t.id === id);
    const pending = pendingCount(tab?.edits);
    if (pending) {
      toast.warning(`${tab?.title} has ${pending} unsaved change${pending > 1 ? "s" : ""}`, {
        description: "Save or discard them before closing the tab.",
        action: {
          label: "Discard & close",
          onClick: () => {
            get().discardEdits(id);
            get().closeTab(id);
          },
        },
      });
      return;
    }
    set((s) => {
      const idx = s.tabs.findIndex((t) => t.id === id);
      const tabs = s.tabs.filter((t) => t.id !== id);
      const activeTabId = s.activeTabId === id ? (tabs[Math.min(idx, tabs.length - 1)]?.id ?? null) : s.activeTabId;
      return { tabs, activeTabId };
    });
  },

  closeOtherTabs(id) {
    set((s) => ({ tabs: s.tabs.filter((t) => t.id === id), activeTabId: id }));
  },

  setActive(id) {
    set({ activeTabId: id });
  },

  updateTab(id, patch) {
    set((s) => ({ tabs: s.tabs.map((t) => (t.id === id ? { ...t, ...patch } : t)) }));
  },

  async runTab(id, queryOverride) {
    const tab = get().tabs.find((t) => t.id === id);
    if (!tab || tab.running) return;
    const query = (queryOverride ?? tab.query).trim();
    if (!query) return;
    const pending = pendingCount(tab.edits);
    if (pending) {
      toast.warning(`You have ${pending} unsaved change${pending > 1 ? "s" : ""}`, {
        description: "Save or discard them before running again.",
        action: {
          label: "Discard & run",
          onClick: () => {
            get().discardEdits(id);
            void get().runTab(id, queryOverride);
          },
        },
      });
      return;
    }
    get().updateTab(id, { running: true, error: undefined });
    if (!(await get().connect(tab.connId))) {
      get().updateTab(id, { running: false, error: get().live[tab.connId]?.error ?? "Not connected" });
      return;
    }
    const started = performance.now();
    try {
      const result = await api.execute(tab.connId, query, {
        database: tab.database,
        schema: tab.schema,
        limit: get().rowLimit,
      });
      get().updateTab(id, { result, running: false, activeSet: Math.max(0, result.sets.findIndex((s) => s.columns.length > 0)) });
      if (tab.source && tab.editInfo === undefined) {
        api
          .editInfo(tab.connId, tab.source)
          .then((editInfo) => get().updateTab(id, { editInfo }))
          .catch(() => get().updateTab(id, { editInfo: null }));
      }
      pushHistory({ connId: tab.connId, query, ok: true, ms: result.elapsedMs, rows: result.sets.reduce((n, s) => n + s.rows.length, 0) });
    } catch (e) {
      get().updateTab(id, { running: false, error: errorText(e), result: undefined });
      pushHistory({ connId: tab.connId, query, ok: false, ms: Math.round(performance.now() - started) });
    }
  },

  setTheme(theme) {
    writeLocal("nexus.theme", theme);
    set({ theme });
  },

  setRowLimit(rowLimit) {
    writeLocal("nexus.rowLimit", rowLimit);
    set({ rowLimit });
  },

  openDialog(editing = null) {
    set({ dialog: { open: true, editing } });
  },

  closeDialog() {
    set({ dialog: { open: false, editing: null } });
  },

  setPalette(paletteOpen) {
    set({ paletteOpen });
  },

  setEdits(tabId, edits) {
    get().updateTab(tabId, { edits });
  },

  discardEdits(tabId) {
    get().updateTab(tabId, { edits: emptyEdits() });
  },

  async saveEdits(tabId, dryRun = false) {
    const tab = get().tabs.find((t) => t.id === tabId);
    if (!tab?.source || !tab.edits || !tab.editInfo) return null;
    const { changes, refs } = buildChanges(tab.edits);
    if (!changes.length) return null;
    if (dryRun) return api.applyChanges(tab.connId, tab.source, changes, true);
    get().updateTab(tabId, { saving: true });
    try {
      const report = await api.applyChanges(tab.connId, tab.source, changes, false);
      const remaining = withoutApplied(tab.edits, refs, report.applied);
      if (report.error) toast.error("Could not save all changes", { description: report.error, duration: 12000 });
      else toast.success(`Saved ${changes.length} change${changes.length > 1 ? "s" : ""}`);
      // refresh, then move what is left onto the refreshed rows
      get().updateTab(tabId, { edits: emptyEdits(), saving: false });
      if (report.applied > 0) await get().runTab(tabId);
      const fresh = get().tabs.find((t) => t.id === tabId);
      const set = fresh?.result?.sets[fresh.activeSet];
      if (pendingCount(remaining) && set && fresh?.editInfo) {
        const { edits, lost } = report.applied > 0 ? reattach(remaining, set, fresh.editInfo) : { edits: remaining, lost: 0 };
        get().updateTab(tabId, { edits });
        if (lost) toast.warning(`${lost} pending change${lost > 1 ? "s" : ""} no longer match a row and were dropped`);
      }
      return report;
    } catch (e) {
      get().updateTab(tabId, { saving: false });
      toast.error("Could not save", { description: errorText(e) });
      return null;
    }
  },

  openImport(connId, path) {
    set({ importDialog: { open: true, connId, path } });
  },

  closeImport() {
    set({ importDialog: { open: false } });
  },

  openDump(mode, connId, database) {
    set({ dumpDialog: { open: true, mode, connId, database } });
  },

  closeDump() {
    set((s) => ({ dumpDialog: { ...s.dumpDialog, open: false } }));
  },
}));

function pushHistory(item: Omit<HistoryItem, "id" | "at">) {
  const history = [{ ...item, id: uid(), at: Date.now() }, ...useStore.getState().history].slice(0, 300);
  writeLocal("nexus.history", history);
  useStore.setState({ history });
}

export function newConnection(flavorId: string): ConnectionConfig {
  const f = flavorOf("postgres", flavorId);
  return {
    id: "",
    name: "",
    engine: f.engine,
    flavor: f.id,
    host: ENGINE_META[f.engine].usesFile ? "" : "localhost",
    port: f.port ?? null,
    user: f.defaults?.user ?? "",
    password: "",
    database: f.defaults?.database ?? "",
    filePath: "",
    url: "",
    ssl: f.defaults?.ssl ?? false,
    trustCert: f.defaults?.trustCert ?? false,
    readOnly: false,
    color: "",
    group: "",
    auth: "password",
    entra: { ...EMPTY_ENTRA },
  };
}
