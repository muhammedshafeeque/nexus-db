use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::sync::atomic::AtomicBool;

/// The wire-level driver used for a connection. UI "flavors" (CockroachDB,
/// MariaDB, TimescaleDB, ...) map onto one of these.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    #[default]
    Postgres,
    Mysql,
    Sqlite,
    Mssql,
    Mongodb,
    Arangodb,
    Redis,
    Clickhouse,
    Elasticsearch,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct ConnectionConfig {
    pub id: String,
    pub name: String,
    pub engine: Engine,
    /// Cosmetic variant shown in the UI (e.g. "cockroachdb", "mariadb").
    pub flavor: Option<String>,
    pub host: String,
    pub port: Option<u16>,
    pub user: String,
    pub password: String,
    pub database: String,
    /// SQLite database file.
    pub file_path: String,
    /// Raw connection string / base URL; overrides host/port/user when set.
    pub url: String,
    pub ssl: bool,
    pub trust_cert: bool,
    pub read_only: bool,
    pub color: String,
    pub group: String,
    /// How to authenticate; `Password` uses `user` / `password` as before.
    pub auth: AuthKind,
    pub entra: EntraOptions,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "camelCase")]
pub enum AuthKind {
    #[default]
    Password,
    /// Browser sign-in (authorization code + PKCE on a loopback redirect).
    EntraInteractive,
    /// Code shown in the app, entered on microsoft.com/devicelogin.
    EntraDeviceCode,
    /// App registration + client secret.
    EntraClientSecret,
    /// App registration + certificate (PEM with the private key).
    EntraCertificate,
    /// Azure VM / App Service / Container Apps / Arc identity.
    EntraManagedIdentity,
    /// Reuse the `az login` session.
    EntraAzureCli,
    /// A pasted access token (kept in `password`).
    EntraAccessToken,
}

