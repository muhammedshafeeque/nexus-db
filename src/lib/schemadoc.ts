import type { ForeignKeyDoc, SchemaDoc, TableDoc } from "./types";

export const tableKey = (schema: string, name: string) => `${schema}.${name}`;

export interface Incoming {
  from: TableDoc;
  fk: ForeignKeyDoc;
}

/** Foreign keys that point at each table ("referenced by"). */
export function incomingRefs(doc: SchemaDoc): Map<string, Incoming[]> {
  const map = new Map<string, Incoming[]>();
  for (const t of doc.tables) {
    for (const fk of t.foreignKeys) {
      const k = tableKey(fk.refSchema || t.schema, fk.refTable);
      map.set(k, [...(map.get(k) ?? []), { from: t, fk }]);
    }
  }
  return map;
}

/** Column → the foreign key it belongs to. */
export function fkByColumn(t: TableDoc): Map<string, ForeignKeyDoc> {
  const m = new Map<string, ForeignKeyDoc>();
  for (const fk of t.foreignKeys) for (const c of fk.columns) if (!m.has(c)) m.set(c, fk);
  return m;
}

/** Columns that are unique on their own (unique key or unique index). */
export function uniqueColumns(t: TableDoc): Set<string> {
  const s = new Set<string>();
  for (const k of t.uniqueKeys) if (k.columns.length === 1) s.add(k.columns[0]);
  for (const i of t.indexes) if (i.unique && !i.primary && i.columns.length === 1) s.add(i.columns[0]);
  return s;
}

const plural = (n: number, word: string) => `${n.toLocaleString()} ${word}${n === 1 ? "" : "s"}`;

/** "5 tables · 1 view · 26 columns · 5 relations" */
export function statsLine(doc: SchemaDoc) {
  const s = docStats(doc);
  return [plural(s.tables, "table"), plural(s.views, "view"), plural(s.columns, "column"), plural(s.relations, "relation")].join(" · ");
}

export function docStats(doc: SchemaDoc) {
  return {
    tables: doc.tables.filter((t) => t.kind === "table").length,
    views: doc.tables.filter((t) => t.kind !== "table").length,
    columns: doc.tables.reduce((n, t) => n + t.columns.length, 0),
    relations: doc.tables.reduce((n, t) => n + t.foreignKeys.length, 0),
  };
}

/** Schema prefix only when the doc spans several schemas. */
export function displayName(doc: SchemaDoc, t: { schema: string; name: string }) {
  const many = new Set(doc.tables.map((x) => x.schema)).size > 1;
  return many ? `${t.schema}.${t.name}` : t.name;
}

export function refName(doc: SchemaDoc, t: TableDoc, fk: ForeignKeyDoc) {
  return displayName(doc, { schema: fk.refSchema || t.schema, name: fk.refTable });
}

const sameSet = (a: string[], b: string[]) => a.length === b.length && a.every((c) => b.includes(c));

/** One-to-one when the referencing columns are themselves unique. */
function isOneToOne(t: TableDoc, fk: ForeignKeyDoc) {
  return (
    sameSet(fk.columns, t.primaryKey) ||
    t.uniqueKeys.some((k) => sameSet(k.columns, fk.columns)) ||
    t.indexes.some((i) => i.unique && sameSet(i.columns, fk.columns))
  );
}

// ---------------------------------------------------------------- ER diagram (Mermaid)

const ident = (s: string) => s.replace(/[^A-Za-z0-9_]/g, "_").replace(/^(\d)/, "_$1") || "_";
const mtype = (s: string) => s.replace(/\s+/g, "_").replace(/,/g, "-").replace(/[^A-Za-z0-9_()[\]-]/g, "") || "unknown";

