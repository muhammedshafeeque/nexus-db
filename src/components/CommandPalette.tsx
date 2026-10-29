import clsx from "clsx";
import { ArchiveRestore, CornerDownLeft, DatabaseBackup, FilePlus2, Moon, Plus, Search, Sun, Upload } from "lucide-react";
import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { flavorOf } from "../lib/engines";
import { useStore } from "../store";
import { EngineBadge, NodeIcon } from "./ui";

interface Item {
  id: string;
  group: string;
  label: string;
  hint?: string;
  icon: ReactNode;
  run: () => void;
}

/** subsequence match score; -1 when not matched */
function score(text: string, q: string): number {
  if (!q) return 0;
  const t = text.toLowerCase();
  const idx = t.indexOf(q);
  if (idx >= 0) return 100 - idx;
  let ti = 0;
  for (const ch of q) {
    ti = t.indexOf(ch, ti);
    if (ti < 0) return -1;
    ti++;
  }
  return 10;
}

export function CommandPalette() {
  const open = useStore((s) => s.paletteOpen);
  const setPalette = useStore((s) => s.setPalette);
  const connections = useStore((s) => s.connections);
  const nodes = useStore((s) => s.nodes);
  const theme = useStore((s) => s.theme);
  const [q, setQ] = useState("");
  const [index, setIndex] = useState(0);
  const listRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (open) {
      setQ("");
      setIndex(0);
    }
  }, [open]);

  const items = useMemo<Item[]>(() => {
    const s = useStore.getState();
    const out: Item[] = [
      { id: "new-conn", group: "Actions", label: "New connection", icon: <Plus className="h-4 w-4" />, run: () => s.openDialog() },
      { id: "import", group: "Actions", label: "Import data from CSV / Excel…", icon: <Upload className="h-4 w-4" />, run: () => s.openImport() },
      { id: "dump", group: "Actions", label: "Create database dump…", icon: <DatabaseBackup className="h-4 w-4" />, run: () => s.openDump("dump") },
      { id: "restore", group: "Actions", label: "Restore dump to a database…", icon: <ArchiveRestore className="h-4 w-4" />, run: () => s.openDump("restore") },
      {
        id: "theme",
        group: "Actions",
        label: `Switch to ${theme === "dark" ? "light" : "dark"} theme`,
        icon: theme === "dark" ? <Sun className="h-4 w-4" /> : <Moon className="h-4 w-4" />,
        run: () => s.setTheme(theme === "dark" ? "light" : "dark"),
      },
    ];
    for (const c of connections) {
      out.push({
        id: `q-${c.id}`,
        group: "Connections",
        label: `New query · ${c.name || flavorOf(c.engine, c.flavor).name}`,
        hint: c.engine,
        icon: <EngineBadge engine={c.engine} flavor={c.flavor} size={18} />,
        run: () => s.openQueryTab(c.id),
      });
    }
    for (const [key, list] of Object.entries(nodes)) {
      const connId = key.split("\u001f")[0];
      const conn = connections.find((c) => c.id === connId);
      if (!conn) continue;
      for (const n of list) {
        if (!["table", "view", "collection", "edge", "key"].includes(n.kind)) continue;
        out.push({
          id: `o-${connId}-${n.path.join("/")}`,
          group: "Objects",
          label: n.name,
          hint: `${conn.name} · ${n.path.slice(0, -1).join(" › ")}`,
          icon: <NodeIcon kind={n.kind} className="h-4 w-4" />,
          run: () => s.openObject(connId, n.path),
        });
      }
    }
    return out;
  }, [connections, nodes, theme]);

  const filtered = useMemo(() => {
    const query = q.trim().toLowerCase();
    return items
      .map((it) => ({ it, s: score(`${it.label} ${it.hint ?? ""}`, query) }))
      .filter((x) => x.s >= 0)
      .sort((a, b) => b.s - a.s)
      .slice(0, 60)
      .map((x) => x.it);
  }, [items, q]);

  useEffect(() => {
    listRef.current?.querySelector(`[data-i="${index}"]`)?.scrollIntoView({ block: "nearest" });
  }, [index]);

  if (!open) return null;

  const run = (it?: Item) => {
    if (!it) return;
    setPalette(false);
    it.run();
  };

  let lastGroup = "";
  return (
    <div className="animate-overlay fixed inset-0 z-50 flex justify-center bg-black/45 pt-[14vh] backdrop-blur-[2px]" onMouseDown={() => setPalette(false)}>
      <div
        onMouseDown={(e) => e.stopPropagation()}
        className="animate-pop flex h-fit max-h-[60vh] w-[640px] flex-col overflow-hidden rounded-2xl border border-line bg-elev shadow-pop"
      >
        <div className="flex items-center gap-3 border-b border-line px-4">
          <Search className="h-4 w-4 text-faint" />
          <input
            autoFocus
            value={q}
            onChange={(e) => {
              setQ(e.target.value);
              setIndex(0);
            }}
            onKeyDown={(e) => {
              if (e.key === "ArrowDown") {
                e.preventDefault();
                setIndex((i) => Math.min(filtered.length - 1, i + 1));
              } else if (e.key === "ArrowUp") {
                e.preventDefault();
                setIndex((i) => Math.max(0, i - 1));
              } else if (e.key === "Enter") run(filtered[index]);
              else if (e.key === "Escape") setPalette(false);
            }}
            placeholder="Search tables, collections, connections and actions…"
            className="h-13 flex-1 bg-transparent text-[14px] outline-none placeholder:text-faint"
          />
        </div>
        <div ref={listRef} className="overflow-auto p-1.5">
          {filtered.length === 0 && <div className="p-6 text-center text-[12.5px] text-faint">Nothing found. Expand a connection to index its objects.</div>}
          {filtered.map((it, i) => {
            const header = it.group !== lastGroup ? it.group : null;
            lastGroup = it.group;
            return (
              <div key={it.id}>
                {header && <div className="px-3 pt-2.5 pb-1 text-[10.5px] font-semibold tracking-[0.08em] text-faint uppercase">{header}</div>}
                <button
                  data-i={i}
                  onMouseMove={() => setIndex(i)}
                  onClick={() => run(it)}
                  className={clsx("flex w-full items-center gap-3 rounded-lg px-3 py-2 text-left", i === index ? "bg-accent-soft" : "")}
                >
                  <span className="grid w-5 place-items-center text-muted">{it.icon}</span>
                  <span className="truncate text-[13px] text-fg">{it.label}</span>
                  {it.hint && <span className="truncate text-[11.5px] text-faint">{it.hint}</span>}
                  {i === index && <CornerDownLeft className="ml-auto h-3.5 w-3.5 shrink-0 text-faint" />}
                </button>
              </div>
            );
          })}
        </div>
        <div className="flex items-center gap-3 border-t border-line px-4 py-2 text-[11px] text-faint">
          <span>↑↓ navigate</span>
          <span>↵ open</span>
          <span>esc close</span>
          <FilePlus2 className="ml-auto h-3 w-3" /> Ctrl+T new query
        </div>
      </div>
    </div>
  );
}
