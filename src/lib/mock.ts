// Demo backend used when the UI runs in a plain browser (no Tauri runtime).
import type { Cell, ConnectionConfig, ResultSet, TreeNode } from "./types";

const base = { host: "localhost", user: "", password: "", database: "", filePath: "", url: "", ssl: false, trustCert: false, readOnly: false, group: "" };

let connections: ConnectionConfig[] = [
  { ...base, id: "demo-pg", name: "Production · Postgres", engine: "postgres", flavor: "postgres", port: 5432, user: "app", database: "shop", color: "#8b5cf6", group: "Production" },
  { ...base, id: "demo-mongo", name: "Events cluster", engine: "mongodb", flavor: "mongodb", port: 27017, database: "analytics", color: "#34d399", group: "Production" },
  { ...base, id: "demo-mssql", name: "ERP · SQL Server", engine: "mssql", flavor: "mssql", port: 1433, user: "sa", color: "", group: "Staging" },
  { ...base, id: "demo-arango", name: "Knowledge graph", engine: "arangodb", flavor: "arangodb", port: 8529, user: "root", database: "_system", color: "#22d3ee", group: "Staging" },
  { ...base, id: "demo-redis", name: "Session cache", engine: "redis", flavor: "redis", port: 6379, color: "#f87171", group: "" },
  { ...base, id: "demo-ch", name: "Clickstream", engine: "clickhouse", flavor: "clickhouse", port: 8123, database: "default", color: "", group: "" },
];

const names = ["Ava Thompson", "Liam Chen", "Sofia Rossi", "Noah Patel", "Mia Johansson", "Lucas Silva", "Emma Müller", "Arjun Nair", "Chloe Martin", "Kenji Sato", "Zara Ahmed", "Mateo García"];
const cities = ["Berlin", "Toronto", "Kochi", "Lisbon", "Osaka", "Austin", "Paris", "Oslo"];

function rng(seed: number) {
  return () => ((seed = (seed * 16807) % 2147483647) - 1) / 2147483646;
}

function users(n: number): ResultSet {
  const r = rng(7);
  const rows: Cell[][] = Array.from({ length: n }, (_, i) => {
    const name = names[i % names.length];
    return [
      i + 1,
      name,
      `${name.split(" ")[0].toLowerCase()}${i > 11 ? i : ""}@example.com`,
      cities[Math.floor(r() * cities.length)],
      Math.round(r() * 500000) / 100,
      r() > 0.25,
      r() > 0.8 ? null : { plan: r() > 0.5 ? "pro" : "free", tags: ["beta", "newsletter"].slice(0, 1 + Math.floor(r() * 2)) },
      new Date(Date.UTC(2025, Math.floor(r() * 12), 1 + Math.floor(r() * 27), Math.floor(r() * 24), 13)).toISOString().replace("T", " ").slice(0, 19),
    ];
  });
  return {
    columns: [
      { name: "id", typeName: "int8" },
      { name: "full_name", typeName: "text" },
      { name: "email", typeName: "varchar" },
      { name: "city", typeName: "text" },
      { name: "lifetime_value", typeName: "numeric" },
      { name: "active", typeName: "bool" },
      { name: "preferences", typeName: "jsonb" },
      { name: "created_at", typeName: "timestamptz" },
    ],
    rows,
    truncated: false,
    documents: false,
  };
}

function docs(n: number): ResultSet {
  const r = rng(11);
  const events = ["page_view", "signup", "checkout", "add_to_cart", "search"];
  return {
    columns: ["_id", "event", "userId", "props", "ts"].map((name) => ({ name, typeName: "" })),
    rows: Array.from({ length: n }, (_, i) => [
      `ObjectId('66a1f0${(0x1000000 + i).toString(16)}c3d9e8f7a6b')`,
      events[Math.floor(r() * events.length)],
      1 + Math.floor(r() * 400),
      { path: ["/", "/pricing", "/docs", "/cart"][Math.floor(r() * 4)], device: r() > 0.5 ? "mobile" : "desktop", ms: Math.round(r() * 900) },
      new Date(Date.UTC(2026, 9, 20, Math.floor(r() * 24), Math.floor(r() * 60))).toISOString(),
    ]),
    truncated: false,
    documents: true,
  };
}

const tables = ["customers", "orders", "order_items", "products", "inventory", "payments", "shipments", "reviews"];

