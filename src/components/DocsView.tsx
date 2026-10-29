import clsx from "clsx";
import { ArrowLeft, ArrowRight, BookOpen, Copy, Download, Eye, KeyRound, Link2, ListTree, RefreshCw, Search, Table2, X } from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { toast } from "sonner";
import { ENGINE_META, flavorOf } from "../lib/engines";
import { displayName, fkByColumn, incomingRefs, statsLine, tableKey, toHtml, toMarkdown, uniqueColumns } from "../lib/schemadoc";
import type { SchemaDoc, TableDoc } from "../lib/types";
import { useStore, type Tab } from "../store";
import { saveText } from "./ResultsPanel";
import { EngineBadge, showContextMenu, Spinner } from "./ui";

export function DocsView({ tab }: { tab: Tab }) {
  const conn = useStore((s) => s.connections.find((c) => c.id === tab.connId));
  const { loadDocs } = useStore.getState();
  const doc = tab.docs;
  const [q, setQ] = useState("");
  const [focus, setFocus] = useState<string | null>(null);

  const engineName = conn ? flavorOf(conn.engine, conn.flavor).name : "";
  const title = doc ? `${doc.database}${tab.schema ? ` · ${tab.schema}` : ""}` : tab.title;
  const incoming = useMemo(() => (doc ? incomingRefs(doc) : new Map()), [doc]);

  // a table matches by its name, a column name or a comment
  const shown = useMemo(() => {
    if (!doc) return [];
    const f = q.trim().toLowerCase();
    if (!f) return doc.tables;
    return doc.tables.filter(
      (t) =>
        t.name.toLowerCase().includes(f) ||
        t.comment?.toLowerCase().includes(f) ||
        t.columns.some((c) => c.name.toLowerCase().includes(f) || c.comment?.toLowerCase().includes(f)),
    );
  }, [doc, q]);

  const jump = (schema: string, name: string) => {
    const key = tableKey(schema, name);
    if (q && !shown.some((t) => tableKey(t.schema, t.name) === key)) setQ("");
    setFocus(key);
    requestAnimationFrame(() => document.getElementById(`doc-${key}`)?.scrollIntoView({ behavior: "smooth", block: "start" }));
  };

  useEffect(() => {
    if (!focus) return;
    const t = setTimeout(() => setFocus(null), 1600);
    return () => clearTimeout(t);
  }, [focus]);

  const fileName = `${doc?.database ?? "database"}${tab.schema ? `-${tab.schema}` : ""}-docs`;
  const exportMenu = (e: React.MouseEvent) => {
    if (!doc) return;
    const docTitle = `${title} – database documentation`;
    showContextMenu(e, [
      { label: "HTML page (.html)", icon: <Download className="h-3.5 w-3.5" />, onSelect: () => void saveText(toHtml(doc, docTitle, engineName), fileName, "html") },
      { label: "Markdown (.md)", icon: <Download className="h-3.5 w-3.5" />, onSelect: () => void saveText(toMarkdown(doc, docTitle, engineName), fileName, "md") },
      "separator",
      {
        label: "Copy as Markdown",
        icon: <Copy className="h-3.5 w-3.5" />,
        onSelect: () => navigator.clipboard.writeText(toMarkdown(doc, docTitle, engineName)).then(() => toast.success("Markdown copied")),
      },
    ]);
  };

  return (
    <div className="flex h-full flex-col">
      <div className="flex h-14 shrink-0 items-center gap-3 border-b border-line bg-panel px-4">
        {conn && <EngineBadge engine={conn.engine} flavor={conn.flavor} size={28} />}
        <div className="min-w-0 leading-tight">
          <div className="flex items-center gap-1.5 text-[14px] font-semibold">
            <BookOpen className="h-3.5 w-3.5 text-accent" /> {title}
          </div>
          {doc && <div className="truncate text-[11.5px] text-faint">{statsLine(doc)}</div>}
        </div>
        <div className="ml-auto flex items-center gap-1.5">
          <div className="flex items-center gap-1.5 rounded-md border border-line bg-panel-2 px-2 py-1 focus-within:border-accent/60">
            <Search className="h-3 w-3 text-faint" />
            <input value={q} onChange={(e) => setQ(e.target.value)} placeholder="Find table or column" className="w-44 bg-transparent text-[12px] outline-none placeholder:text-faint" />
            {q && <X className="h-3 w-3 cursor-pointer text-faint hover:text-fg" onClick={() => setQ("")} />}
          </div>
          <button title="Reload" onClick={() => void loadDocs(tab.id)} disabled={tab.running} className="grid h-7 w-7 place-items-center rounded-md text-muted hover:bg-hover hover:text-fg disabled:opacity-40">
            <RefreshCw className={clsx("h-3.5 w-3.5", tab.running && "animate-spin")} />
          </button>
          <button onClick={exportMenu} disabled={!doc} className="flex items-center gap-1.5 rounded-md px-2 py-1 text-[12px] text-muted hover:bg-hover hover:text-fg disabled:opacity-40">
            <Download className="h-3.5 w-3.5" /> Export
          </button>
        </div>
      </div>

      {tab.running && !doc ? (
        <div className="grid flex-1 place-items-center text-[12.5px] text-muted">
          <div className="flex items-center gap-2">
            <Spinner className="h-4 w-4 text-accent" /> Reading the catalog…
          </div>
        </div>
      ) : tab.error ? (
        <div className="m-4 rounded-xl border border-danger/25 bg-danger/[0.06] p-4 font-mono text-[12px] text-danger">{tab.error}</div>
      ) : doc ? (
        <div className="flex min-h-0 flex-1">
          <div className="w-[230px] shrink-0 overflow-auto border-r border-line bg-panel py-2">
            {shown.length === 0 && <div className="px-4 py-2 text-[12px] text-faint italic">no match</div>}
            {shown.map((t) => (
              <button
                key={tableKey(t.schema, t.name)}
                onClick={() => jump(t.schema, t.name)}
                className="flex w-full items-center gap-2 px-3 py-1 text-left text-[12.5px] hover:bg-hover"
                title={t.comment ?? displayName(doc, t)}
              >
                {t.kind === "table" ? <Table2 className="h-3.5 w-3.5 shrink-0 text-accent" /> : <Eye className="h-3.5 w-3.5 shrink-0 text-accent-2" />}
                <span className="min-w-0 flex-1 truncate">{displayName(doc, t)}</span>
                {t.foreignKeys.length + (incoming.get(tableKey(t.schema, t.name))?.length ?? 0) > 0 && <Link2 className="h-3 w-3 shrink-0 text-faint" />}
              </button>
            ))}
          </div>
          <div className="min-w-0 flex-1 overflow-auto bg-bg p-4">
            {doc.tables.length === 0 && <div className="p-6 text-center text-[13px] text-faint">This database has no tables or views.</div>}
            {shown.map((t) => (
              <TableCard key={tableKey(t.schema, t.name)} doc={doc} table={t} connId={tab.connId} incoming={incoming.get(tableKey(t.schema, t.name)) ?? []} focused={focus === tableKey(t.schema, t.name)} highlight={q.trim().toLowerCase()} onJump={jump} />
            ))}
          </div>
        </div>
      ) : null}
    </div>
  );
}

