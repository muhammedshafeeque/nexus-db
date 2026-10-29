import clsx from "clsx";
import {
  ChevronRight,
  Copy,
  FilePlus2,
  Pencil,
  Plug,
  PlugZap,
  Plus,
  RefreshCw,
  Search,
  Table2,
  Trash2,
  Unplug,
  Info,
  Lock,
  Upload,
  X,
} from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { api, errorText } from "../lib/api";
import { toast } from "sonner";
import { flavorOf } from "../lib/engines";
import type { ConnectionConfig, TreeNode } from "../lib/types";
import { nodeKey, useStore } from "../store";
import { EngineBadge, IconButton, NodeIcon, showContextMenu, Spinner, type MenuEntry } from "./ui";

const OPENABLE = new Set(["table", "view", "collection", "edge", "key"]);

export function Sidebar() {
  const connections = useStore((s) => s.connections);
  const openDialog = useStore((s) => s.openDialog);
  const [filter, setFilter] = useState("");

  const groups = useMemo(() => {
    const f = filter.toLowerCase();
    const visible = connections.filter(
      (c) => !f || c.name.toLowerCase().includes(f) || c.host.toLowerCase().includes(f) || c.engine.includes(f),
    );
    const map = new Map<string, ConnectionConfig[]>();
    for (const c of visible) {
      const g = c.group.trim();
      map.set(g, [...(map.get(g) ?? []), c]);
    }
    return [...map.entries()].sort(([a], [b]) => (a === "" ? 1 : b === "" ? -1 : a.localeCompare(b)));
  }, [connections, filter]);

  return (
    <div className="flex h-full flex-col bg-panel">
      <div className="flex items-center gap-2 px-3 pt-3 pb-2">
        <span className="flex-1 text-[11px] font-semibold tracking-[0.08em] text-faint uppercase">Connections</span>
        <IconButton title="New connection" onClick={() => openDialog()}>
          <Plus className="h-4 w-4" />
        </IconButton>
      </div>
      <div className="px-3 pb-2">
        <div className="flex items-center gap-2 rounded-lg border border-line bg-panel-2 px-2.5 py-1.5 transition-colors focus-within:border-accent/60">
          <Search className="h-3.5 w-3.5 text-faint" />
          <input
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            placeholder="Filter connections"
            className="w-full bg-transparent text-[12.5px] outline-none placeholder:text-faint"
          />
        </div>
      </div>
      <div className="min-h-0 flex-1 overflow-auto px-1.5 pb-4">
        {connections.length === 0 && (
          <div className="mx-2 mt-6 rounded-xl border border-dashed border-line-strong p-5 text-center">
            <PlugZap className="mx-auto mb-2 h-6 w-6 text-accent" />
            <p className="mb-3 text-[12.5px] text-muted">No connections yet</p>
            <button onClick={() => openDialog()} className="btn-primary rounded-lg px-3 py-1.5 text-[12px] font-medium text-white">
              Add a database
            </button>
          </div>
        )}
        {groups.map(([group, conns]) => (
          <div key={group || "_"} className="mb-1">
            {group && (
              <div className="px-2.5 pt-2.5 pb-1 text-[10.5px] font-semibold tracking-[0.08em] text-faint uppercase">{group}</div>
            )}
            {conns.map((c) => (
              <ConnectionItem key={c.id} conn={c} />
            ))}
          </div>
        ))}
      </div>
    </div>
  );
}