impl AuthKind {
    pub fn is_entra(self) -> bool {
        self != AuthKind::Password
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct EntraOptions {
    /// "public" (default), "usgov" or "china".
    pub cloud: String,
    /// Tenant ID or domain; empty means "organizations" (any work/school account).
    pub tenant_id: String,
    /// App registration; empty uses the Azure CLI public client.
    pub client_id: String,
    pub client_secret: String,
    pub certificate_path: String,
    /// Token scope override, e.g. `api://<app-id>/.default`.
    pub scope: String,
    /// Client ID of a user-assigned managed identity.
    pub identity_client_id: String,
}

impl ConnectionConfig {
    pub fn host_or_default(&self) -> &str {
        if self.host.trim().is_empty() {
            "localhost"
        } else {
            self.host.trim()
        }
    }

    pub fn port_or(&self, default: u16) -> u16 {
        self.port.filter(|p| *p != 0).unwrap_or(default)
    }

    pub fn has_url(&self) -> bool {
        !self.url.trim().is_empty()
    }
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct TreeNode {
    pub name: String,
    /// database | schema | table | view | collection | edge | column | index | key | folder
    pub kind: String,
    pub path: Vec<String>,
    pub expandable: bool,
    pub detail: Option<String>,
}

impl TreeNode {
    pub fn new(parent: &[String], name: impl Into<String>, kind: &str, expandable: bool) -> Self {
        let name = name.into();
        let mut path = parent.to_vec();
        path.push(name.clone());
        Self { name, kind: kind.to_string(), path, expandable, detail: None }
    }

    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        let d = detail.into();
        self.detail = if d.is_empty() { None } else { Some(d) };
        self
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Column {
    pub name: String,
    pub type_name: String,
}

impl Column {
    pub fn new(name: impl Into<String>, type_name: impl Into<String>) -> Self {
        Self { name: name.into(), type_name: type_name.into() }
    }
}

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct ResultSet {
    pub columns: Vec<Column>,
    pub rows: Vec<Vec<Value>>,
    pub affected: Option<u64>,
    pub truncated: bool,
    pub message: Option<String>,
    /// true when rows came from documents (Mongo, Arango, ES) – UI offers a JSON view.
    pub documents: bool,
}

impl ResultSet {
    pub fn message(msg: impl Into<String>) -> Self {
        Self { message: Some(msg.into()), ..Default::default() }
    }

    pub fn table(columns: Vec<Column>, rows: Vec<Vec<Value>>) -> Self {
        Self { columns, rows, ..Default::default() }
    }
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct ExecContext {
    pub database: Option<String>,
    pub schema: Option<String>,
    pub limit: usize,
}

impl ExecContext {
    pub fn limit(&self) -> usize {
        if self.limit == 0 { 1000 } else { self.limit }
    }

    pub fn database(&self) -> Option<&str> {
        self.database.as_deref().filter(|d| !d.is_empty())
    }

    pub fn schema(&self) -> Option<&str> {
        self.schema.as_deref().filter(|d| !d.is_empty())
    }
}

/// Query the UI should open (and run) when the user opens a tree object.
#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct ObjectQuery {
    pub query: String,
    pub database: Option<String>,
    pub schema: Option<String>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    pub version: String,
}

/// What the grid editor and the importer need to know about a table / collection.
#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct EditColumn {
    pub name: String,
    pub type_name: String,
    pub nullable: bool,
    pub has_default: bool,
    pub key: bool,
}

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct EditInfo {
    pub columns: Vec<EditColumn>,
    /// Columns that identify a row (primary key, `_id`, `_key`).
    pub key_columns: Vec<String>,
    /// Rows can be edited in the grid.
    pub editable: bool,
    /// Why not, or a caveat worth showing.
    pub reason: Option<String>,
    /// Document store: any key may be written, dotted keys become nested objects.
    pub documents: bool,
    /// The importer can update existing rows / documents.
    pub can_upsert: bool,
    pub upsert_note: Option<String>,
    /// Imports can run in one all-or-nothing transaction.
    pub transactional: bool,
}

impl EditInfo {
    pub fn unsupported(reason: impl Into<String>) -> Self {
        Self { reason: Some(reason.into()), ..Default::default() }
    }
}

/// One pending grid change. Keys and values are JSON as shown in the grid.
#[derive(Deserialize, Clone, Debug)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum RowChange {
    Update { key: Map<String, Value>, values: Map<String, Value> },
    Insert { values: Map<String, Value> },
    Delete { key: Map<String, Value> },
}

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct ApplyReport {
    /// Statements / requests that were run (or would be, for a dry run).
    pub statements: Vec<String>,
    /// Number of changes applied, in order. With `atomic` it is all or nothing.
    pub applied: usize,
    pub atomic: bool,
    pub error: Option<String>,
}

#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum ImportMode {
    #[default]
    Insert,
    /// Update rows whose key columns match, insert the rest.
    Upsert,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct ImportOptions {
    pub mode: ImportMode,
    pub key_columns: Vec<String>,
    /// One transaction for the whole import, where the engine has transactions.
    pub atomic: bool,
    pub batch_size: usize,
    /// Redis: key name template such as `user:{id}`.
    pub key_template: String,
}

impl ImportOptions {
    pub fn batch(&self, max: usize) -> usize {
        self.batch_size.clamp(1, max)
    }
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RowError {
    /// 1-based row number in the source file.
    pub row: usize,
    pub message: String,
}

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    pub written: u64,
    pub failed: u64,
    /// The first errors (at most [`ImportReport::MAX_ERRORS`]).
    pub errors: Vec<RowError>,
    pub cancelled: bool,
    /// Nothing was written because the import ran in one transaction that was rolled back.
    pub rolled_back: bool,
}

impl ImportReport {
    pub const MAX_ERRORS: usize = 50;

    pub fn fail(&mut self, row: usize, message: impl Into<String>) {
        self.failed += 1;
        if self.errors.len() < Self::MAX_ERRORS {
            self.errors.push(RowError { row, message: message.into() });
        }
    }
}

/// A source row: its 1-based row number in the file and the mapped values.
pub type ImportRow = (usize, Vec<Value>);

pub struct ImportCtx<'a> {
    /// Called with the number of source rows processed so far.
    pub progress: &'a (dyn Fn(usize) + Send + Sync),
    pub cancel: &'a AtomicBool,
}

impl ImportCtx<'_> {
    pub fn cancelled(&self) -> bool {
        self.cancel.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// Database documentation: every table / view with its keys and relations.
#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct SchemaDoc {
    pub database: String,
    pub tables: Vec<TableDoc>,
}

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct TableDoc {
    pub schema: String,
    pub name: String,
    /// table | view | materialized view | foreign table
    pub kind: String,
    pub comment: Option<String>,
    /// Estimated rows, where the catalog keeps a count.
    pub rows: Option<i64>,
    pub columns: Vec<ColumnDoc>,
    pub primary_key: Vec<String>,
    pub unique_keys: Vec<KeyDoc>,
    pub foreign_keys: Vec<ForeignKeyDoc>,
    pub indexes: Vec<IndexDoc>,
}

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct ColumnDoc {
    pub name: String,
    pub type_name: String,
    pub nullable: bool,
    pub default: Option<String>,
    pub comment: Option<String>,
}

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct KeyDoc {
    pub name: String,
    pub columns: Vec<String>,
}

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct ForeignKeyDoc {
    pub name: String,
    pub columns: Vec<String>,
    pub ref_schema: String,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
    pub on_update: Option<String>,
    pub on_delete: Option<String>,
}

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct IndexDoc {
    pub name: String,
    pub columns: Vec<String>,
    pub unique: bool,
    pub primary: bool,
}

#[async_trait]
pub trait Driver: Send + Sync {
    async fn server_info(&self) -> Result<ServerInfo>;