export function mermaidEr(doc: SchemaDoc): string {
  const lines = ["erDiagram"];
  const ent = (t: { schema: string; name: string }) => ident(displayName(doc, t));
  for (const t of doc.tables) {
    const fks = fkByColumn(t);
    lines.push(`  ${ent(t)} {`);
    for (const c of t.columns) {
      const keys = [t.primaryKey.includes(c.name) && "PK", fks.has(c.name) && "FK"].filter(Boolean).join(", ");
      lines.push(`    ${mtype(c.typeName)} ${ident(c.name)}${keys ? ` ${keys}` : ""}`);
    }
    lines.push("  }");
  }
  for (const t of doc.tables) {
    for (const fk of t.foreignKeys) {
      const parent = ent({ schema: fk.refSchema || t.schema, name: fk.refTable });
      const nullable = fk.columns.some((c) => t.columns.find((x) => x.name === c)?.nullable);
      const left = nullable ? "|o" : "||";
      const right = isOneToOne(t, fk) ? "o|" : "o{";
      lines.push(`  ${parent} ${left}--${right} ${ent(t)} : "${fk.columns.join(", ").replace(/"/g, "'")}"`);
    }
  }
  return lines.join("\n");
}

// ---------------------------------------------------------------- Markdown

const mdCell = (s: string | null | undefined) => (s ?? "").replace(/\|/g, "\\|").replace(/\r?\n/g, " ");
const anchor = (s: string) => s.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "");

function fkActions(fk: ForeignKeyDoc) {
  return [fk.onDelete && `ON DELETE ${fk.onDelete}`, fk.onUpdate && `ON UPDATE ${fk.onUpdate}`].filter(Boolean).join(", ");
}

export function toMarkdown(doc: SchemaDoc, title: string, engine: string): string {
  const s = docStats(doc);
  const inc = incomingRefs(doc);
  const out: string[] = [
    `# ${title}`,
    "",
    `${engine} · ${statsLine(doc)} · generated ${new Date().toLocaleString()}`,
    "",
    "## Contents",
    "",
    ...doc.tables.map((t) => `- [${displayName(doc, t)}](#${anchor(displayName(doc, t))})${t.kind !== "table" ? ` (${t.kind})` : ""}`),
    "",
  ];
  if (s.relations) out.push("## Relationships", "", "```mermaid", mermaidEr(doc), "```", "");
  for (const t of doc.tables) {
    const name = displayName(doc, t);
    const fks = fkByColumn(t);
    out.push(`## ${name}`, "");
    const meta = [t.kind !== "table" && `*${t.kind}*`, t.rows != null && `~${t.rows.toLocaleString()} rows`].filter(Boolean).join(" · ");
    if (meta) out.push(meta, "");
    if (t.comment) out.push(t.comment, "");
    out.push("| Column | Type | Null | Default | Key | Description |", "|---|---|---|---|---|---|");
    for (const c of t.columns) {
      const fk = fks.get(c.name);
      const key = [
        t.primaryKey.includes(c.name) && "PK",
        fk && `FK → [${refName(doc, t, fk)}](#${anchor(refName(doc, t, fk))}).${fk.refColumns[fk.columns.indexOf(c.name)] ?? ""}`,
      ]
        .filter(Boolean)
        .join(" ");
      out.push(`| ${mdCell(c.name)} | \`${mdCell(c.typeName)}\` | ${c.nullable ? "yes" : "no"} | ${c.default ? `\`${mdCell(c.default)}\`` : ""} | ${key} | ${mdCell(c.comment)} |`);
    }
    out.push("");
    if (t.foreignKeys.length) {
      out.push("**References**", "");
      for (const fk of t.foreignKeys) {
        const act = fkActions(fk);
        out.push(`- \`${fk.name}\`: (${fk.columns.join(", ")}) → [${refName(doc, t, fk)}](#${anchor(refName(doc, t, fk))}) (${fk.refColumns.join(", ")})${act ? ` – ${act}` : ""}`);
      }
      out.push("");
    }
    const refs = inc.get(tableKey(t.schema, t.name)) ?? [];
    if (refs.length) {
      out.push("**Referenced by**", "");
      for (const r of refs) out.push(`- [${displayName(doc, r.from)}](#${anchor(displayName(doc, r.from))}) (${r.fk.columns.join(", ")}) – \`${r.fk.name}\``);
      out.push("");
    }
    if (t.indexes.length) {
      out.push("**Indexes**", "");
      for (const i of t.indexes) out.push(`- \`${i.name}\` (${i.columns.join(", ")})${i.primary ? " – primary" : i.unique ? " – unique" : ""}`);
      out.push("");
    }
  }
  return out.join("\n");
}

