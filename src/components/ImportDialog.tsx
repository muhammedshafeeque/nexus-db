import clsx from "clsx";
import { AlertTriangle, ArrowRight, CheckCircle2, FileSpreadsheet, FolderOpen, KeyRound, Table2, Upload, X, XCircle } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { toast } from "sonner";
import { api, errorText, isTauri } from "../lib/api";
import { ENGINE_META, flavorOf } from "../lib/engines";
import type { Cell, EditInfo, FileOptions, ImportMode, ImportPreview, ImportReport } from "../lib/types";
import { nodeKey, useStore } from "../store";
import { cellText } from "./DataGrid";
import { EngineBadge, Spinner, Toggle } from "./ui";

const inputCls =
  "h-8 w-full rounded-lg border border-line bg-panel-2 px-2.5 text-[12.5px] text-fg outline-none transition-colors placeholder:text-faint focus:border-accent/70";

const ENCODINGS = ["", "utf-8", "windows-1252", "iso-8859-1", "utf-16le", "utf-16be", "shift_jis", "gbk", "big5", "windows-1251"];
const DELIMITERS: [string, string][] = [["", "Detect"], [",", "Comma ,"], [";", "Semicolon ;"], ["\\t", "Tab"], ["|", "Pipe |"]];
const TABLE_KINDS = new Set(["table", "collection", "edge"]);

const norm = (s: string) => s.toLowerCase().replace(/[^a-z0-9]/g, "");

function Section({ n, title, children }: { n: number; title: string; children: React.ReactNode }) {
  return (
    <div className="space-y-2.5">
      <div className="flex items-center gap-2 text-[11px] font-semibold tracking-[0.08em] text-faint uppercase">
        <span className="grid h-4 w-4 place-items-center rounded-full bg-accent/20 text-[10px] text-accent">{n}</span>
        {title}
      </div>
      {children}
    </div>
  );
}

function Label({ text, children }: { text: string; children: React.ReactNode }) {
  return (
    <label className="block">
      <span className="mb-1 block text-[11.5px] text-muted">{text}</span>
      {children}
    </label>
  );
}

export function ImportDialog() {
  const { open, connId, path } = useStore((s) => s.importDialog);
  if (!open) return null;
  return <ImportWizard initialConn={connId} initialPath={path ?? []} />;
}

interface Mapping {
  include: boolean;
  target: string;
}

