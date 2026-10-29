export type Engine =
  | "postgres"
  | "mysql"
  | "sqlite"
  | "mssql"
  | "mongodb"
  | "arangodb"
  | "redis"
  | "clickhouse"
  | "elasticsearch";

export interface ConnectionConfig {
  id: string;
  name: string;
  engine: Engine;
  flavor?: string | null;
  host: string;
  port?: number | null;
  user: string;
  password: string;
  database: string;
  filePath: string;
  url: string;
  ssl: boolean;
  trustCert: boolean;
  readOnly: boolean;
  color: string;
  group: string;
  /** missing on connections saved before Entra support → "password" */
  auth?: AuthKind;
  entra?: EntraOptions;
}

export type AuthKind =
  | "password"
  | "entraInteractive"
  | "entraDeviceCode"
  | "entraClientSecret"
  | "entraCertificate"
  | "entraManagedIdentity"
  | "entraAzureCli"
  | "entraAccessToken";

export interface EntraOptions {
  cloud: string;
  tenantId: string;
  clientId: string;
  clientSecret: string;
  certificatePath: string;
  scope: string;
  identityClientId: string;
}

export type EntraLoginEvent =
  | { state: "browser"; url: string }
  | { state: "deviceCode"; url: string; code: string; message: string }
  | { state: "done" };

export interface TreeNode {
  name: string;
  kind: string;
  path: string[];
  expandable: boolean;
  detail?: string | null;
}

export interface Column {
  name: string;
  typeName: string;
}

export type Cell = string | number | boolean | null | Cell[] | { [k: string]: Cell };

export interface ResultSet {
  columns: Column[];
  rows: Cell[][];
  affected?: number | null;
  truncated: boolean;
  message?: string | null;
  documents: boolean;
}

export interface ExecResponse {
  sets: ResultSet[];
  elapsedMs: number;
}

export interface ExecContext {
  database?: string | null;
  schema?: string | null;
  limit: number;
}

export interface ObjectQuery {
  query: string;
  database?: string | null;
  schema?: string | null;
}

export interface ServerInfo {
  version: string;
}

export interface TestResult {
  version: string;
  elapsedMs: number;
}

export interface EditColumn {
  name: string;
  typeName: string;
  nullable: boolean;
  hasDefault: boolean;
  key: boolean;
}

export interface EditInfo {
  columns: EditColumn[];
  keyColumns: string[];
  editable: boolean;
  reason?: string | null;
  /** document store: any key may be written, dotted keys nest */
  documents: boolean;
  canUpsert: boolean;
  upsertNote?: string | null;
  transactional: boolean;
}

export type RowChange =
  | { kind: "update"; key: Record<string, Cell>; values: Record<string, Cell> }
  | { kind: "insert"; values: Record<string, Cell> }
  | { kind: "delete"; key: Record<string, Cell> };

export interface ApplyReport {
  statements: string[];
  applied: number;
  atomic: boolean;
  error?: string | null;
}

/** Unsaved grid changes of a tab. Row numbers index the result rows; each entry
 *  also keeps the row's key so it can be re-attached after a refresh. */
export interface PendingEdits {
  updates: Record<number, { key: Record<string, Cell>; values: Record<string, Cell> }>;
  inserts: Record<string, Cell>[];
  deletes: Record<number, Record<string, Cell>>;
}

export interface FileOptions {
  path: string;
  sheet?: string | null;
  hasHeader?: boolean;
  delimiter?: string | null;
  encoding?: string | null;
}

export interface ImportPreview {
  sheets: string[];
  sheet?: string | null;
  columns: string[];
  rows: Cell[][];
  firstRow: number;
  delimiter?: string | null;
  encoding?: string | null;
  errorCells: number;
  totalRows: number;
}

export type ImportMode = "insert" | "upsert";

export interface ImportOptions {
  mode: ImportMode;
  keyColumns: string[];
  atomic: boolean;
  batchSize: number;
  keyTemplate: string;
}

export interface ValueOptions {
  trim: boolean;
  emptyAsNull: boolean;
  detectTypes: boolean;
}

export interface ImportReport {
  written: number;
  failed: number;
  errors: { row: number; message: string }[];
  cancelled: boolean;
  rolledBack: boolean;
}

export interface XlsxSheet {
  name: string;
  columns: Column[];
  rows: Cell[][];
}
