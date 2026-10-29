import { invoke } from "@tauri-apps/api/core";
import type {
  ConnectionConfig,
  ExecContext,
  ApplyReport,
  DumpOptions,
  DumpReport,
  RestoreOptions,
  RestoreReport,
  SchemaDoc,
  ToolStatus,
  EditInfo,
  ExecResponse,
  FileOptions,
  ImportOptions,
  ImportPreview,
  ImportReport,
  RowChange,
  ValueOptions,
  XlsxSheet,
  ObjectQuery,
  ResultSet,
  ServerInfo,
  TestResult,
  TreeNode,
} from "./types";
import { mockInvoke } from "./mock";

export const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

/** Outside Tauri (plain `npm run dev` in a browser) the UI runs on demo data. */
function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  return isTauri ? invoke<T>(cmd, args) : mockInvoke<T>(cmd, args ?? {});
}

export const api = {
  listConnections: () => call<ConnectionConfig[]>("list_connections"),
  saveConnection: (config: ConnectionConfig) => call<ConnectionConfig>("save_connection", { config }),
  deleteConnection: (id: string) => call<void>("delete_connection", { id }),
  testConnection: (config: ConnectionConfig) => call<TestResult>("test_connection", { config }),
  connect: (id: string) => call<ServerInfo>("connect", { id }),
  disconnect: (id: string) => call<void>("disconnect", { id }),
  listChildren: (id: string, path: string[]) => call<TreeNode[]>("list_children", { id, path }),
  execute: (id: string, query: string, ctx: ExecContext) => call<ExecResponse>("execute", { id, query, ctx }),
  objectQuery: (id: string, path: string[]) => call<ObjectQuery>("object_query", { id, path }),
  describe: (id: string, path: string[]) => call<ResultSet>("describe", { id, path }),
  writeTextFile: (path: string, contents: string) => call<void>("write_text_file", { path, contents }),
  searchChildren: (id: string, path: string[], pattern: string) => call<TreeNode[]>("search_children", { id, path, pattern }),
  editInfo: (id: string, path: string[]) => call<EditInfo>("edit_info", { id, path }),
  applyChanges: (id: string, path: string[], changes: RowChange[], dryRun: boolean) => call<ApplyReport>("apply_changes", { id, path, changes, dryRun }),
  writeXlsx: (path: string, sheets: XlsxSheet[]) => call<void>("write_xlsx", { path, sheets }),
  importPreview: (file: FileOptions) => call<ImportPreview>("import_preview", { file }),
  importRun: (id: string, file: FileOptions, target: string[], mappings: { source: number; target: string }[], options: ImportOptions, values: ValueOptions) =>
    call<ImportReport>("import_run", { id, file, target, mappings, options, values }),
  importCancel: () => call<void>("import_cancel"),
  dumpTools: (id: string, toolDir: string) => call<ToolStatus[]>("dump_tools", { id, toolDir }),
  dumpRun: (id: string, options: DumpOptions) => call<DumpReport>("dump_run", { id, options }),
  restoreRun: (id: string, options: RestoreOptions) => call<RestoreReport>("restore_run", { id, options }),
  dumpCancel: () => call<void>("dump_cancel"),
  schemaDoc: (id: string, database?: string | null, schema?: string | null) => call<SchemaDoc>("schema_doc", { id, database, schema }),
  entraCancel: () => call<void>("entra_cancel"),
  entraSignOut: (config: ConnectionConfig) => call<void>("entra_sign_out", { config }),
  openUrl: (url: string) => call<void>("open_url", { url }),
};

export function errorText(e: unknown): string {
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  return JSON.stringify(e);
}