function tree(id: string, path: string[]): TreeNode[] {
  const node = (name: string, kind: string, expandable: boolean, detail?: string): TreeNode => ({ name, kind, path: [...path, name], expandable, detail });
  const conn = connections.find((c) => c.id === id)!;
  switch (conn.engine) {
    case "postgres":
    case "mssql":
      if (path.length === 0) return ["shop", "billing", "postgres"].map((d) => node(d, "database", true));
      if (path.length === 1) return (conn.engine === "mssql" ? ["dbo", "sales"] : ["public", "audit", "reporting"]).map((s) => node(s, "schema", true));
      if (path.length === 2) return [...tables.map((t, i) => node(t, "table", true, `~${(i + 3) * 1371} rows`)), node("active_customers", "view", true)];
      return [node("id", "pk", false, "bigint"), node("full_name", "column", false, "text"), node("email", "column", false, "varchar(255)"), node("created_at", "column", false, "timestamptz")];
    case "mongodb":
      if (path.length === 0) return ["analytics", "app", "admin"].map((d) => node(d, "database", true));
      if (path.length === 1) return ["events", "sessions", "users", "experiments"].map((c) => node(c, "collection", true));
      return [node("_id_", "index", false, "_id"), node("userId_1_ts_-1", "index", false, "userId, ts")];
    case "arangodb":
      if (path.length === 0) return ["_system", "knowledge"].map((d) => node(d, "database", true));
      if (path.length === 1) return [node("concepts", "collection", true), node("people", "collection", true), node("relates_to", "edge", true, "edge")];
      return [node("primary", "index", false, "primary (_key)")];
    case "redis":
      if (path.length === 0) return Array.from({ length: 16 }, (_, i) => node(`db${i}`, "database", true, i < 2 ? `${[1284, 12][i]} keys` : ""));
      return ["session:8f2a", "session:91bc", "rate:api:42", "user:42:profile", "leaderboard", "queue:emails"].map((k, i) => node(k, "key", false, ["string", "string", "string", "hash", "zset", "list"][i]));
    default:
      if (path.length === 0) return ["default", "system"].map((d) => node(d, "database", true));
      if (path.length === 1) return ["clicks", "sessions_mv"].map((t) => node(t, t.endsWith("mv") ? "view" : "table", true, "MergeTree"));
      return [node("ts", "pk", false, "DateTime"), node("url", "column", false, "String")];
  }
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

export async function mockInvoke<T>(cmd: string, args: Record<string, any>): Promise<T> {
  await sleep(120 + Math.random() * 180);
  const out = (() => {
    switch (cmd) {
      case "list_connections":
        return connections;
      case "save_connection": {
        const c = { ...args.config, id: args.config.id || crypto.randomUUID() };
        connections = connections.some((x) => x.id === c.id) ? connections.map((x) => (x.id === c.id ? c : x)) : [...connections, c];
        return c;
      }
      case "delete_connection":
        connections = connections.filter((c) => c.id !== args.id);
        return null;
      case "test_connection":
        if (!args.config.host && !args.config.url && !args.config.filePath) throw "Host is required";
        return { version: "Demo server 1.0 (browser preview)", elapsedMs: 42 };
      case "connect": {
        const c = connections.find((x) => x.id === args.id)!;
        const v: Record<string, string> = { postgres: "PostgreSQL 17.2", mongodb: "MongoDB 8.0.4", mssql: "SQL Server 16.0.4165.4 Developer Edition", arangodb: "ArangoDB 3.12.4", redis: "Redis 8.0.5", clickhouse: "ClickHouse 25.8.2" };
        return { version: v[c.engine] ?? "Demo" };
      }
      case "disconnect":
        return null;
      case "list_children":
        return tree(args.id, args.path);
      case "execute": {
        const q: string = args.query.toLowerCase();
        if (q.includes("error")) throw 'error returned from database: relation "error" does not exist';
        const c = connections.find((x) => x.id === args.id)!;
        if (q.startsWith("update") || q.startsWith("delete") || q.startsWith("insert")) return { sets: [{ columns: [], rows: [], affected: 3, truncated: false, documents: false }], elapsedMs: 18 };
        const set = c.engine === "mongodb" || c.engine === "arangodb" ? docs(240) : users(1000);
        return { sets: [set, ...(q.includes(";\n") ? [users(12)] : [])], elapsedMs: 23 + Math.round(Math.random() * 60) };
      }
      case "object_query": {
        const c = connections.find((x) => x.id === args.id)!;
        const p: string[] = args.path;
        const queries: Record<string, string> = {
          postgres: `SELECT *\nFROM "${p[1]}"."${p[2]}"\nLIMIT 100;`,
          mssql: `SELECT TOP 100 *\nFROM [${p[1]}].[${p[2]}];`,
          mongodb: `db.${p[1]}.find({})\n  .sort({ _id: -1 })\n  .limit(100)`,
          arangodb: `FOR doc IN \`${p[1]}\`\n  SORT doc._key DESC\n  LIMIT 100\n  RETURN doc`,
          redis: `GET ${p[1]}\nTTL ${p[1]}`,
          clickhouse: `SELECT *\nFROM \`${p[0]}\`.\`${p[1]}\`\nLIMIT 100`,
        };
        return { query: queries[c.engine] ?? "SELECT 1", database: p[0], schema: c.engine === "postgres" || c.engine === "mssql" ? p[1] : null };
      }
      case "describe":
        return {
          columns: ["column", "type", "nullable", "default", "primary_key"].map((name) => ({ name, typeName: "text" })),
          rows: [
            ["id", "bigint", false, "nextval('customers_id_seq')", true],
            ["full_name", "text", false, null, false],
            ["email", "varchar(255)", false, null, false],
            ["preferences", "jsonb", true, "'{}'::jsonb", false],
            ["created_at", "timestamptz", false, "now()", false],
          ],
          truncated: false,
          documents: false,
        };
      case "search_children":
        return tree(args.id, args.path).filter((n) => n.name.toLowerCase().includes(String(args.pattern).toLowerCase()));
      case "edit_info": {
        const c = connections.find((x) => x.id === args.id)!;
        if (c.engine === "mongodb" || c.engine === "arangodb")
          return { columns: ["_id", "event", "userId", "props", "ts"].map((name, i) => ({ name, typeName: "", nullable: i > 0, hasDefault: false, key: i === 0 })), keyColumns: ["_id"], editable: true, documents: true, canUpsert: true, transactional: false, reason: "Changes are saved one by one." };
        if (c.engine === "redis") return { columns: [], keyColumns: [], editable: false, documents: true, canUpsert: true, transactional: false, reason: "Edit Redis values with commands." };
        return {
          columns: users(1).columns.map((col, i) => ({ name: col.name, typeName: col.typeName, nullable: i > 1, hasDefault: i === 0, key: i === 0 })),
          keyColumns: ["id"],
          editable: c.engine !== "clickhouse",
          documents: false,
          canUpsert: c.engine !== "clickhouse",
          transactional: c.engine !== "clickhouse",
          reason: c.engine === "clickhouse" ? "ClickHouse tables are not edited row by row." : null,
        };
      }
      case "apply_changes":
        return { statements: (args.changes as { kind: string }[]).map((ch) => `-- demo: ${ch.kind} ${JSON.stringify(ch)}`), applied: args.dryRun ? 0 : args.changes.length, atomic: true, error: null };
      case "import_preview": {
        const set = users(40);
        return { sheets: String(args.file.path).endsWith(".xlsx") ? ["Customers", "Orders"] : [], sheet: null, columns: ["ID", "Full Name", "E-mail", "City", "Lifetime value", "Active", "Notes", "Signed up"], rows: set.rows.map((r) => r.map((v) => (v !== null && typeof v === "object" ? "" : v))), firstRow: 2, delimiter: ",", encoding: "UTF-8", errorCells: 0, totalRows: 1250 };
      }
      case "import_run":
        return { written: 1247, failed: 3, errors: [{ row: 18, message: 'null value in column "email" violates not-null constraint' }, { row: 311, message: "duplicate key value violates unique constraint \"customers_email_key\"" }, { row: 902, message: 'invalid input syntax for type numeric: "n/a"' }], cancelled: false, rolledBack: false };
      case "write_xlsx":
      case "import_cancel":
      case "write_text_file":
      case "entra_cancel":
      case "entra_sign_out":
      case "open_url":
        return null;
      default:
        throw `Unknown command ${cmd}`;
    }
  })();
  return out as T;
}
