import type { AuthKind, EntraOptions, Engine } from "./types";

export type Language = "sql" | "mongo" | "aql" | "redis" | "es";

export interface Flavor {
  id: string;
  engine: Engine;
  name: string;
  short: string;
  color: string;
  /** text color on top of `color` */
  ink?: string;
  port?: number;
  category: "Relational" | "Document" | "Key-value" | "Analytics" | "Search" | "Graph";
  blurb: string;
  defaults?: Partial<{ user: string; database: string; ssl: boolean; trustCert: boolean }>;
}

export const FLAVORS: Flavor[] = [
  { id: "postgres", engine: "postgres", name: "PostgreSQL", short: "Pg", color: "#336791", port: 5432, category: "Relational", blurb: "The world's most advanced open source database", defaults: { user: "postgres", database: "postgres" } },
  { id: "mysql", engine: "mysql", name: "MySQL", short: "My", color: "#00758f", port: 3306, category: "Relational", blurb: "Popular open source relational database", defaults: { user: "root" } },
  { id: "mariadb", engine: "mysql", name: "MariaDB", short: "Ma", color: "#c0765a", port: 3306, category: "Relational", blurb: "Community-developed MySQL fork", defaults: { user: "root" } },
  { id: "mssql", engine: "mssql", name: "SQL Server", short: "SQ", color: "#cc2927", port: 1433, category: "Relational", blurb: "Microsoft SQL Server & Azure SQL", defaults: { user: "sa", trustCert: true } },
  { id: "sqlite", engine: "sqlite", name: "SQLite", short: "Li", color: "#0f80cc", category: "Relational", blurb: "Embedded, file-based database" },
  { id: "mongodb", engine: "mongodb", name: "MongoDB", short: "Mo", color: "#00a35c", port: 27017, category: "Document", blurb: "Document database with mongo-shell queries" },
  { id: "arangodb", engine: "arangodb", name: "ArangoDB", short: "Ar", color: "#7fa93a", port: 8529, category: "Graph", blurb: "Multi-model graph & document DB (AQL)", defaults: { user: "root", database: "_system" } },
  { id: "redis", engine: "redis", name: "Redis", short: "Re", color: "#dc382d", port: 6379, category: "Key-value", blurb: "In-memory key-value store" },
  { id: "valkey", engine: "redis", name: "Valkey", short: "Va", color: "#4f46e5", port: 6379, category: "Key-value", blurb: "Open source Redis fork by the Linux Foundation" },
  { id: "clickhouse", engine: "clickhouse", name: "ClickHouse", short: "CH", color: "#faff69", ink: "#151515", port: 8123, category: "Analytics", blurb: "Column-oriented OLAP database", defaults: { user: "default", database: "default" } },
  { id: "elasticsearch", engine: "elasticsearch", name: "Elasticsearch", short: "El", color: "#00bfb3", port: 9200, category: "Search", blurb: "Search & analytics engine (console syntax)", defaults: { user: "elastic" } },
  { id: "opensearch", engine: "elasticsearch", name: "OpenSearch", short: "Os", color: "#005eb8", port: 9200, category: "Search", blurb: "Open source search suite", defaults: { user: "admin", trustCert: true } },
  { id: "cockroachdb", engine: "postgres", name: "CockroachDB", short: "Cr", color: "#6933ff", port: 26257, category: "Relational", blurb: "Distributed SQL, Postgres wire-compatible", defaults: { user: "root", database: "defaultdb" } },
  { id: "timescaledb", engine: "postgres", name: "TimescaleDB", short: "Ts", color: "#f5a524", ink: "#1b1300", port: 5432, category: "Analytics", blurb: "Time-series on PostgreSQL", defaults: { user: "postgres", database: "postgres" } },
  { id: "supabase", engine: "postgres", name: "Supabase", short: "Sb", color: "#3ecf8e", ink: "#062b1b", port: 5432, category: "Relational", blurb: "Hosted Postgres", defaults: { user: "postgres", database: "postgres", ssl: true, trustCert: true } },
  { id: "yugabytedb", engine: "postgres", name: "YugabyteDB", short: "Yb", color: "#ff6e42", port: 5433, category: "Relational", blurb: "Distributed Postgres-compatible SQL", defaults: { user: "yugabyte", database: "yugabyte" } },
  { id: "redshift", engine: "postgres", name: "Redshift", short: "Rs", color: "#8c4fff", port: 5439, category: "Analytics", blurb: "AWS data warehouse", defaults: { database: "dev", ssl: true } },
  { id: "tidb", engine: "mysql", name: "TiDB", short: "Ti", color: "#e30c34", port: 4000, category: "Relational", blurb: "Distributed MySQL-compatible SQL", defaults: { user: "root" } },
  { id: "azuresql", engine: "mssql", name: "Azure SQL", short: "Az", color: "#0078d4", port: 1433, category: "Relational", blurb: "Managed SQL Server in Azure", defaults: { ssl: true } },
];