function ConnectionItem({ conn }: { conn: ConnectionConfig }) {
  const live = useStore((s) => s.live[conn.id]);
  const expanded = useStore((s) => !!s.expanded[nodeKey(conn.id, [])]);
  const loading = useStore((s) => !!s.loading[nodeKey(conn.id, [])]);
  const { toggleNode, openQueryTab, openDialog, disconnect, deleteConnection, loadChildren, openImport } = useStore.getState();
  const f = flavorOf(conn.engine, conn.flavor);

  const status = live?.status;
  const menu: MenuEntry[] = [
    { label: "New query", icon: <FilePlus2 className="h-3.5 w-3.5" />, shortcut: "Ctrl T", onSelect: () => openQueryTab(conn.id) },
    { label: "Refresh", icon: <RefreshCw className="h-3.5 w-3.5" />, onSelect: () => loadChildren(conn.id, [], true) },
    ...(conn.readOnly
      ? []
      : ([{ label: "Import data…", icon: <Upload className="h-3.5 w-3.5" />, onSelect: () => openImport(conn.id, []) }] as MenuEntry[])),
    "separator",
    { label: "Edit connection", icon: <Pencil className="h-3.5 w-3.5" />, onSelect: () => openDialog(conn) },
    { label: "Duplicate", icon: <Copy className="h-3.5 w-3.5" />, onSelect: () => openDialog({ ...conn, id: "", name: `${conn.name} copy` }) },
    status === "connected"
      ? { label: "Disconnect", icon: <Unplug className="h-3.5 w-3.5" />, onSelect: () => disconnect(conn.id) }
      : { label: "Connect", icon: <Plug className="h-3.5 w-3.5" />, onSelect: () => toggleNode(conn.id, []) },
    "separator",
    {
      label: "Delete",
      icon: <Trash2 className="h-3.5 w-3.5" />,
      danger: true,
      onSelect: async () => {
        await deleteConnection(conn.id);
        toast.success(`Deleted ${conn.name}`);
      },
    },
  ];

  return (
    <div>
      <div
        onClick={() => toggleNode(conn.id, [])}
        onDoubleClick={() => openQueryTab(conn.id)}
        onContextMenu={(e) => showContextMenu(e, menu)}
        className="group relative flex h-11 items-center gap-2.5 rounded-lg px-2 transition-colors hover:bg-hover"
      >
        {conn.color && <span className="absolute top-2.5 bottom-2.5 left-0 w-[3px] rounded-full" style={{ background: conn.color }} />}
        <ChevronRight className={clsx("h-3.5 w-3.5 shrink-0 text-faint transition-transform", expanded && "rotate-90")} />
        <div className="relative">
          <EngineBadge engine={conn.engine} flavor={conn.flavor} size={26} />
          <span
            className={clsx(
              "absolute -right-0.5 -bottom-0.5 h-2.5 w-2.5 rounded-full border-2 border-panel",
              status === "connected" && "bg-success",
              status === "error" && "bg-danger",
              status === "connecting" && "animate-pulse bg-warn",
              !status && "bg-line-strong",
            )}
          />
        </div>
        <div className="min-w-0 flex-1 leading-tight">
          <div className="flex items-center gap-1 truncate text-[12.5px] font-medium text-fg">
            <span className="truncate">{conn.name || f.name}</span>
            {conn.readOnly && <Lock className="h-3 w-3 shrink-0 text-warn" />}
          </div>
          <div className="truncate text-[11px] text-faint">
            {status === "error" ? (
              <span className="text-danger">connection failed</span>
            ) : conn.engine === "sqlite" ? (
              conn.filePath.split(/[\\/]/).pop()
            ) : conn.url ? (
              "connection string"
            ) : (
              `${conn.host || "localhost"}${conn.port ? `:${conn.port}` : ""}`
            )}
          </div>
        </div>
        {loading && <Spinner className="h-3.5 w-3.5 text-accent" />}
        <div className="hidden items-center group-hover:flex">
          <IconButton
            title="New query"
            onClick={(e) => {
              e.stopPropagation();
              openQueryTab(conn.id);
            }}
          >
            <FilePlus2 className="h-3.5 w-3.5" />
          </IconButton>
        </div>
      </div>
      {expanded && <Children connId={conn.id} path={[]} depth={1} />}
    </div>
  );
}

const SEARCH_LABEL: Record<string, string> = {
  database: "databases",
  schema: "schemas",
  table: "tables",
  view: "tables",
  collection: "collections",
  edge: "collections",
  key: "keys",
};

function Children({ connId, path, depth }: { connId: string; path: string[]; depth: number }) {
  const nodes = useStore((s) => s.nodes[nodeKey(connId, path)]);
  const loading = useStore((s) => !!s.loading[nodeKey(connId, path)]);
  const engine = useStore((s) => s.connections.find((c) => c.id === connId)?.engine);
  const [q, setQ] = useState("");
  const [remote, setRemote] = useState<TreeNode[] | null>(null);
  const [searching, setSearching] = useState(false);
  const kind = nodes?.find((n) => SEARCH_LABEL[n.kind])?.kind;
  // Redis lists only the first keys – search the keyspace on the server
  const serverSide = engine === "redis" && kind === "key";
  const query = q.trim();

  useEffect(() => {
    if (!serverSide || !query) {
      setRemote(null);
      return;
    }
    setSearching(true);
    const t = setTimeout(() => {
      api
        .searchChildren(connId, path, query)
        .then(setRemote)
        .catch((e) => toast.error("Search failed", { description: errorText(e) }))
        .finally(() => setSearching(false));
    }, 300);
    return () => clearTimeout(t);
  }, [serverSide, query, connId, path.join("\u001f")]);

  if (!nodes) {
    return loading ? (
      <div className="flex items-center gap-2 py-1 text-[12px] text-faint" style={{ paddingLeft: depth * 14 + 14 }}>
        <Spinner className="h-3 w-3" /> Loading…
      </div>
    ) : null;
  }
  if (nodes.length === 0) {
    return (
      <div className="py-1 text-[11.5px] text-faint italic" style={{ paddingLeft: depth * 14 + 22 }}>
        empty
      </div>
    );
  }
  const shown = !query ? nodes : serverSide ? (remote ?? []) : nodes.filter((n) => n.name.toLowerCase().includes(query.toLowerCase()));
  const searchable = !!kind && (nodes.length > 1 || serverSide || !!query);
  return (
    <>
      {searchable && (
        <div className="py-0.5 pr-2" style={{ paddingLeft: depth * 14 + 4 }}>
          <div className="flex h-[24px] items-center gap-1.5 rounded-md border border-line bg-panel-2 px-2 focus-within:border-accent/60">
            {searching ? <Spinner className="h-3 w-3 shrink-0 text-accent" /> : <Search className="h-3 w-3 shrink-0 text-faint" />}
            <input
              value={q}
              onChange={(e) => setQ(e.target.value)}
              onKeyDown={(e) => e.key === "Escape" && setQ("")}
              placeholder={`Search ${SEARCH_LABEL[kind!]}`}
              className="w-full min-w-0 bg-transparent text-[11.5px] outline-none placeholder:text-faint"
            />
            {query ? (
              <>
                <span className="shrink-0 font-mono text-[10px] text-faint">
                  {serverSide ? (remote ? remote.length : "") : `${shown.length}/${nodes.length}`}
                </span>
                <X className="h-3 w-3 shrink-0 cursor-pointer text-faint hover:text-fg" onClick={() => setQ("")} />
              </>
            ) : (
              <span className="shrink-0 font-mono text-[10px] text-faint">{nodes.length}</span>
            )}
          </div>
        </div>
      )}
      {shown.map((n) => (
        <TreeRow key={n.path.join("/")} connId={connId} node={n} depth={depth} highlight={query} />
      ))}
      {query && !searching && shown.length === 0 && (
        <div className="py-1 text-[11.5px] text-faint italic" style={{ paddingLeft: depth * 14 + 22 }}>
          no match for “{query}”
        </div>
      )}
    </>
  );
}