// ---------------------------------------------------------------- HTML

const esc = (s: string | null | undefined) =>
  (s ?? "").replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");

export function toHtml(doc: SchemaDoc, title: string, engine: string): string {
  const s = docStats(doc);
  const inc = incomingRefs(doc);
  const id = (t: { schema: string; name: string }) => `t-${anchor(tableKey(t.schema, t.name))}`;
  const link = (schema: string, name: string) => `<a href="#${id({ schema, name })}">${esc(displayName(doc, { schema, name }))}</a>`;
  const toc = doc.tables.map((t) => `<li><a href="#${id(t)}">${esc(displayName(doc, t))}</a>${t.kind !== "table" ? ` <span class="tag">${esc(t.kind)}</span>` : ""}</li>`).join("");
  const cards = doc.tables
    .map((t) => {
      const fks = fkByColumn(t);
      const uniq = uniqueColumns(t);
      const rows = t.columns
        .map((c) => {
          const fk = fks.get(c.name);
          const keys = [
            t.primaryKey.includes(c.name) && `<span class="k pk">PK</span>`,
            fk && `<span class="k fk">FK</span>`,
            uniq.has(c.name) && `<span class="k uq">UQ</span>`,
          ]
            .filter(Boolean)
            .join("");
          const ref = fk ? `→ ${link(fk.refSchema || t.schema, fk.refTable)}.${esc(fk.refColumns[fk.columns.indexOf(c.name)])}` : "";
          return `<tr><td class="name">${esc(c.name)}</td><td>${keys}</td><td><code>${esc(c.typeName)}</code></td><td>${c.nullable ? "" : "NOT NULL"}</td><td>${c.default ? `<code>${esc(c.default)}</code>` : ""}</td><td>${[ref, esc(c.comment)].filter(Boolean).join("<br>")}</td></tr>`;
        })
        .join("");
      const out = t.foreignKeys
        .map((fk) => `<li><code>${esc(fk.name)}</code> (${esc(fk.columns.join(", "))}) → ${link(fk.refSchema || t.schema, fk.refTable)} (${esc(fk.refColumns.join(", "))})${fkActions(fk) ? ` <span class="muted">${esc(fkActions(fk))}</span>` : ""}</li>`)
        .join("");
      const back = (inc.get(tableKey(t.schema, t.name)) ?? [])
        .map((r) => `<li>${link(r.from.schema, r.from.name)} (${esc(r.fk.columns.join(", "))}) <span class="muted">${esc(r.fk.name)}</span></li>`)
        .join("");
      const idx = t.indexes
        .map((i) => `<li><code>${esc(i.name)}</code> (${esc(i.columns.join(", "))})${i.primary ? ` <span class="tag">primary</span>` : i.unique ? ` <span class="tag">unique</span>` : ""}</li>`)
        .join("");
      return `<section id="${id(t)}"><h2>${esc(displayName(doc, t))}${t.kind !== "table" ? ` <span class="tag">${esc(t.kind)}</span>` : ""}${t.rows != null ? ` <span class="muted small">~${t.rows.toLocaleString()} rows</span>` : ""}</h2>
${t.comment ? `<p>${esc(t.comment)}</p>` : ""}
<div class="scroll"><table><thead><tr><th>Column</th><th>Key</th><th>Type</th><th>Null</th><th>Default</th><th>Description</th></tr></thead><tbody>${rows}</tbody></table></div>
<div class="grid">${out ? `<div><h3>References</h3><ul>${out}</ul></div>` : ""}${back ? `<div><h3>Referenced by</h3><ul>${back}</ul></div>` : ""}${idx ? `<div><h3>Indexes</h3><ul>${idx}</ul></div>` : ""}</div></section>`;
    })
    .join("\n");
  const er = s.relations
    ? `<section><h2>Relationships</h2><pre class="mermaid">${esc(mermaidEr(doc))}</pre></section>
<script type="module">import mermaid from "https://cdn.jsdelivr.net/npm/mermaid@10.9.1/dist/mermaid.esm.min.mjs";
mermaid.initialize({ startOnLoad: true, theme: matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "default", er: { useMaxWidth: false } });</script>`
    : "";
  return `<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>${esc(title)}</title>
<style>
:root{--bg:#fff;--fg:#1d2330;--muted:#6b7385;--line:#e3e6ee;--panel:#f6f7fa;--accent:#5b5bd6;--pk:#b7791f;--fk:#2b6cb0;--uq:#2f855a}
@media (prefers-color-scheme:dark){:root{--bg:#121419;--fg:#e6e8ee;--muted:#8a91a3;--line:#2a2e38;--panel:#1a1d24;--accent:#8b8bf5;--pk:#ecc94b;--fk:#63b3ed;--uq:#68d391}}
*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--fg);font:14px/1.5 system-ui,-apple-system,Segoe UI,sans-serif}
.wrap{display:flex;max-width:1400px;margin:0 auto}nav{position:sticky;top:0;align-self:flex-start;width:240px;flex-shrink:0;height:100vh;overflow:auto;padding:24px 16px;border-right:1px solid var(--line)}
nav ul{list-style:none;padding:0;margin:0}nav li{padding:2px 0}main{flex:1;min-width:0;padding:24px 32px}
a{color:var(--accent);text-decoration:none}a:hover{text-decoration:underline}h1{margin:0 0 4px;font-size:24px}h2{font-size:18px;margin:0 0 8px}h3{font-size:12px;text-transform:uppercase;letter-spacing:.06em;color:var(--muted);margin:12px 0 4px}
section{border:1px solid var(--line);border-radius:12px;padding:16px 18px;margin:16px 0;background:var(--panel)}
table{border-collapse:collapse;width:100%;font-size:13px}th,td{text-align:left;padding:6px 8px;border-bottom:1px solid var(--line);vertical-align:top}th{color:var(--muted);font-weight:600;font-size:12px}
td.name{font-weight:600;white-space:nowrap}code{font:12px ui-monospace,SFMono-Regular,Menlo,monospace}.scroll{overflow-x:auto}
.k{display:inline-block;font-size:10px;font-weight:700;padding:1px 5px;border-radius:4px;margin-right:3px;border:1px solid currentColor}.pk{color:var(--pk)}.fk{color:var(--fk)}.uq{color:var(--uq)}
.tag{font-size:11px;font-weight:500;color:var(--muted);border:1px solid var(--line);border-radius:999px;padding:0 7px}.muted{color:var(--muted)}.small{font-size:12px;font-weight:400}
.grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(240px,1fr));gap:4px 24px}ul{margin:0;padding-left:18px}.mermaid{background:var(--bg);border-radius:8px;padding:12px;overflow:auto}
@media (max-width:800px){nav{display:none}main{padding:16px}}
</style></head><body><div class="wrap"><nav><strong>${esc(doc.database)}</strong><ul>${toc}</ul></nav><main>
<h1>${esc(title)}</h1><p class="muted">${esc(engine)} · ${esc(statsLine(doc))} · generated ${esc(new Date().toLocaleString())}</p>
${er}
${cards}
</main></div></body></html>`;
}