export function flavorOf(engine: Engine, flavor?: string | null): Flavor {
  return FLAVORS.find((f) => f.id === flavor) ?? FLAVORS.find((f) => f.engine === engine)!;
}

export interface EngineMeta {
  language: Language;
  /** root tree children are databases → show database selector */
  hasDatabases: boolean;
  /** second level are schemas → show schema selector */
  hasSchemas: boolean;
  usesFile?: boolean;
  /** tree depth of tables / collections (import targets): postgres = db/schema/table */
  tableDepth: number;
  /** names of the tree levels down to the table */
  levels: string[];
  urlPlaceholder: string;
  starter: string;
}

export const ENGINE_META: Record<Engine, EngineMeta> = {
  postgres: { tableDepth: 3, levels: ["Database", "Schema", "Table"], language: "sql", hasDatabases: true, hasSchemas: true, urlPlaceholder: "postgres://user:pass@host:5432/db?sslmode=require", starter: "SELECT now(), current_database(), version();" },
  mysql: { tableDepth: 2, levels: ["Database", "Table"], language: "sql", hasDatabases: true, hasSchemas: false, urlPlaceholder: "mysql://user:pass@host:3306/db", starter: "SELECT NOW(), DATABASE(), VERSION();" },
  sqlite: { tableDepth: 1, levels: ["Table"], language: "sql", hasDatabases: false, hasSchemas: false, usesFile: true, urlPlaceholder: "sqlite://path/to/file.db", starter: "SELECT name, type FROM sqlite_master ORDER BY name;" },
  mssql: { tableDepth: 3, levels: ["Database", "Schema", "Table"], language: "sql", hasDatabases: true, hasSchemas: true, urlPlaceholder: "server=tcp:host,1433;user=sa;password=...;TrustServerCertificate=true", starter: "SELECT @@VERSION AS version, DB_NAME() AS db, SYSDATETIME() AS now;" },
  mongodb: { tableDepth: 2, levels: ["Database", "Collection"], language: "mongo", hasDatabases: true, hasSchemas: false, urlPlaceholder: "mongodb+srv://user:pass@cluster.example.net/db", starter: "show collections\n\n// db.users.find({ age: { $gt: 21 } }).sort({ _id: -1 }).limit(50)" },
  arangodb: { tableDepth: 2, levels: ["Database", "Collection"], language: "aql", hasDatabases: true, hasSchemas: false, urlPlaceholder: "https://my-cluster.arangodb.cloud:8529", starter: "FOR c IN COLLECTIONS()\n  FILTER !STARTS_WITH(c.name, \"_\")\n  RETURN c" },
  redis: { tableDepth: 1, levels: ["Database"], language: "redis", hasDatabases: true, hasSchemas: false, urlPlaceholder: "redis://:password@host:6379/0", starter: "INFO server\nDBSIZE\nSCAN 0 COUNT 100" },
  clickhouse: { tableDepth: 2, levels: ["Database", "Table"], language: "sql", hasDatabases: true, hasSchemas: false, urlPlaceholder: "https://host:8443", starter: "SELECT version(), uptime(), currentDatabase();" },
  elasticsearch: { tableDepth: 1, levels: ["Index"], language: "es", hasDatabases: false, hasSchemas: false, urlPlaceholder: "https://host:9200", starter: "GET /_cat/indices?v\n\nGET /_cluster/health" },
};