function ImportWizard({ initialConn, initialPath }: { initialConn?: string; initialPath: string[] }) {
  const connections = useStore((s) => s.connections);
  const nodes = useStore((s) => s.nodes);
  const { closeImport, loadChildren, openObject } = useStore.getState();
  const writable = connections.filter((c) => !c.readOnly);
  const [connId, setConnId] = useState(initialConn && writable.some((c) => c.id === initialConn) ? initialConn : (writable[0]?.id ?? ""));
  const conn = connections.find((c) => c.id === connId);
  const meta = conn ? ENGINE_META[conn.engine] : null;
  const depth = meta?.tableDepth ?? 1;
  const [target, setTarget] = useState<string[]>(() => initialPath.slice(0, depth));

  const [file, setFile] = useState<FileOptions>({ path: "", hasHeader: true, sheet: null, delimiter: null, encoding: null });
  const [preview, setPreview] = useState<ImportPreview | null>(null);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [loadingPreview, setLoadingPreview] = useState(false);

  const [info, setInfo] = useState<EditInfo | null>(null);
  const [infoError, setInfoError] = useState<string | null>(null);
  const [mappings, setMappings] = useState<Mapping[]>([]);
  const [keys, setKeys] = useState<string[]>([]);
  const [mode, setMode] = useState<ImportMode>("insert");
  const [atomic, setAtomic] = useState(true);
  const [batchSize, setBatchSize] = useState(500);
  const [keyTemplate, setKeyTemplate] = useState("");
  const [trim, setTrim] = useState(true);
  const [emptyAsNull, setEmptyAsNull] = useState(true);
  const [detectTypes, setDetectTypes] = useState(false);

  const [phase, setPhase] = useState<"setup" | "running" | "done">("setup");
  const [progress, setProgress] = useState({ done: 0, total: 0 });
  const [report, setReport] = useState<ImportReport | null>(null);
  const [runError, setRunError] = useState<string | null>(null);

  const complete = target.length === depth && target.every(Boolean);
  const isRedis = conn?.engine === "redis";
  const documents = !!info?.documents;

  // close on Escape unless an import is running
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && phase !== "running" && closeImport();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [phase]);

  // ---- target tree
  useEffect(() => {
    if (!conn) return;
    for (let i = 0; i < depth; i++) {
      const prefix = target.slice(0, i);
      if (prefix.length === i && prefix.every(Boolean)) void loadChildren(conn.id, prefix);
    }
  }, [connId, target.join("\u001f")]);

  const levelOptions = (i: number): string[] => {
    if (!conn) return [];
    const list = nodes[nodeKey(conn.id, target.slice(0, i))] ?? [];
    const last = i === depth - 1;
    return list
      .filter((n) => (last && !isRedis ? TABLE_KINDS.has(n.kind) : n.kind === "database" || n.kind === "schema"))
      .map((n) => n.name);
  };

  // ---- target columns
  useEffect(() => {
    setInfo(null);
    setInfoError(null);
    if (!conn || !complete) return;
    let stale = false;
    api
      .editInfo(conn.id, target)
      .then((i) => !stale && setInfo(i))
      .catch((e) => !stale && setInfoError(errorText(e)));
    return () => {
      stale = true;
    };
  }, [connId, complete, target.join("\u001f")]);

  useEffect(() => {
    if (info) {
      setDetectTypes(info.documents);
      setAtomic(info.transactional);
      if (!info.canUpsert) setMode("insert");
    }
  }, [info]);

  // ---- file preview
  const loadPreview = async (opts: FileOptions) => {
    if (!opts.path.trim()) return;
    setLoadingPreview(true);
    setPreviewError(null);
    try {
      const p = await api.importPreview(opts);
      setPreview(p);
    } catch (e) {
      setPreview(null);
      setPreviewError(errorText(e));
    } finally {
      setLoadingPreview(false);
    }
  };

  const updateFile = (patch: Partial<FileOptions>) => {
    const next = { ...file, ...patch };
    setFile(next);
    void loadPreview(next);
  };

  const browse = async () => {
    if (!isTauri) {
      updateFile({ path: file.path || "/demo/customers.csv" });
      return;
    }
    const { open } = await import("@tauri-apps/plugin-dialog");
    const p = await open({
      multiple: false,
      filters: [
        { name: "Spreadsheets & CSV", extensions: ["csv", "tsv", "txt", "xlsx", "xlsm", "xlsb", "xls", "ods"] },
        { name: "All files", extensions: ["*"] },
      ],
    });
    if (typeof p === "string") updateFile({ path: p, sheet: null });
  };

  // ---- automatic mapping whenever the file columns or the target change
  const targetCols = info?.columns ?? [];
  useEffect(() => {
    if (!preview) return;
    const next = preview.columns.map((src): Mapping => {
      const match = targetCols.find((c) => norm(c.name) === norm(src));
      if (match) return { include: true, target: match.name };
      if (documents || isRedis) return { include: true, target: src.trim() };
      return { include: false, target: "" };
    });
    setMappings(next);
    const mapped = new Set(next.filter((m) => m.include).map((m) => m.target));
    setKeys((info?.keyColumns ?? []).filter((k) => mapped.has(k)));
    if (isRedis) {
      const first = next.find((m) => m.include)?.target;
      setKeyTemplate(first ? `item:{${first}}` : "");
    }
  }, [preview?.columns.join("\u001f"), info]);

  const included = mappings.map((m, i) => ({ ...m, source: i })).filter((m) => m.include);
  const mappedTargets = included.map((m) => m.target.trim()).filter(Boolean);
  const duplicates = mappedTargets.filter((t, i) => mappedTargets.findIndex((x) => x.toLowerCase() === t.toLowerCase()) !== i);
  const missingRequired = documents || isRedis ? [] : targetCols.filter((c) => !c.nullable && !c.hasDefault && !mappedTargets.includes(c.name));

  const samples = useMemo(() => {
    if (!preview) return [];
    return preview.columns.map((_, i) =>
      preview.rows
        .map((r) => r[i])
        .filter((v) => v !== null && v !== "")
        .slice(0, 3),
    );
  }, [preview]);

  const problems: string[] = [];
  if (!preview) problems.push("Choose a file");
  if (!complete) problems.push(`Choose the target ${meta?.levels[depth - 1]?.toLowerCase() ?? "table"}`);
  if (infoError) problems.push(infoError);
  if (preview && !included.length) problems.push("Select at least one column");
  if (included.some((m) => !m.target.trim())) problems.push("Every selected column needs a target");
  if (duplicates.length) problems.push(`"${duplicates[0]}" is mapped twice`);
  if (mode === "upsert" && !keys.length && conn?.engine !== "mysql" && !isRedis) problems.push("Pick the key column(s) used to match existing rows");
  if (isRedis && !/\{[^}]+\}/.test(keyTemplate)) problems.push("Enter a key template such as user:{id}");

  const run = async () => {
    if (!conn || !preview || problems.length) return;
    setPhase("running");
    setRunError(null);
    setProgress({ done: 0, total: preview.totalRows });
    let unlisten: (() => void) | undefined;
    if (isTauri) {
      const { listen } = await import("@tauri-apps/api/event");
      unlisten = await listen<{ done: number; total: number }>("import-progress", (e) => setProgress(e.payload));
    }
    try {
      const r = await api.importRun(
        conn.id,
        file,
        target,
        included.map((m) => ({ source: m.source, target: m.target.trim() })),
        { mode, keyColumns: mode === "upsert" ? keys : [], atomic: atomic && !!info?.transactional, batchSize, keyTemplate },
        { trim, emptyAsNull, detectTypes },
      );
      setReport(r);
      setPhase("done");
      void loadChildren(conn.id, target.slice(0, -1), true);
    } catch (e) {
      setRunError(errorText(e));
      setPhase("setup");
    } finally {
      unlisten?.();
    }
  };

  const targetLabel = target.filter(Boolean).join(" › ");

  return (
    <div className="animate-overlay fixed inset-0 z-50 grid place-items-center bg-black/55 p-5 backdrop-blur-[3px]" onMouseDown={() => phase !== "running" && closeImport()}>
      <div
        onMouseDown={(e) => e.stopPropagation()}
        className="animate-pop flex h-[min(840px,94vh)] w-[min(1180px,100%)] flex-col overflow-hidden rounded-2xl border border-line bg-panel shadow-pop"
      >
        <div className="flex items-center gap-3 border-b border-line px-5 py-3.5">
          <div className="grid h-9 w-9 place-items-center rounded-xl bg-accent/15 text-accent">
            <Upload className="h-4.5 w-4.5" />
          </div>
          <div className="min-w-0">
            <h2 className="text-[15px] font-semibold">Import data</h2>
            <p className="truncate text-[12px] text-muted">CSV, TSV, Excel (.xlsx, .xls, .xlsb) and OpenDocument (.ods) into a table or collection</p>
          </div>
          <button onClick={closeImport} disabled={phase === "running"} className="ml-auto grid h-8 w-8 place-items-center rounded-lg text-muted hover:bg-hover hover:text-fg disabled:opacity-40">
            <X className="h-4 w-4" />
          </button>
        </div>

        {phase === "done" && report ? (
          <ImportResult report={report} targetLabel={targetLabel} />
        ) : (
          <div className="grid min-h-0 flex-1 grid-cols-[330px_1fr]">
            {/* ---------------- left: file, target, options */}
            <div className="space-y-5 overflow-auto border-r border-line bg-panel-2/40 p-4">
              <Section n={1} title="File">
                <div className="flex gap-2">
                  <input
                    className={clsx(inputCls, "font-mono text-[11.5px]")}
                    value={file.path}
                    placeholder="/path/to/data.xlsx"
                    onChange={(e) => setFile({ ...file, path: e.target.value })}
                    onBlur={() => file.path && loadPreview(file)}
                    onKeyDown={(e) => e.key === "Enter" && loadPreview(file)}
                  />
                  <button onClick={browse} className="flex h-8 shrink-0 items-center gap-1.5 rounded-lg border border-line bg-panel-2 px-2.5 text-[12px] hover:bg-hover">
                    <FolderOpen className="h-3.5 w-3.5" /> Browse
                  </button>
                </div>
                {loadingPreview && (
                  <div className="flex items-center gap-2 text-[12px] text-muted">
                    <Spinner className="h-3.5 w-3.5" /> Reading file…
                  </div>
                )}
                {previewError && <div className="rounded-lg border border-danger/30 bg-danger/[0.06] p-2 text-[12px] text-danger">{previewError}</div>}
                {preview && (
                  <>
                    <div className="flex flex-wrap gap-x-3 gap-y-1 text-[11.5px] text-muted">
                      <span>
                        <b className="text-fg">{preview.totalRows.toLocaleString()}</b> rows
                      </span>
                      <span>
                        <b className="text-fg">{preview.columns.length}</b> columns
                      </span>
                      {preview.encoding && <span>{preview.encoding}</span>}
                      {preview.errorCells > 0 && <span className="text-warn">{preview.errorCells} error cells read as empty</span>}
                    </div>
                    {preview.sheets.length > 1 && (
                      <Label text="Sheet">
                        <select className={clsx(inputCls, "cursor-pointer")} value={preview.sheet ?? ""} onChange={(e) => updateFile({ sheet: e.target.value })}>
                          {preview.sheets.map((s) => (
                            <option key={s}>{s}</option>
                          ))}
                        </select>
                      </Label>
                    )}
                    {!preview.sheets.length && (
                      <div className="grid grid-cols-2 gap-2">
                        <Label text="Delimiter">
                          <select className={clsx(inputCls, "cursor-pointer")} value={file.delimiter ?? ""} onChange={(e) => updateFile({ delimiter: e.target.value || null })}>
                            {DELIMITERS.map(([v, l]) => (
                              <option key={v} value={v}>
                                {v === "" && preview.delimiter ? `Detect (${preview.delimiter === "\\t" ? "tab" : preview.delimiter})` : l}
                              </option>
                            ))}
                          </select>
                        </Label>
                        <Label text="Encoding">
                          <select className={clsx(inputCls, "cursor-pointer")} value={file.encoding ?? ""} onChange={(e) => updateFile({ encoding: e.target.value || null })}>
                            {ENCODINGS.map((v) => (
                              <option key={v} value={v}>
                                {v || "Detect"}
                              </option>
                            ))}
                          </select>
                        </Label>
                      </div>
                    )}
                    <Toggle checked={file.hasHeader !== false} onChange={(hasHeader) => updateFile({ hasHeader })} label="First row has column names" />
                  </>
                )}
              </Section>

              <Section n={2} title="Target">
                <Label text="Connection">
                  <select
                    className={clsx(inputCls, "cursor-pointer")}
                    value={connId}
                    onChange={(e) => {
                      setConnId(e.target.value);
                      setTarget([]);
                    }}
                  >
                    {writable.map((c) => (
                      <option key={c.id} value={c.id}>
                        {c.name || flavorOf(c.engine, c.flavor).name}
                      </option>
                    ))}
                  </select>
                </Label>
                {meta?.levels.map((level, i) => {
                  if (i > 0 && !target[i - 1]) return null;
                  const options = levelOptions(i);
                  const last = i === depth - 1;
                  const free = last && (documents || conn?.engine === "elasticsearch" || conn?.engine === "mongodb" || conn?.engine === "arangodb");
                  const set = (v: string) => setTarget([...target.slice(0, i), v]);
                  return (
                    <Label key={level} text={free ? `${level} (existing or new)` : level}>
                      {free ? (
                        <>
                          <input className={inputCls} list={`import-level-${i}`} value={target[i] ?? ""} placeholder={`${level.toLowerCase()} name`} onChange={(e) => set(e.target.value.trim())} />
                          <datalist id={`import-level-${i}`}>
                            {options.map((o) => (
                              <option key={o} value={o} />
                            ))}
                          </datalist>
                        </>
                      ) : (
                        <select className={clsx(inputCls, "cursor-pointer")} value={target[i] ?? ""} onChange={(e) => set(e.target.value)}>
                          <option value="">Choose…</option>
                          {options.map((o) => (
                            <option key={o}>{o}</option>
                          ))}
                        </select>
                      )}
                    </Label>
                  );
                })}
                {isRedis && (
                  <Label text="Key template (one hash per row)">
                    <input className={clsx(inputCls, "font-mono text-[12px]")} value={keyTemplate} placeholder="user:{id}" onChange={(e) => setKeyTemplate(e.target.value)} />
                  </Label>
                )}
                {infoError && complete && <div className="text-[11.5px] text-danger">{infoError}</div>}
              </Section>

              <Section n={3} title="Options">
                {!isRedis && (
                  <div className="flex rounded-lg border border-line bg-panel-2 p-0.5">
                    {(["insert", "upsert"] as const).map((m) => (
                      <button
                        key={m}
                        disabled={m === "upsert" && info !== null && !info.canUpsert}
                        onClick={() => setMode(m)}
                        className={clsx(
                          "flex-1 rounded-md px-2 py-1 text-[12px] disabled:opacity-40",
                          mode === m ? "bg-elev text-fg shadow-sm" : "text-muted",
                        )}
                      >
                        {m === "insert" ? "Insert new rows" : "Insert or update"}
                      </button>
                    ))}
                  </div>
                )}
                {mode === "upsert" && info?.upsertNote && <p className="text-[11.5px] text-faint">{info.upsertNote}</p>}
                {mode === "insert" && info && !info.canUpsert && info.upsertNote && <p className="text-[11.5px] text-faint">{info.upsertNote}</p>}
                {info?.transactional && (
                  <Toggle checked={atomic} onChange={setAtomic} label="All or nothing" hint={atomic ? "One transaction – any bad row rolls everything back" : "Good rows are saved, bad rows are reported"} />
                )}
                {info && !info.transactional && <p className="text-[11.5px] text-faint">This database has no transactions: good rows are saved and bad rows are reported.</p>}
                <Toggle checked={trim} onChange={setTrim} label="Trim spaces" />
                <Toggle checked={emptyAsNull} onChange={setEmptyAsNull} label={documents ? "Leave out empty cells" : "Empty cells as NULL"} />
                <Toggle checked={detectTypes} onChange={setDetectTypes} label="Detect numbers & booleans" hint={documents ? "Recommended for documents" : "SQL columns convert text themselves"} />
                <Label text="Batch size">
                  <input className={clsx(inputCls, "font-mono")} value={batchSize} onChange={(e) => setBatchSize(Math.max(1, Math.min(10000, Number(e.target.value.replace(/\D/g, "")) || 1)))} />
                </Label>
              </Section>
            </div>

            {/* ---------------- right: mapping */}
            <div className="flex min-h-0 flex-col">
              <div className="flex items-center gap-2 border-b border-line px-4 py-2.5">
                <span className="text-[11px] font-semibold tracking-[0.08em] text-faint uppercase">Column mapping</span>
                {preview && (
                  <span className="text-[11.5px] text-muted">
                    {included.length} of {preview.columns.length} columns
                  </span>
                )}
                {conn && complete && (
                  <span className="ml-auto flex min-w-0 items-center gap-1.5 text-[12px] text-muted">
                    <EngineBadge engine={conn.engine} flavor={conn.flavor} size={16} />
                    <span className="truncate">{targetLabel}</span>
                  </span>
                )}
              </div>
              <div className="min-h-0 flex-1 overflow-auto">
                {!preview ? (
                  <div className="grid h-full place-items-center p-8 text-center">
                    <div>
                      <FileSpreadsheet className="mx-auto mb-3 h-10 w-10 text-faint" />
                      <p className="text-[13px] text-muted">Choose a CSV or Excel file to map its columns.</p>
                    </div>
                  </div>
                ) : (
                  <table className="w-full border-separate border-spacing-0 text-[12.5px]">
                    <thead className="sticky top-0 z-10 bg-panel">
                      <tr className="text-left text-[11px] text-faint">
                        <th className="w-9 border-b border-line py-2 pl-4">
                          <input
                            type="checkbox"
                            className="accent-[var(--accent)]"
                            checked={included.length === mappings.length && mappings.length > 0}
                            onChange={(e) => setMappings(mappings.map((m, i) => ({ include: e.target.checked, target: m.target || (documents || isRedis ? preview.columns[i] : m.target) })))}
                          />
                        </th>
                        <th className="border-b border-line py-2 font-medium">File column</th>
                        <th className="border-b border-line py-2 font-medium">Sample values</th>
                        <th className="w-6 border-b border-line" />
                        <th className="border-b border-line py-2 pr-3 font-medium">{documents || isRedis ? "Field / key name" : "Target column"}</th>
                        {mode === "upsert" && !isRedis && <th className="w-14 border-b border-line py-2 pr-4 text-center font-medium">Match</th>}
                      </tr>
                    </thead>
                    <tbody>
                      {preview.columns.map((src, i) => {
                        const m = mappings[i] ?? { include: false, target: "" };
                        const set = (patch: Partial<Mapping>) => setMappings(mappings.map((x, j) => (j === i ? { ...x, ...patch } : x)));
                        const col = targetCols.find((c) => c.name === m.target);
                        const dup = m.include && duplicates.some((d) => d.toLowerCase() === m.target.trim().toLowerCase());
                        return (
                          <tr key={i} className={clsx("group", !m.include && "opacity-55")}>
                            <td className="border-b border-line py-1.5 pl-4">
                              <input type="checkbox" className="accent-[var(--accent)]" checked={m.include} onChange={(e) => set({ include: e.target.checked, target: m.target || (documents || isRedis ? src : "") })} />
                            </td>
                            <td className="max-w-[180px] border-b border-line py-1.5 pr-2">
                              <div className="truncate font-medium" title={src}>
                                {src}
                              </div>
                            </td>
                            <td className="max-w-[240px] border-b border-line py-1.5 pr-2">
                              <div className="flex gap-1 overflow-hidden">
                                {samples[i]?.length ? (
                                  samples[i].map((v, k) => (
                                    <span key={k} className="max-w-[110px] shrink-0 truncate rounded bg-panel-2 px-1.5 py-px font-mono text-[11px] text-muted" title={cellText(v as Cell)}>
                                      {cellText(v as Cell)}
                                    </span>
                                  ))
                                ) : (
                                  <span className="text-[11px] text-faint italic">empty</span>
                                )}
                              </div>
                            </td>
                            <td className="border-b border-line text-faint">
                              <ArrowRight className="h-3.5 w-3.5" />
                            </td>
                            <td className="border-b border-line py-1.5 pr-3">
                              {documents || isRedis ? (
                                <>
                                  <input
                                    className={clsx(inputCls, "h-7 font-mono text-[12px]", dup && "border-danger/70")}
                                    list="import-fields"
                                    value={m.target}
                                    placeholder="field name (a.b nests)"
                                    onChange={(e) => set({ target: e.target.value, include: true })}
                                  />
                                </>
                              ) : (
                                <div className="flex items-center gap-2">
                                  <select
                                    className={clsx(inputCls, "h-7 cursor-pointer", dup && "border-danger/70")}
                                    value={m.target}
                                    onChange={(e) => set({ target: e.target.value, include: !!e.target.value })}
                                  >
                                    <option value="">— skip —</option>
                                    {targetCols.map((c) => (
                                      <option key={c.name} value={c.name}>
                                        {c.name}
                                        {c.typeName ? `  ·  ${c.typeName}` : ""}
                                      </option>
                                    ))}
                                  </select>
                                  {col?.key && <KeyRound className="h-3.5 w-3.5 shrink-0 text-warn" />}
                                </div>
                              )}
                            </td>
                            {mode === "upsert" && !isRedis && (
                              <td className="border-b border-line py-1.5 pr-4 text-center">
                                <input
                                  type="checkbox"
                                  className="accent-[var(--accent)]"
                                  disabled={!m.include || !m.target}
                                  checked={m.include && keys.includes(m.target)}
                                  onChange={(e) => setKeys(e.target.checked ? [...keys, m.target] : keys.filter((k) => k !== m.target))}
                                  title="Match existing rows on this column"
                                />
                              </td>
                            )}
                          </tr>
                        );
                      })}
                    </tbody>
                  </table>
                )}
                <datalist id="import-fields">
                  {targetCols.map((c) => (
                    <option key={c.name} value={c.name} />
                  ))}
                </datalist>
              </div>
              {missingRequired.length > 0 && preview && (
                <div className="flex items-start gap-2 border-t border-warn/30 bg-warn/[0.06] px-4 py-2 text-[12px] text-warn">
                  <AlertTriangle className="mt-0.5 h-3.5 w-3.5 shrink-0" />
                  <span>
                    Required column{missingRequired.length > 1 ? "s" : ""} without a default {missingRequired.length > 1 ? "are" : "is"} not mapped:{" "}
                    <b>{missingRequired.map((c) => c.name).join(", ")}</b> – rows will fail unless you map {missingRequired.length > 1 ? "them" : "it"}.
                  </span>
                </div>
              )}
            </div>
          </div>
        )}

        {/* ---------------- footer */}
        <div className="flex items-center gap-3 border-t border-line bg-panel-2/40 px-5 py-3">
          {phase === "running" ? (
            <>
              <div className="flex-1">
                <div className="mb-1 flex justify-between text-[12px] text-muted">
                  <span>
                    Importing into <b className="text-fg">{targetLabel}</b>…
                  </span>
                  <span className="font-mono">
                    {progress.done.toLocaleString()} / {progress.total.toLocaleString()}
                  </span>
                </div>
                <div className="h-1.5 overflow-hidden rounded-full bg-line">
                  <div className="h-full rounded-full bg-accent transition-all" style={{ width: `${progress.total ? (progress.done / progress.total) * 100 : 0}%` }} />
                </div>
              </div>
              <button onClick={() => void api.importCancel()} className="h-9 rounded-lg border border-line-strong px-3.5 text-[12.5px] hover:bg-hover">
                Cancel import
              </button>
            </>
          ) : phase === "done" ? (
            <>
              <div className="flex-1" />
              {conn && report && report.written > 0 && (
                <button
                  onClick={() => {
                    closeImport();
                    void openObject(conn.id, target);
                  }}
                  className="flex h-9 items-center gap-2 rounded-lg border border-line-strong px-3.5 text-[12.5px] hover:bg-hover"
                >
                  <Table2 className="h-3.5 w-3.5" /> Open {meta?.levels[depth - 1]?.toLowerCase()}
                </button>
              )}
              <button onClick={closeImport} className="btn-primary h-9 rounded-lg px-4 text-[12.5px] font-semibold text-white">
                Done
              </button>
            </>
          ) : (
            <>
              <div className="min-w-0 flex-1 text-[12px]">
                {runError ? (
                  <span className="text-danger">{runError}</span>
                ) : problems.length ? (
                  <span className="text-faint">{problems[0]}</span>
                ) : (
                  <span className="text-muted">
                    {mode === "upsert" ? "Insert or update" : "Insert"} <b className="text-fg">{preview?.totalRows.toLocaleString()}</b> rows · {included.length} columns →{" "}
                    <b className="text-fg">{targetLabel}</b>
                  </span>
                )}
              </div>
              <button onClick={closeImport} className="h-9 rounded-lg px-3.5 text-[12.5px] text-muted hover:bg-hover hover:text-fg">
                Cancel
              </button>
              <button
                onClick={() => {
                  if (problems.length) toast.error(problems[0]);
                  else void run();
                }}
                className={clsx("btn-primary flex h-9 items-center gap-2 rounded-lg px-4 text-[12.5px] font-semibold text-white", problems.length > 0 && "opacity-50")}
              >
                <Upload className="h-3.5 w-3.5" /> Import
              </button>
            </>
          )}
        </div>
      </div>
    </div>
  );
}

