import type { Cell, EditInfo, PendingEdits, ResultSet, RowChange } from "./types";

export const emptyEdits = (): PendingEdits => ({ updates: {}, inserts: [], deletes: {} });

const same = (a: Cell | undefined, b: Cell | undefined) => JSON.stringify(a ?? null) === JSON.stringify(b ?? null);

export function pendingCount(e?: PendingEdits): number {
  if (!e) return 0;
  const updates = Object.keys(e.updates).filter((r) => !(r in e.deletes)).length;
  return updates + e.inserts.length + Object.keys(e.deletes).length;
}

export interface EditState {
  editable: boolean;
  reason?: string;
  /** per result column: may the user change it */
  colEditable: boolean[];
}

/** Whether this result can be edited: it needs the table's key columns and, for SQL, real table columns. */
export function editState(set: ResultSet, info?: EditInfo | null, readOnly?: boolean): EditState {
  const none = (reason?: string): EditState => ({ editable: false, reason, colEditable: set.columns.map(() => false) });
  if (!info) return none();
  if (readOnly) return none("This connection is read-only.");
  if (!info.editable) return none(info.reason ?? "Editing is not available here.");
  const names = set.columns.map((c) => c.name);
  const missing = info.keyColumns.filter((k) => !names.includes(k) && !(info.documents && k === "_index"));
  if (missing.length) return none(`Include the key column${missing.length > 1 ? "s" : ""} ${missing.join(", ")} in the query to edit rows.`);
  const known = new Set(info.columns.map((c) => c.name));
  const colEditable = set.columns.map((c) =>
    info.documents ? !["_id", "_key", "_index", "_rev"].includes(c.name) : known.has(c.name),
  );
  return { editable: true, reason: info.reason ?? undefined, colEditable };
}

export function rowKey(set: ResultSet, info: EditInfo, row: number): Record<string, Cell> {
  const key: Record<string, Cell> = {};
  for (const k of info.keyColumns) {
    const i = set.columns.findIndex((c) => c.name === k);
    if (i >= 0) key[k] = set.rows[row]?.[i] ?? null;
  }
  return key;
}

/** Value currently shown for a cell (pending edit or original). */
export function cellValue(set: ResultSet, edits: PendingEdits | undefined, row: number, col: number): Cell | undefined {
  const name = set.columns[col]?.name;
  if (row >= set.rows.length) return edits?.inserts[row - set.rows.length]?.[name];
  const upd = edits?.updates[row]?.values;
  return upd && name in upd ? upd[name] : set.rows[row][col];
}

export function withCell(edits: PendingEdits, set: ResultSet, info: EditInfo, row: number, col: number, value: Cell): PendingEdits {
  const name = set.columns[col].name;
  if (row >= set.rows.length) {
    const inserts = edits.inserts.slice();
    inserts[row - set.rows.length] = { ...inserts[row - set.rows.length], [name]: value };
    return { ...edits, inserts };
  }
  const entry = edits.updates[row] ?? { key: rowKey(set, info, row), values: {} };
  const values = { ...entry.values };
  if (same(set.rows[row][col], value)) delete values[name];
  else values[name] = value;
  const updates = { ...edits.updates };
  if (Object.keys(values).length) updates[row] = { ...entry, values };
  else delete updates[row];
  return { ...edits, updates };
}

export function withDeleted(edits: PendingEdits, set: ResultSet, info: EditInfo, rows: number[], deleted: boolean): PendingEdits {
  const deletes = { ...edits.deletes };
  const dropInserts = new Set<number>();
  for (const r of rows) {
    if (r >= set.rows.length) dropInserts.add(r - set.rows.length);
    else if (deleted) deletes[r] = rowKey(set, info, r);
    else delete deletes[r];
  }
  return { ...edits, deletes, inserts: edits.inserts.filter((_, i) => !dropInserts.has(i)) };
}

export function withReverted(edits: PendingEdits, set: ResultSet, row: number): PendingEdits {
  if (row >= set.rows.length) return { ...edits, inserts: edits.inserts.filter((_, i) => i !== row - set.rows.length) };
  const updates = { ...edits.updates };
  const deletes = { ...edits.deletes };
  delete updates[row];
  delete deletes[row];
  return { ...edits, updates, deletes };
}

type Ref = { kind: "update" | "delete"; row: number } | { kind: "insert"; index: number };

/** Deletes first (frees unique values), then updates, then inserts. */
export function buildChanges(edits: PendingEdits): { changes: RowChange[]; refs: Ref[] } {
  const changes: RowChange[] = [];
  const refs: Ref[] = [];
  for (const [row, key] of Object.entries(edits.deletes)) {
    changes.push({ kind: "delete", key });
    refs.push({ kind: "delete", row: Number(row) });
  }
  for (const [row, u] of Object.entries(edits.updates)) {
    if (row in edits.deletes) continue;
    changes.push({ kind: "update", key: u.key, values: u.values });
    refs.push({ kind: "update", row: Number(row) });
  }
  edits.inserts.forEach((values, index) => {
    const clean = Object.fromEntries(Object.entries(values).filter(([, v]) => v !== undefined));
    changes.push({ kind: "insert", values: clean });
    refs.push({ kind: "insert", index });
  });
  return { changes, refs };
}

/** Removes the first `applied` changes (they were saved). */
export function withoutApplied(edits: PendingEdits, refs: Ref[], applied: number): PendingEdits {
  const next: PendingEdits = { updates: { ...edits.updates }, deletes: { ...edits.deletes }, inserts: edits.inserts.slice() };
  const doneInserts = new Set<number>();
  for (const ref of refs.slice(0, applied)) {
    if (ref.kind === "insert") doneInserts.add(ref.index);
    else if (ref.kind === "update") delete next.updates[ref.row];
    else delete next.deletes[ref.row];
  }
  next.inserts = next.inserts.filter((_, i) => !doneInserts.has(i));
  return next;
}

/** After a refresh, moves pending row edits to the rows with the same key. */
export function reattach(edits: PendingEdits, set: ResultSet, info: EditInfo): { edits: PendingEdits; lost: number } {
  const index = new Map<string, number>();
  set.rows.forEach((_, r) => index.set(JSON.stringify(rowKey(set, info, r)), r));
  const next: PendingEdits = { updates: {}, deletes: {}, inserts: edits.inserts };
  let lost = 0;
  for (const u of Object.values(edits.updates)) {
    const r = index.get(JSON.stringify(u.key));
    if (r === undefined) lost++;
    else next.updates[r] = u;
  }
  for (const key of Object.values(edits.deletes)) {
    const r = index.get(JSON.stringify(key));
    if (r === undefined) lost++;
    else next.deletes[r] = key;
  }
  return { edits: next, lost };
}

/** Text typed into a cell → value. SQL gets text (the server converts it);
 *  documents get JSON when it parses, unless the field held text before. */
export function parseInput(text: string, original: Cell | undefined, documents: boolean): Cell {
  if (!documents) return text;
  const t = text.trim();
  if (typeof original === "string" && !/^[[{"]/.test(t)) return text;
  if (t === "") return text;
  try {
    return JSON.parse(t) as Cell;
  } catch {
    return text;
  }
}

/** Text shown in the cell editor. */
export function editText(v: Cell | undefined): string {
  if (v === null || v === undefined) return "";
  if (typeof v === "object") return JSON.stringify(v);
  return String(v);
}