export const CONNECTION_COLORS = ["", "#8b5cf6", "#22d3ee", "#34d399", "#fbbf24", "#f97316", "#f87171", "#ec4899"];

export const AUTH_METHODS: { id: AuthKind; label: string; hint: string }[] = [
  { id: "password", label: "User & password", hint: "Database user name and password" },
  { id: "entraInteractive", label: "Entra ID · Interactive (browser)", hint: "Sign in with Microsoft in your browser – supports MFA and Conditional Access" },
  { id: "entraDeviceCode", label: "Entra ID · Device code", hint: "Enter a code on microsoft.com/devicelogin from any device" },
  { id: "entraAzureCli", label: "Entra ID · Azure CLI", hint: "Reuse the account signed in with `az login`" },
  { id: "entraClientSecret", label: "Entra ID · Service principal (secret)", hint: "App registration with a client secret" },
  { id: "entraCertificate", label: "Entra ID · Service principal (certificate)", hint: "App registration with a certificate (PEM incl. private key)" },
  { id: "entraManagedIdentity", label: "Entra ID · Managed identity", hint: "Only when Nexus DB runs on an Azure VM, App Service, AKS or Arc server" },
  { id: "entraAccessToken", label: "Entra ID · Access token", hint: "Paste a token you obtained elsewhere" },
];

export const EMPTY_ENTRA: EntraOptions = { cloud: "public", tenantId: "", clientId: "", clientSecret: "", certificatePath: "", scope: "", identityClientId: "" };

export const AZURE_CLOUDS = [
  { id: "public", name: "Azure (global)" },
  { id: "usgov", name: "Azure US Government" },
  { id: "china", name: "Azure China (21Vianet)" },
];

export interface EntraSupport {
  /** what the server must be, shown under the auth selector */
  note: string;
  /** label + placeholder for the user field with Entra auth */
  userLabel: string;
  userPlaceholder: string;
  /** true when the engine has no built-in Entra resource and needs a scope */
  needsScope: boolean;
}

/** `null` = Entra ID does not apply (SQLite is a local file). */
export function entraSupport(engine: Engine): EntraSupport | null {
  switch (engine) {
    case "mssql":
      return { note: "Azure SQL Database, Managed Instance, Synapse and Fabric SQL endpoints.", userLabel: "User", userPlaceholder: "not needed", needsScope: false };
    case "postgres":
      return { note: "Azure Database for PostgreSQL. The user is the Entra user, group or identity name the role was created with.", userLabel: "Entra role", userPlaceholder: "signed-in user", needsScope: false };
    case "mysql":
      return { note: "Azure Database for MySQL. Uses the cleartext plugin over TLS. The user is the Entra user, group or identity name.", userLabel: "Entra user", userPlaceholder: "signed-in user", needsScope: false };
    case "mongodb":
      return { note: "Azure Cosmos DB for MongoDB vCore and other MONGODB-OIDC servers (set the scope for Atlas workforce identity).", userLabel: "Principal", userPlaceholder: "optional", needsScope: false };
    case "redis":
      return { note: "Azure Cache for Redis / Azure Managed Redis. TLS is always used; connections re-authenticate before the token expires.", userLabel: "Object ID", userPlaceholder: "from token", needsScope: false };
    case "clickhouse":
    case "elasticsearch":
    case "arangodb":
      return { note: "Sends an Entra bearer token – for servers or gateways that validate Entra JWTs. Enter the scope they accept.", userLabel: "User", userPlaceholder: "not used", needsScope: true };
    case "sqlite":
      return null;
  }
}

/** Mirrors the backend's built-in scopes, for the placeholder. */
export function defaultEntraScope(engine: Engine, cloud: string): string | null {
  const suffix = cloud === "usgov" ? "database.usgovcloudapi.net" : cloud === "china" ? "database.chinacloudapi.cn" : "database.windows.net";
  if (engine === "mssql") return `https://${suffix}/.default`;
  if (engine === "postgres" || engine === "mysql" || engine === "mongodb") return `https://ossrdbms-aad.${suffix}/.default`;
  if (engine === "redis" && (cloud === "public" || !cloud)) return "https://redis.azure.com/.default";
  return null;
}