function ImportResult({ report, targetLabel }: { report: ImportReport; targetLabel: string }) {
  const ref = useRef<HTMLDivElement>(null);
  const ok = report.failed === 0 && !report.cancelled;
  const copyErrors = () =>
    navigator.clipboard
      .writeText(report.errors.map((e) => `row ${e.row}\t${e.message}`).join("\n"))
      .then(() => toast.success("Errors copied"));
  return (
    <div ref={ref} className="min-h-0 flex-1 overflow-auto p-6">
      <div className="mx-auto max-w-[760px]">
        <div className="mb-5 flex items-center gap-3">
          {ok ? <CheckCircle2 className="h-9 w-9 text-success" /> : report.rolledBack ? <XCircle className="h-9 w-9 text-danger" /> : <AlertTriangle className="h-9 w-9 text-warn" />}
          <div>
            <div className="text-[16px] font-semibold">
              {report.cancelled
                ? "Import cancelled"
                : ok
                  ? "Import finished"
                  : report.rolledBack
                    ? "Import rolled back – nothing was written"
                    : "Import finished with errors"}
            </div>
            <div className="text-[12.5px] text-muted">{targetLabel}</div>
          </div>
        </div>
        <div className="mb-5 grid grid-cols-3 gap-3">
          <Stat label="Rows written" value={report.written} tone="success" />
          <Stat label="Rows failed" value={report.failed} tone={report.failed ? "danger" : undefined} />
          <Stat label="Mode" text={report.rolledBack ? "rolled back" : report.cancelled ? "stopped" : "committed"} />
        </div>
        {report.rolledBack && report.failed > 0 && (
          <p className="mb-3 text-[12.5px] text-muted">
            “All or nothing” was on, so the whole import was undone. Fix the rows below, or turn the option off to save the good rows and skip the bad ones.
          </p>
        )}
        {report.errors.length > 0 && (
          <div className="rounded-xl border border-line">
            <div className="flex items-center border-b border-line px-3 py-2 text-[12px] text-muted">
              {report.failed > report.errors.length ? `First ${report.errors.length} of ${report.failed.toLocaleString()} errors` : `${report.errors.length} error${report.errors.length > 1 ? "s" : ""}`}
              <button onClick={copyErrors} className="ml-auto rounded px-2 py-0.5 text-[11.5px] hover:bg-hover hover:text-fg">
                Copy
              </button>
            </div>
            <div className="max-h-[360px] overflow-auto">
              {report.errors.map((e, i) => (
                <div key={i} className="flex gap-3 border-b border-line px-3 py-1.5 text-[12px] last:border-0">
                  <span className="w-16 shrink-0 font-mono text-faint">{e.row ? `row ${e.row}` : "—"}</span>
                  <span className="selectable min-w-0 font-mono break-words text-danger/90">{e.message}</span>
                </div>
              ))}
            </div>
          </div>
        )}
      </div>
    </div>
  );
}

function Stat({ label, value, text, tone }: { label: string; value?: number; text?: string; tone?: "success" | "danger" }) {
  return (
    <div className="rounded-xl border border-line bg-panel-2/60 p-3">
      <div className="text-[11px] text-faint">{label}</div>
      <div className={clsx("mt-0.5 text-[20px] font-semibold", tone === "success" && "text-success", tone === "danger" && "text-danger")}>
        {text ?? value?.toLocaleString()}
      </div>
    </div>
  );
}
