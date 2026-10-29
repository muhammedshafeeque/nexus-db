use dbcore::dump;
use dbcore::tabular::{self, FileOptions, Table, ValueOptions};
use dbcore::{
    ApplyReport, ConnectionConfig, Driver, EditInfo, ExecContext, ImportCtx, ImportOptions, ImportReport, ObjectQuery, ResultSet,
    RowChange, ServerInfo, TreeNode,
};
use serde::Deserialize;
use std::sync::atomic::{AtomicBool, Ordering};
use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tauri::{Emitter, Manager, State};
use tokio::sync::{Mutex, RwLock};

type CmdResult<T> = Result<T, String>;

fn err(e: anyhow::Error) -> String {
    format!("{e:#}")
}

struct AppState {
    store_path: PathBuf,
    configs: Mutex<Vec<ConnectionConfig>>,
    live: RwLock<HashMap<String, Arc<dyn Driver>>>,
}

impl AppState {
    fn load(store_path: PathBuf) -> Self {
        let configs = std::fs::read_to_string(&store_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self { store_path, configs: Mutex::new(configs), live: RwLock::default() }
    }

    async fn persist(&self, configs: &[ConnectionConfig]) -> CmdResult<()> {
        if let Some(dir) = self.store_path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string_pretty(configs).map_err(|e| e.to_string())?;
        std::fs::write(&self.store_path, json).map_err(|e| e.to_string())
    }

    async fn config(&self, id: &str) -> CmdResult<ConnectionConfig> {
        self.configs
            .lock()
            .await
            .iter()
            .find(|c| c.id == id)
            .cloned()
            .ok_or_else(|| "Unknown connection".to_string())
    }

    async fn driver(&self, id: &str) -> CmdResult<Arc<dyn Driver>> {
        if let Some(d) = self.live.read().await.get(id) {
            return Ok(d.clone());
        }
        let cfg = self.config(id).await?;
        let driver = dbcore::connect(&cfg).await.map_err(err)?;
        self.live.write().await.insert(id.to_string(), driver.clone());
        Ok(driver)
    }

    async fn drop_live(&self, id: &str) {
        if let Some(d) = self.live.write().await.remove(id) {
            d.close().await;
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TestResult {
    version: String,
    elapsed_ms: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExecResponse {
    sets: Vec<ResultSet>,
    elapsed_ms: u64,
}

#[tauri::command]
async fn list_connections(state: State<'_, AppState>) -> CmdResult<Vec<ConnectionConfig>> {
    Ok(state.configs.lock().await.clone())
}

#[tauri::command]
async fn save_connection(state: State<'_, AppState>, mut config: ConnectionConfig) -> CmdResult<ConnectionConfig> {
    if config.id.is_empty() {
        config.id = uuid::Uuid::new_v4().to_string();
    }
    let mut configs = state.configs.lock().await;
    match configs.iter_mut().find(|c| c.id == config.id) {
        Some(existing) => *existing = config.clone(),
        None => configs.push(config.clone()),
    }
    state.persist(&configs).await?;
    drop(configs);
    // settings may have changed – reconnect lazily on next use
    state.drop_live(&config.id).await;
    Ok(config)
}

#[tauri::command]
async fn delete_connection(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    state.drop_live(&id).await;
    let mut configs = state.configs.lock().await;
    configs.retain(|c| c.id != id);
    state.persist(&configs).await
}

#[tauri::command]
async fn test_connection(config: ConnectionConfig) -> CmdResult<TestResult> {
    let started = Instant::now();
    let driver = dbcore::connect(&config).await.map_err(err)?;
    let info = driver.server_info().await.map_err(err)?;
    driver.close().await;
    Ok(TestResult { version: info.version, elapsed_ms: started.elapsed().as_millis() as u64 })
}

#[tauri::command]
async fn connect(state: State<'_, AppState>, id: String) -> CmdResult<ServerInfo> {
    let driver = state.driver(&id).await?;
    match driver.server_info().await {
        Ok(info) => Ok(info),
        Err(e) => {
            state.drop_live(&id).await;
            Err(err(e))
        }
    }
}

#[tauri::command]
async fn disconnect(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    state.drop_live(&id).await;
    Ok(())
}

#[tauri::command]
async fn list_children(state: State<'_, AppState>, id: String, path: Vec<String>) -> CmdResult<Vec<TreeNode>> {
    state.driver(&id).await?.list_children(&path).await.map_err(err)
}

#[tauri::command]
async fn execute(
    state: State<'_, AppState>,
    id: String,
    query: String,
    ctx: ExecContext,
) -> CmdResult<ExecResponse> {
    let cfg = state.config(&id).await?;
    if cfg.read_only && dbcore::is_mutating(cfg.engine, &query) {
        return Err("This connection is read-only – the query looks like it modifies data.".into());
    }
    let driver = state.driver(&id).await?;
    let started = Instant::now();
    let mut sets = driver.execute(&query, &ctx).await.map_err(err)?;
    let limit = ctx.limit();
    for set in &mut sets {
        if set.rows.len() > limit {
            set.rows.truncate(limit);
            set.truncated = true;
        }
    }
    Ok(ExecResponse { sets, elapsed_ms: started.elapsed().as_millis() as u64 })
}

#[tauri::command]
async fn object_query(state: State<'_, AppState>, id: String, path: Vec<String>) -> CmdResult<ObjectQuery> {
    state.driver(&id).await?.object_query(&path).await.map_err(err)
}

#[tauri::command]
async fn describe(state: State<'_, AppState>, id: String, path: Vec<String>) -> CmdResult<ResultSet> {
    state.driver(&id).await?.describe(&path).await.map_err(err)
}

/// Aborts a pending browser / device-code sign-in.
#[tauri::command]
fn entra_cancel() {
    dbcore::auth::cancel_login();
}

/// Forgets the Entra sign-in used by a connection and closes it.
#[tauri::command]
async fn entra_sign_out(state: State<'_, AppState>, config: ConnectionConfig) -> CmdResult<()> {
    dbcore::auth::sign_out(&config);
    if !config.id.is_empty() {
        state.drop_live(&config.id).await;
    }
    Ok(())
}

#[tauri::command]
fn open_url(url: String) -> CmdResult<()> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("Only web links can be opened".into());
    }
    dbcore::auth::open_browser(&url).map_err(err)
}

#[tauri::command]
async fn search_children(state: State<'_, AppState>, id: String, path: Vec<String>, pattern: String) -> CmdResult<Vec<TreeNode>> {
    state.driver(&id).await?.search_children(&path, &pattern).await.map_err(err)
}

#[tauri::command]
async fn edit_info(state: State<'_, AppState>, id: String, path: Vec<String>) -> CmdResult<EditInfo> {
    let mut info = state.driver(&id).await?.edit_info(&path).await.map_err(err)?;
    if state.config(&id).await?.read_only && info.editable {
        info.editable = false;
        info.reason = Some("This connection is read-only.".into());
    }
    Ok(info)
}

#[tauri::command]
async fn apply_changes(
    state: State<'_, AppState>,
    id: String,
    path: Vec<String>,
    changes: Vec<RowChange>,
    dry_run: bool,
) -> CmdResult<ApplyReport> {
    if !dry_run && state.config(&id).await?.read_only {
        return Err("This connection is read-only.".into());
    }
    state.driver(&id).await?.apply_changes(&path, &changes, dry_run).await.map_err(err)
}

#[tauri::command]
async fn write_xlsx(path: String, sheets: Vec<tabular::Sheet>) -> CmdResult<()> {
    tokio::task::spawn_blocking(move || tabular::write_xlsx(&path, &sheets)).await.map_err(|e| e.to_string())?.map_err(err)
}

const PREVIEW_ROWS: usize = 100;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ImportPreview {
    #[serde(flatten)]
    table: Table,
    total_rows: usize,
}

async fn read_file(file: FileOptions) -> CmdResult<Table> {
    tokio::task::spawn_blocking(move || tabular::read_table(&file)).await.map_err(|e| e.to_string())?.map_err(err)
}

#[tauri::command]
async fn import_preview(file: FileOptions) -> CmdResult<ImportPreview> {
    let mut table = read_file(file).await?;
    let total_rows = table.rows.len();
    table.rows.truncate(PREVIEW_ROWS);
    Ok(ImportPreview { table, total_rows })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ColumnMapping {
    /// Column index in the file.
    source: usize,
    /// Target column / key name.
    target: String,
}

static IMPORT_CANCEL: AtomicBool = AtomicBool::new(false);

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ImportProgress {
    done: usize,
    total: usize,
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
async fn import_run(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    id: String,
    file: FileOptions,
    target: Vec<String>,
    mappings: Vec<ColumnMapping>,
    options: ImportOptions,
    values: ValueOptions,
) -> CmdResult<ImportReport> {
    if state.config(&id).await?.read_only {
        return Err("This connection is read-only.".into());
    }
    if mappings.is_empty() {
        return Err("Map at least one column.".into());
    }
    let mut seen = std::collections::HashSet::new();
    for m in &mappings {
        if m.target.trim().is_empty() {
            return Err("Every selected column needs a target.".into());
        }
        if !seen.insert(m.target.trim().to_lowercase()) {
            return Err(format!("\"{}\" is the target of more than one column.", m.target));
        }
    }
    let columns: Vec<String> = mappings.iter().map(|m| m.target.trim().to_string()).collect();
    for k in &options.key_columns {
        if !columns.contains(k) {
            return Err(format!("Key column \"{k}\" is not mapped."));
        }
    }
    let table = read_file(file).await?;
    if let Some(m) = mappings.iter().find(|m| m.source >= table.columns.len()) {
        return Err(format!("Column {} is not in the file any more – reload the preview.", m.source + 1));
    }
    let rows: Vec<dbcore::ImportRow> = table
        .rows
        .iter()
        .enumerate()
        .map(|(i, r)| (table.first_row + i, mappings.iter().map(|m| tabular::prepare(&r[m.source], values)).collect()))
        .collect();
    let total = rows.len();
    let driver = state.driver(&id).await?;
    IMPORT_CANCEL.store(false, Ordering::Relaxed);
    let progress = move |done: usize| {
        let _ = app.emit("import-progress", ImportProgress { done, total });
    };
    let ctx = ImportCtx { progress: &progress, cancel: &IMPORT_CANCEL };
    driver.import_rows(&target, &columns, rows, &options, &ctx).await.map_err(err)
}

#[tauri::command]
fn import_cancel() {
    IMPORT_CANCEL.store(true, Ordering::Relaxed);
}

#[tauri::command]
async fn schema_doc(state: State<'_, AppState>, id: String, database: Option<String>, schema: Option<String>) -> CmdResult<dbcore::SchemaDoc> {
    let driver = state.driver(&id).await?;
    driver.schema_doc(database.as_deref().filter(|d| !d.is_empty()), schema.as_deref().filter(|s| !s.is_empty())).await.map_err(err)
}

static DUMP_CANCEL: AtomicBool = AtomicBool::new(false);

#[tauri::command]
async fn dump_tools(state: State<'_, AppState>, id: String, tool_dir: String) -> CmdResult<Vec<dump::ToolStatus>> {
    let cfg = state.config(&id).await?;
    Ok(dump::tools(cfg.engine, &tool_dir).await)
}

#[tauri::command]
async fn dump_run(app: tauri::AppHandle, state: State<'_, AppState>, id: String, options: dump::DumpOptions) -> CmdResult<dump::DumpReport> {
    let cfg = state.config(&id).await?;
    let driver = state.driver(&id).await?;
    DUMP_CANCEL.store(false, Ordering::Relaxed);
    let log = move |line: &str| {
        let _ = app.emit("dump-log", line);
    };
    let ctx = dump::JobCtx { log: &log, cancel: &DUMP_CANCEL };
    dump::dump(&cfg, driver.as_ref(), &options, &ctx).await.map_err(err)
}

#[tauri::command]
async fn restore_run(app: tauri::AppHandle, state: State<'_, AppState>, id: String, options: dump::RestoreOptions) -> CmdResult<dump::RestoreReport> {
    let cfg = state.config(&id).await?;
    if cfg.read_only {
        return Err("This connection is read-only.".into());
    }
    let driver = state.driver(&id).await?;
    DUMP_CANCEL.store(false, Ordering::Relaxed);
    let log = move |line: &str| {
        let _ = app.emit("dump-log", line);
    };
    let ctx = dump::JobCtx { log: &log, cancel: &DUMP_CANCEL };
    dump::restore(&cfg, driver.as_ref(), &options, &ctx).await.map_err(err)
}

#[tauri::command]
fn dump_cancel() {
    DUMP_CANCEL.store(true, Ordering::Relaxed);
}

#[tauri::command]
async fn write_text_file(path: String, contents: String) -> CmdResult<()> {
    std::fs::write(path, contents).map_err(|e| e.to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    dbcore::init();
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let dir = app.path().app_config_dir()?;
            dbcore::auth::set_cache_path(dir.join("entra-tokens.json"));
            let handle = app.handle().clone();
            dbcore::auth::set_login_ui(move |event| {
                let _ = handle.emit("entra-login", event);
            });
            app.manage(AppState::load(dir.join("connections.json")));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_connections,
            save_connection,
            delete_connection,
            test_connection,
            connect,
            disconnect,
            list_children,
            execute,
            object_query,
            describe,
            search_children,
            edit_info,
            apply_changes,
            write_xlsx,
            import_preview,
            import_run,
            import_cancel,
            dump_tools,
            dump_run,
            restore_run,
            dump_cancel,
            schema_doc,
            entra_cancel,
            entra_sign_out,
            open_url,
            write_text_file,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Nexus DB");
}