function Highlight({ text, query }: { text: string; query?: string }) {
  const i = query ? text.toLowerCase().indexOf(query.toLowerCase()) : -1;
  if (!query || i < 0) return <>{text}</>;
  return (
    <>
      {text.slice(0, i)}
      <mark className="rounded-sm bg-accent/25 text-fg">{text.slice(i, i + query.length)}</mark>
      {text.slice(i + query.length)}
    </>
  );
}

function TreeRow({ connId, node, depth, highlight }: { connId: string; node: TreeNode; depth: number; highlight?: string }) {
  const key = nodeKey(connId, node.path);
  const expanded = useStore((s) => !!s.expanded[key]);
  const loading = useStore((s) => !!s.loading[key]);
  const readOnly = useStore((s) => !!s.connections.find((c) => c.id === connId)?.readOnly);
  const { toggleNode, openObject, openStructure, openQueryTab, loadChildren, openImport } = useStore.getState();
  const openable = OPENABLE.has(node.kind);
  const importable = !readOnly && ["table", "collection", "edge", "database", "schema"].includes(node.kind);

  const menu: MenuEntry[] = [
    ...(openable
      ? ([
          { label: "Open data", icon: <Table2 className="h-3.5 w-3.5" />, onSelect: () => openObject(connId, node.path) },
          { label: "View structure", icon: <Info className="h-3.5 w-3.5" />, onSelect: () => openStructure(connId, node.path) },
        ] as MenuEntry[])
      : []),
    ...(node.kind === "database" || node.kind === "schema"
      ? ([
          {
            label: "New query here",
            icon: <FilePlus2 className="h-3.5 w-3.5" />,
            onSelect: () =>
              openQueryTab(connId, {
                database: node.path[0],
                schema: node.kind === "schema" ? node.name : null,
              }),
          },
        ] as MenuEntry[])
      : []),
    ...(importable
      ? ([{ label: "Import data…", icon: <Upload className="h-3.5 w-3.5" />, onSelect: () => openImport(connId, node.path) }] as MenuEntry[])
      : []),
    { label: "Copy name", icon: <Copy className="h-3.5 w-3.5" />, onSelect: () => navigator.clipboard.writeText(node.name) },
    ...(node.expandable
      ? ([{ label: "Refresh", icon: <RefreshCw className="h-3.5 w-3.5" />, onSelect: () => loadChildren(connId, node.path, true) }] as MenuEntry[])
      : []),
  ];

  return (
    <>
      <div
        onClick={() => node.expandable && toggleNode(connId, node.path)}
        onDoubleClick={() => openable && openObject(connId, node.path)}
        onContextMenu={(e) => showContextMenu(e, menu)}
        className="group flex h-[26px] items-center gap-1.5 rounded-md pr-2 text-[12.5px] transition-colors hover:bg-hover"
        style={{ paddingLeft: depth * 14 + 4 }}
        title={node.detail ? `${node.name} · ${node.detail}` : node.name}
      >
        {node.expandable ? (
          <ChevronRight className={clsx("h-3 w-3 shrink-0 text-faint transition-transform", expanded && "rotate-90")} />
        ) : (
          <span className="w-3 shrink-0" />
        )}
        <NodeIcon kind={node.kind} />
        <span className={clsx("truncate", node.kind === "column" || node.kind === "index" ? "text-muted" : "text-fg")}>
          <Highlight text={node.name} query={highlight} />
        </span>
        {node.detail && <span className="ml-auto shrink-0 truncate pl-2 font-mono text-[10.5px] text-faint">{node.detail}</span>}
        {loading && <Spinner className="ml-1 h-3 w-3 shrink-0 text-accent" />}
      </div>
      {expanded && node.expandable && <Children connId={connId} path={node.path} depth={depth + 1} />}
    </>
  );
}