function TableCard({
  doc,
  table: t,
  connId,
  incoming,
  focused,
  highlight,
  onJump,
}: {
  doc: SchemaDoc;
  table: TableDoc;
  connId: string;
  incoming: { from: TableDoc; fk: TableDoc["foreignKeys"][number] }[];
  focused: boolean;
  highlight: string;
  onJump: (schema: string, name: string) => void;
}) {
  const engine = useStore((s) => s.connections.find((c) => c.id === connId)?.engine);
  const { openObject, openStructure } = useStore.getState();
  const fks = fkByColumn(t);
  const uniq = uniqueColumns(t);
  const key = tableKey(t.schema, t.name);
  const depth = engine ? ENGINE_META[engine].tableDepth : 1;
  const path = depth === 3 ? [doc.database, t.schema, t.name] : depth === 2 ? [doc.database, t.name] : [t.name];

  const RefLink = ({ schema, name }: { schema: string; name: string }) => (
    <button onClick={() => onJump(schema, name)} className="font-medium text-accent hover:underline">
      {displayName(doc, { schema, name })}
    </button>
  );

  return (
    <section
      id={`doc-${key}`}
      style={{ contentVisibility: "auto", containIntrinsicSize: "auto 320px" }}
      className={clsx("mb-4 scroll-mt-3 rounded-xl border bg-panel transition-colors", focused ? "border-accent shadow-[0_0_0_3px_var(--accent-soft)]" : "border-line")}
    >
      <div className="flex items-center gap-2 border-b border-line px-4 py-2.5">
        {t.kind === "table" ? <Table2 className="h-4 w-4 text-accent" /> : <Eye className="h-4 w-4 text-accent-2" />}
        <h3 className="text-[14px] font-semibold">{displayName(doc, t)}</h3>
        {t.kind !== "table" && <span className="rounded-full border border-line px-2 text-[10.5px] text-muted">{t.kind}</span>}
        {t.rows != null && <span className="text-[11.5px] text-faint">~{t.rows.toLocaleString()} rows</span>}
        <div className="ml-auto flex items-center gap-1">
          <button onClick={() => void openObject(connId, path)} className="flex items-center gap-1 rounded-md px-2 py-0.5 text-[11.5px] text-muted hover:bg-hover hover:text-fg">
            <Table2 className="h-3 w-3" /> Data
          </button>
          <button onClick={() => void openStructure(connId, path)} className="flex items-center gap-1 rounded-md px-2 py-0.5 text-[11.5px] text-muted hover:bg-hover hover:text-fg">
            <ListTree className="h-3 w-3" /> Structure
          </button>
        </div>
      </div>
      {t.comment && <p className="px-4 pt-2.5 text-[12.5px] text-muted">{t.comment}</p>}
      <div className="overflow-x-auto px-2 pt-1.5">
        <table className="w-full text-[12.5px]">
          <thead>
            <tr className="text-left text-[11px] text-faint">
              <th className="px-2 py-1.5 font-medium">Column</th>
              <th className="px-2 py-1.5 font-medium">Type</th>
              <th className="px-2 py-1.5 font-medium">Null</th>
              <th className="px-2 py-1.5 font-medium">Default</th>
              <th className="px-2 py-1.5 font-medium">References / description</th>
            </tr>
          </thead>
          <tbody>
            {t.columns.map((c) => {
              const fk = fks.get(c.name);
              const pk = t.primaryKey.includes(c.name);
              const hit = !!highlight && c.name.toLowerCase().includes(highlight);
              return (
                <tr key={c.name} className={clsx("border-t border-line align-top", hit && "bg-accent/[0.08]")}>
                  <td className="px-2 py-1.5 whitespace-nowrap">
                    <span className="inline-flex items-center gap-1.5">
                      <span className={clsx("font-medium", pk ? "text-fg" : "text-fg/90")}>{c.name}</span>
                      {pk && <Badge tone="pk">PK</Badge>}
                      {fk && <Badge tone="fk">FK</Badge>}
                      {uniq.has(c.name) && !pk && <Badge tone="uq">UQ</Badge>}
                    </span>
                  </td>
                  <td className="px-2 py-1.5 font-mono text-[11.5px] text-accent-2">{c.typeName}</td>
                  <td className="px-2 py-1.5 text-[11.5px] whitespace-nowrap">{c.nullable ? <span className="text-faint">null</span> : <span className="text-muted">not null</span>}</td>
                  <td className="max-w-[220px] truncate px-2 py-1.5 font-mono text-[11.5px] text-muted" title={c.default ?? ""}>
                    {c.default}
                  </td>
                  <td className="px-2 py-1.5 text-[12px] text-muted">
                    {fk && (
                      <span className="inline-flex items-center">
                        <ArrowRight className="mr-1 h-3 w-3 text-faint" />
                        <RefLink schema={fk.refSchema || t.schema} name={fk.refTable} />
                        <span className="font-mono text-[11.5px]">.{fk.refColumns[fk.columns.indexOf(c.name)]}</span>
                      </span>
                    )}
                    {fk && c.comment && <br />}
                    {c.comment}
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>
      {(t.foreignKeys.length > 0 || incoming.length > 0 || t.indexes.length > 0) && (
        <div className="grid gap-x-6 gap-y-3 px-4 pt-2 pb-3.5 [grid-template-columns:repeat(auto-fit,minmax(240px,1fr))]">
          {t.foreignKeys.length > 0 && (
            <Group title="References">
              {t.foreignKeys.map((fk) => (
                <li key={fk.name} title={fk.name}>
                  <span className="font-mono text-[11.5px]">({fk.columns.join(", ")})</span> <ArrowRight className="inline h-3 w-3 text-faint" />{" "}
                  <RefLink schema={fk.refSchema || t.schema} name={fk.refTable} /> <span className="font-mono text-[11.5px]">({fk.refColumns.join(", ")})</span>
                  {(fk.onDelete || fk.onUpdate) && (
                    <span className="ml-1 text-[11px] text-faint">
                      {[fk.onDelete && `on delete ${fk.onDelete.toLowerCase()}`, fk.onUpdate && `on update ${fk.onUpdate.toLowerCase()}`].filter(Boolean).join(", ")}
                    </span>
                  )}
                </li>
              ))}
            </Group>
          )}
          {incoming.length > 0 && (
            <Group title="Referenced by">
              {incoming.map(({ from, fk }) => (
                <li key={`${tableKey(from.schema, from.name)}:${fk.name}`} title={fk.name}>
                  <ArrowLeft className="inline h-3 w-3 text-faint" /> <RefLink schema={from.schema} name={from.name} />{" "}
                  <span className="font-mono text-[11.5px]">({fk.columns.join(", ")})</span>
                </li>
              ))}
            </Group>
          )}
          {t.indexes.length > 0 && (
            <Group title="Indexes">
              {t.indexes.map((i) => (
                <li key={i.name}>
                  <span className="font-mono text-[11.5px]">{i.name}</span> <span className="text-faint">({i.columns.join(", ")})</span>
                  {i.primary ? (
                    <Badge tone="pk" className="ml-1.5">
                      primary
                    </Badge>
                  ) : i.unique ? (
                    <Badge tone="uq" className="ml-1.5">
                      unique
                    </Badge>
                  ) : null}
                </li>
              ))}
            </Group>
          )}
        </div>
      )}
    </section>
  );
}

function Group({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div className="min-w-0">
      <div className="mb-1 text-[10.5px] font-semibold tracking-[0.08em] text-faint uppercase">{title}</div>
      <ul className="space-y-0.5 text-[12px] text-muted">{children}</ul>
    </div>
  );
}

function Badge({ tone, className, children }: { tone: "pk" | "fk" | "uq"; className?: string; children: React.ReactNode }) {
  return (
    <span
      className={clsx(
        "inline-flex items-center gap-0.5 rounded border px-1 text-[9.5px] leading-[14px] font-bold",
        tone === "pk" && "border-warn/50 text-warn",
        tone === "fk" && "border-accent/50 text-accent",
        tone === "uq" && "border-success/50 text-success",
        className,
      )}
    >
      {tone === "pk" && children === "PK" && <KeyRound className="h-2.5 w-2.5" />}
      {children}
    </span>
  );
}

