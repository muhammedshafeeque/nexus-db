import clsx from "clsx";
import { ChevronDown, Database, FolderTree, History, Lock, Play } from "lucide-react";
import { useEffect, useMemo } from "react";
import { Group, Panel, Separator } from "react-resizable-panels";
import { ENGINE_META, flavorOf } from "../lib/engines";
import { nodeKey, useStore, type Tab } from "../store";
import { Editor } from "./Editor";
import { ResultsPanel } from "./ResultsPanel";
import { EngineBadge, Kbd, showContextMenu, Spinner } from "./ui";

const LIMITS = [100, 500, 1000, 5000, 10000, 50000];

function Picker({
  icon,
  value,
  placeholder,
  options,
  onChange,
}: {
  icon: React.ReactNode;
  value: string | null | undefined;
  placeholder: string;
  options: string[];
  onChange: (v: string | null) => void;
}) {
  return (
    <button
      onClick={(e) =>
        showContextMenu(e, [
          { label: `(default)`, onSelect: () => onChange(null) },
          "separator",
          ...options.map((o) => ({ label: o, onSelect: () => onChange(o) })),
        ])
      }
      className="flex h-7 max-w-[200px] items-center gap-1.5 rounded-md border border-line bg-panel-2 px-2 text-[12px] text-fg transition-colors hover:border-line-strong"
    >
      {icon}
      <span className={clsx("truncate", !value && "text-faint")}>{value || placeholder}</span>
      <ChevronDown className="h-3 w-3 shrink-0 text-faint" />
    </button>
  );
}