    /// Lazily lists the children of a tree path. `[]` is the root.
    async fn list_children(&self, path: &[String]) -> Result<Vec<TreeNode>>;

    /// Runs a script (possibly several statements) and returns one result per statement.
    async fn execute(&self, query: &str, ctx: &ExecContext) -> Result<Vec<ResultSet>>;

    /// The query that "opening" a table / collection / key should run.
    async fn object_query(&self, path: &[String]) -> Result<ObjectQuery>;

    /// Structure of a table / collection (columns, inferred fields, ...).
    async fn describe(&self, path: &[String]) -> Result<ResultSet>;

    /// Children of `path` whose name contains `pattern`; engines whose listings
    /// are capped (Redis keys) search on the server instead.
    async fn search_children(&self, path: &[String], pattern: &str) -> Result<Vec<TreeNode>> {
        let p = pattern.to_lowercase();
        Ok(self.list_children(path).await?.into_iter().filter(|n| n.name.to_lowercase().contains(&p)).collect())
    }

    async fn edit_info(&self, _path: &[String]) -> Result<EditInfo> {
        Ok(EditInfo::unsupported("Editing rows is not supported for this database."))
    }

    async fn apply_changes(&self, _path: &[String], _changes: &[RowChange], _dry_run: bool) -> Result<ApplyReport> {
        anyhow::bail!("Editing rows is not supported for this database.")
    }

    /// Writes mapped rows into `path`; `columns` are the target column / key names.
    async fn import_rows(
        &self,
        _path: &[String],
        _columns: &[String],
        _rows: Vec<ImportRow>,
        _opts: &ImportOptions,
        _ctx: &ImportCtx<'_>,
    ) -> Result<ImportReport> {
        anyhow::bail!("Importing is not supported for this database.")
    }

    /// Tables, columns, keys and relations of a database (optionally one schema).
    async fn schema_doc(&self, _database: Option<&str>, _schema: Option<&str>) -> Result<SchemaDoc> {
        anyhow::bail!("Documentation is available for PostgreSQL, MySQL / MariaDB, SQL Server and SQLite.")
    }

    async fn close(&self) {}
}