export function QueryTab({ tab }: { tab: Tab }) {
  const conn = useStore((s) => s.connections.find((c) => c.id === tab.connId));
  const connections = useStore((s) => s.connections);
  const nodes = useStore((s) => s.nodes);
  const rowLimit = useStore((s) => s.rowLimit);
  const history = useStore((s) => s.history);
  const { updateTab, runTab, loadChildren, setRowLimit, setEdits, saveEdits, discardEdits } = useStore.getState();
  const meta = conn ? ENGINE_META[conn.engine] : null;

  useEffect(() => {
    if (meta?.hasDatabases) void loadChildren(tab.connId, []);
  }, [tab.connId, meta?.hasDatabases]);

  useEffect(() => {
    if (meta?.hasSchemas && tab.database) void loadChildren(tab.connId, [tab.database]);
  }, [tab.connId, tab.database, meta?.hasSchemas]);

  const databases = (nodes[nodeKey(tab.connId, [])] ?? []).filter((n) => n.kind === "database").map((n) => n.name);
  const schemas = tab.database
    ? (nodes[nodeKey(tab.connId, [tab.database])] ?? []).filter((n) => n.kind === "schema").map((n) => n.name)
    : [];

  // autocomplete schema from whatever part of the tree is loaded
  const completion = useMemo(() => {
    const out: Record<string, string[]> = {};
    const prefix = tab.connId + "\u001f";
    for (const [k, list] of Object.entries(nodes)) {
      if (!k.startsWith(prefix)) continue;
      for (const n of list) {
        if (n.kind === "table" || n.kind === "view") {
          const cols = nodes[nodeKey(tab.connId, n.path)]?.map((c) => c.name) ?? [];
          out[n.name] = cols;
        }
      }
    }
    return out;
  }, [nodes, tab.connId]);

  if (!conn || !meta) return <div className="p-6 text-muted">Connection was removed.</div>;
  const f = flavorOf(conn.engine, conn.flavor);
  const recent = history.filter((h) => h.connId === conn.id).slice(0, 15);

  return (
    <div className="flex h-full flex-col">
      <div className="flex h-12 shrink-0 items-center gap-2 border-b border-line bg-panel px-3">
        <button
          onClick={() => runTab(tab.id)}
          disabled={tab.running}
          className="btn-primary flex h-8 items-center gap-2 rounded-lg pr-2.5 pl-3 text-[12.5px] font-semibold text-white disabled:opacity-70"
        >
          {tab.running ? <Spinner className="h-3.5 w-3.5" /> : <Play className="h-3.5 w-3.5 fill-current" />}
          Run
          <span className="rounded bg-white/15 px-1 font-mono text-[10px] font-normal">Ctrl ↵</span>
        </button>
        <div className="mx-1 h-5 w-px bg-line" />
        <button
          onClick={(e) =>
            showContextMenu(
              e,
              connections.map((c) => ({
                label: c.name || flavorOf(c.engine, c.flavor).name,
                icon: <EngineBadge engine={c.engine} flavor={c.flavor} size={16} />,
                onSelect: () => updateTab(tab.id, { connId: c.id, database: c.database || null, schema: null }),
              })),
            )
          }
          className="flex h-7 items-center gap-2 rounded-md px-1.5 text-[12.5px] font-medium hover:bg-hover"
        >
          <EngineBadge engine={conn.engine} flavor={conn.flavor} size={20} />
          <span className="max-w-[180px] truncate">{conn.name || f.name}</span>
          {conn.readOnly && <Lock className="h-3 w-3 text-warn" />}
          <ChevronDown className="h-3 w-3 text-faint" />
        </button>
        {meta.hasDatabases && (
          <Picker
            icon={<Database className="h-3.5 w-3.5 text-[#a78bfa]" />}
            value={tab.database}
            placeholder="database"
            options={databases}
            onChange={(database) => updateTab(tab.id, { database, schema: null })}
          />
        )}
        {meta.hasSchemas && (
          <Picker
            icon={<FolderTree className="h-3.5 w-3.5 text-[#60a5fa]" />}
            value={tab.schema}
            placeholder="schema"
            options={schemas}
            onChange={(schema) => updateTab(tab.id, { schema })}
          />
        )}
        <div className="flex-1" />
        <button
          onClick={(e) =>
            showContextMenu(
              e,
              recent.length
                ? recent.map((h) => ({
                    label: h.query.replace(/\s+/g, " ").slice(0, 60),
                    icon: <span className={clsx("h-1.5 w-1.5 rounded-full", h.ok ? "bg-success" : "bg-danger")} />,
                    shortcut: new Date(h.at).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" }),
                    onSelect: () => updateTab(tab.id, { query: h.query }),
                  }))
                : [{ label: "No history yet", onSelect: () => {} }],
            )
          }
          className="flex h-7 items-center gap-1.5 rounded-md px-2 text-[12px] text-muted hover:bg-hover hover:text-fg"
        >
          <History className="h-3.5 w-3.5" /> History
        </button>
        <button
          onClick={(e) =>
            showContextMenu(
              e,
              LIMITS.map((l) => ({
                label: `${l.toLocaleString()} rows`,
                shortcut: l === rowLimit ? "✓" : undefined,
                onSelect: () => setRowLimit(l),
              })),
            )
          }
          className="flex h-7 items-center gap-1.5 rounded-md px-2 text-[12px] text-muted hover:bg-hover hover:text-fg"
          title="Maximum rows fetched per result"
        >
          Limit <span className="font-mono text-fg">{rowLimit.toLocaleString()}</span>
          <ChevronDown className="h-3 w-3" />
        </button>
      </div>

      <Group orientation="vertical" className="min-h-0 flex-1">
        <Panel defaultSize="42" minSize="12" className="bg-bg">
          <div className="relative h-full">
            <Editor
              value={tab.query}
              onChange={(query) => updateTab(tab.id, { query })}
              engine={conn.engine}
              schema={completion}
              onRun={(sel) => runTab(tab.id, sel ?? undefined)}
            />
            <div className="pointer-events-none absolute right-3 bottom-2 flex items-center gap-1 text-[11px] text-faint">
              <Kbd>{meta.language.toUpperCase()}</Kbd>
            </div>
          </div>
        </Panel>
        <Separator className="resize-handle h-px" />
        <Panel minSize="15" className="bg-panel">
          <ResultsPanel
            result={tab.result}
            error={tab.error}
            running={tab.running}
            activeSet={tab.activeSet}
            onActiveSet={(activeSet) => updateTab(tab.id, { activeSet })}
            title={tab.title}
            editing={
              tab.source
                ? {
                    target: tab.source[tab.source.length - 1],
                    info: tab.editInfo,
                    edits: tab.edits,
                    readOnly: conn.readOnly,
                    saving: !!tab.saving,
                    onChange: (edits) => setEdits(tab.id, edits),
                    onSave: (dryRun) => saveEdits(tab.id, dryRun),
                    onDiscard: () => discardEdits(tab.id),
                  }
                : undefined
            }
          />
        </Panel>
      </Group>
    </div>
  );
}
