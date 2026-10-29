use super::sqlcommon::{self, SqlExec, SqlKind};
use crate::auth;
use crate::model::*;
use crate::util::{hex, quote_ident, sql_lit};
use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use futures::TryStreamExt;
use serde_json::Value;
use tiberius::{AuthMethod, Client, ColumnData, Config, EncryptionLevel, FromSql, QueryItem};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

/// SQL Server, Azure SQL, Sybase-compatible TDS servers.
pub struct MssqlDriver {
    client: Mutex<Client<Compat<TcpStream>>>,
    default_db: Option<String>,
}

async fn tcp(addr: String) -> Result<TcpStream> {
    let stream = tokio::time::timeout(std::time::Duration::from_secs(15), TcpStream::connect(&addr))
        .await
        .with_context(|| format!("timed out connecting to {addr}"))??;
    stream.set_nodelay(true)?;
    Ok(stream)
}

/// Runs statements as plain batches – sp_executesql would refuse a
/// BEGIN TRANSACTION that outlives it – and reads the count via @@ROWCOUNT.
struct MsExec<'a>(&'a mut Client<Compat<TcpStream>>);

#[async_trait]
impl SqlExec for MsExec<'_> {
    async fn exec(&mut self, sql: &str) -> Result<u64> {
        let batch = format!("{};\nSELECT CAST(@@ROWCOUNT AS bigint)", sql.trim_end().trim_end_matches(';'));
        let results = self.0.simple_query(batch).await?.into_results().await?;
        Ok(results.last().and_then(|r| r.first()).and_then(|row| row.get::<i64, _>(0)).unwrap_or(0) as u64)
    }
}

impl MssqlDriver {
    fn target(path: &[String]) -> Result<String> {
        let [db, schema, table, ..] = path else { bail!("Select a table") };
        Ok(format!("{}.{}.{}", quote_ident(db, '['), quote_ident(schema, '['), quote_ident(table, '[')))
    }

    pub async fn connect(cfg: &ConnectionConfig) -> Result<Self> {
        let mut config = if cfg.has_url() {
            Config::from_ado_string(cfg.url.trim())?
        } else {
            let mut c = Config::new();
            c.host(cfg.host_or_default());
            c.port(cfg.port_or(1433));
            c.authentication(AuthMethod::sql_server(&cfg.user, &cfg.password));
            if !cfg.database.is_empty() {
                c.database(&cfg.database);
            }
            // Entra tokens must never travel unencrypted
            c.encryption(if cfg.ssl || cfg.auth.is_entra() { EncryptionLevel::Required } else { EncryptionLevel::Off });
            c
        };
        if let Some(src) = auth::token_source(cfg)? {
            // also replaces any User ID / Password from a connection string
            config.authentication(AuthMethod::aad_token(src.token().await?.secret));
        }
        if cfg.trust_cert {
            config.trust_cert();
        }
        config.application_name("Nexus DB");

        let client = match Client::connect(config.clone(), tcp(config.get_addr()).await?.compat_write()).await {
            Ok(c) => c,
            // Azure SQL gateways redirect to the actual node
            Err(tiberius::error::Error::Routing { host, port }) => {
                config.host(&host);
                config.port(port);
                Client::connect(config.clone(), tcp(format!("{host}:{port}")).await?.compat_write()).await?
            }
            Err(e) => return Err(e.into()),
        };
        let default_db = Some(cfg.database.clone()).filter(|d| !d.is_empty());
        Ok(Self { client: Mutex::new(client), default_db })
    }

    async fn run(&self, query: &str, ctx: &ExecContext) -> Result<Vec<ResultSet>> {
        let mut client = self.client.lock().await;
        if let Some(db) = ctx.database().map(str::to_string).or_else(|| self.default_db.clone()) {
            client.simple_query(format!("USE {}", quote_ident(&db, '['))).await?.into_results().await?;
        }
        let limit = ctx.limit();
        let mut sets: Vec<ResultSet> = Vec::new();
        let mut stream = client.simple_query(query).await?;
        while let Some(item) = stream.try_next().await? {
            match item {
                QueryItem::Metadata(meta) => sets.push(ResultSet::table(
                    meta.columns()
                        .iter()
                        .map(|c| Column::new(c.name(), format!("{:?}", c.column_type()).to_lowercase()))
                        .collect(),
                    Vec::new(),
                )),
                QueryItem::Row(row) => {
                    if let Some(set) = sets.last_mut() {
                        if set.rows.len() >= limit {
                            // keep draining so the connection stays usable
                            set.truncated = true;
                            continue;
                        }
                        set.rows.push(row.into_iter().map(cell).collect());
                    }
                }
            }
        }
        if sets.is_empty() {
            sets.push(ResultSet::message("Statement executed successfully"));
        }
        Ok(sets)
    }

    async fn catalog(&self, sql: &str) -> Result<Vec<Vec<String>>> {
        let ctx = ExecContext { limit: 100_000, ..Default::default() };
        Ok(super::sqlcommon::strings(&self.run(sql, &ctx).await?))
    }

    fn columns_sql(db: &str, schema: &str, table: &str) -> String {
        let db = quote_ident(db, '[');
        format!(
            "SELECT c.COLUMN_NAME AS [column], \
               c.DATA_TYPE + COALESCE('(' + CASE WHEN c.CHARACTER_MAXIMUM_LENGTH = -1 THEN 'max' \
                 ELSE CAST(c.CHARACTER_MAXIMUM_LENGTH AS varchar(10)) END + ')', '') AS [type], \
               c.IS_NULLABLE AS nullable, c.COLUMN_DEFAULT AS [default], \
               CASE WHEN k.COLUMN_NAME IS NULL THEN 'NO' ELSE 'YES' END AS primary_key \
             FROM {db}.INFORMATION_SCHEMA.COLUMNS c \
             LEFT JOIN ({db}.INFORMATION_SCHEMA.KEY_COLUMN_USAGE k \
               JOIN {db}.INFORMATION_SCHEMA.TABLE_CONSTRAINTS t ON t.CONSTRAINT_NAME = k.CONSTRAINT_NAME \
                 AND t.CONSTRAINT_SCHEMA = k.CONSTRAINT_SCHEMA AND t.CONSTRAINT_TYPE = 'PRIMARY KEY') \
               ON k.TABLE_SCHEMA = c.TABLE_SCHEMA AND k.TABLE_NAME = c.TABLE_NAME AND k.COLUMN_NAME = c.COLUMN_NAME \
             WHERE c.TABLE_SCHEMA = {} AND c.TABLE_NAME = {} ORDER BY c.ORDINAL_POSITION",
            sql_lit(schema),
            sql_lit(table)
        )
    }
}

fn num(f: f64) -> Value {
    serde_json::Number::from_f64(f).map(Value::Number).unwrap_or(Value::String(f.to_string()))
}

fn cell(d: ColumnData<'static>) -> Value {
    let date = match &d {
        ColumnData::DateTime(_) | ColumnData::SmallDateTime(_) | ColumnData::DateTime2(_) => {
            Some(chrono::NaiveDateTime::from_sql(&d).ok().flatten().map(|t| t.to_string()))
        }
        ColumnData::Date(_) => Some(chrono::NaiveDate::from_sql(&d).ok().flatten().map(|t| t.to_string())),
        ColumnData::Time(_) => Some(chrono::NaiveTime::from_sql(&d).ok().flatten().map(|t| t.to_string())),
        ColumnData::DateTimeOffset(_) => Some(
            chrono::DateTime::<chrono::FixedOffset>::from_sql(&d).ok().flatten().map(|t| t.to_rfc3339()),
        ),
        _ => None,
    };
    if let Some(date) = date {
        return date.map(Value::String).unwrap_or(Value::Null);
    }
    let v = match d {
        ColumnData::U8(v) => v.map(Value::from),
        ColumnData::I16(v) => v.map(Value::from),
        ColumnData::I32(v) => v.map(Value::from),
        ColumnData::I64(v) => v.map(Value::from),
        ColumnData::F32(v) => v.map(|f| num(f as f64)),
        ColumnData::F64(v) => v.map(num),
        ColumnData::Bit(v) => v.map(Value::Bool),
        ColumnData::String(v) => v.map(|s| Value::String(s.into_owned())),
        ColumnData::Guid(v) => v.map(|g| Value::String(g.to_string().to_uppercase())),
        ColumnData::Binary(v) => v.map(|b| Value::String(hex(&b))),
        ColumnData::Numeric(v) => v.map(|n| Value::String(n.to_string())),
        ColumnData::Xml(v) => v.map(|x| Value::String(x.to_string())),
        _ => None,
    };
    v.unwrap_or(Value::Null)
}

#[async_trait]
impl Driver for MssqlDriver {
    async fn server_info(&self) -> Result<ServerInfo> {
        let sets = self
            .run(
                "SELECT CAST(SERVERPROPERTY('ProductVersion') AS nvarchar(64)) + ' ' + \
                 CAST(SERVERPROPERTY('Edition') AS nvarchar(128))",
                &ExecContext::default(),
            )
            .await?;
        Ok(ServerInfo { version: format!("SQL Server {}", super::sqlcommon::first_value(&sets)) })
    }

    async fn list_children(&self, path: &[String]) -> Result<Vec<TreeNode>> {
        Ok(match path {
            [] => self
                .catalog("SELECT name FROM sys.databases WHERE state = 0 ORDER BY CASE WHEN database_id <= 4 THEN 1 ELSE 0 END, name")
                .await?
                .into_iter()
                .map(|r| TreeNode::new(path, &r[0], "database", true))
                .collect(),
            [db] => self
                .catalog(&format!(
                    "SELECT DISTINCT TABLE_SCHEMA FROM {}.INFORMATION_SCHEMA.TABLES ORDER BY TABLE_SCHEMA",
                    quote_ident(db, '[')
                ))
                .await?
                .into_iter()
                .map(|r| TreeNode::new(path, &r[0], "schema", true))
                .collect(),
            [db, schema] => self
                .catalog(&format!(
                    "SELECT TABLE_NAME, TABLE_TYPE FROM {}.INFORMATION_SCHEMA.TABLES WHERE TABLE_SCHEMA = {} ORDER BY TABLE_NAME",
                    quote_ident(db, '['),
                    sql_lit(schema)
                ))
                .await?
                .into_iter()
                .map(|r| TreeNode::new(path, &r[0], if r[1] == "VIEW" { "view" } else { "table" }, true))
                .collect(),
            [db, schema, table] => self
                .catalog(&Self::columns_sql(db, schema, table))
                .await?
                .into_iter()
                .map(|r| TreeNode::new(path, &r[0], if r[4] == "YES" { "pk" } else { "column" }, false).detail(&r[1]))
                .collect(),
            _ => Vec::new(),
        })
    }

    async fn execute(&self, query: &str, ctx: &ExecContext) -> Result<Vec<ResultSet>> {
        self.run(query, ctx).await
    }

    async fn object_query(&self, path: &[String]) -> Result<ObjectQuery> {
        let [db, schema, table, ..] = path else { bail!("Select a table or view") };
        Ok(ObjectQuery {
            query: format!("SELECT TOP 100 *\nFROM {}.{};", quote_ident(schema, '['), quote_ident(table, '[')),
            database: Some(db.clone()),
            schema: Some(schema.clone()),
        })
    }

    async fn edit_info(&self, path: &[String]) -> Result<EditInfo> {
        Ok(sqlcommon::edit_info_from_describe(&self.describe(path).await?))
    }

    async fn apply_changes(&self, path: &[String], changes: &[RowChange], dry_run: bool) -> Result<ApplyReport> {
        let target = Self::target(path)?;
        let mut client = self.client.lock().await;
        let res = sqlcommon::apply_changes(SqlKind::Mssql, &mut MsExec(&mut client), &target, changes, dry_run).await;
        if res.is_err() {
            let _ = client.simple_query("IF @@TRANCOUNT > 0 ROLLBACK TRANSACTION").await;
        }
        res
    }

    async fn import_rows(
        &self,
        path: &[String],
        columns: &[String],
        rows: Vec<ImportRow>,
        opts: &ImportOptions,
        ctx: &ImportCtx<'_>,
    ) -> Result<ImportReport> {
        let target = Self::target(path)?;
        let mut client = self.client.lock().await;
        let res = sqlcommon::import_rows(SqlKind::Mssql, &mut MsExec(&mut client), &target, columns, rows, opts, ctx).await;
        if res.is_err() {
            let _ = client.simple_query("IF @@TRANCOUNT > 0 ROLLBACK TRANSACTION").await;
        }
        res
    }

    async fn schema_doc(&self, database: Option<&str>, schema: Option<&str>) -> Result<SchemaDoc> {
        let ctx = ExecContext { database: database.map(str::to_string), limit: 100_000, ..Default::default() };
        let q = |sql: String| {
            let ctx = ctx.clone();
            async move { Ok::<_, anyhow::Error>(super::sqlcommon::strings(&self.run(&sql, &ctx).await?)) }
        };
        let db = super::sqlcommon::first_value(&self.run("SELECT DB_NAME()", &ctx).await?);
        let f = schema.map(|s| format!(" AND s.name = {}", crate::util::sql_lit(s))).unwrap_or_default();
        let user = "o.type IN ('U','V') AND o.is_ms_shipped = 0";
        let tables = format!(
            "SELECT s.name, o.name, CASE o.type WHEN 'V' THEN 'view' ELSE 'table' END, CAST(ep.value AS nvarchar(max)), \
             (SELECT SUM(p.rows) FROM sys.partitions p WHERE p.object_id = o.object_id AND p.index_id IN (0, 1) AND o.type = 'U') \
             FROM sys.objects o JOIN sys.schemas s ON s.schema_id = o.schema_id \
             LEFT JOIN sys.extended_properties ep ON ep.class = 1 AND ep.major_id = o.object_id AND ep.minor_id = 0 \
               AND ep.name = 'MS_Description' \
             WHERE {user}{f} ORDER BY 1, 2"
        );
        let columns = format!(
            "SELECT s.name, o.name, c.column_id, c.name, t.name + CASE \
               WHEN t.name IN ('varchar', 'char', 'varbinary', 'binary') \
                 THEN '(' + CASE WHEN c.max_length = -1 THEN 'max' ELSE CAST(c.max_length AS varchar(10)) END + ')' \
               WHEN t.name IN ('nvarchar', 'nchar') \
                 THEN '(' + CASE WHEN c.max_length = -1 THEN 'max' ELSE CAST(c.max_length / 2 AS varchar(10)) END + ')' \
               WHEN t.name IN ('decimal', 'numeric') \
                 THEN '(' + CAST(c.precision AS varchar(10)) + ',' + CAST(c.scale AS varchar(10)) + ')' \
               ELSE '' END + CASE WHEN c.is_identity = 1 THEN ' identity' ELSE '' END, \
             c.is_nullable, dc.definition, CAST(ep.value AS nvarchar(max)) \
             FROM sys.columns c JOIN sys.objects o ON o.object_id = c.object_id JOIN sys.schemas s ON s.schema_id = o.schema_id \
             JOIN sys.types t ON t.user_type_id = c.user_type_id \
             LEFT JOIN sys.default_constraints dc ON dc.object_id = c.default_object_id \
             LEFT JOIN sys.extended_properties ep ON ep.class = 1 AND ep.major_id = c.object_id AND ep.minor_id = c.column_id \
               AND ep.name = 'MS_Description' \
             WHERE {user}{f} ORDER BY 1, 2, 3"
        );
        let constraints = format!(
            "SELECT s.name, t.name, kc.name, CASE kc.type WHEN 'PK' THEN 'p' ELSE 'u' END, c.name, ic.key_ordinal, \
               NULL, NULL, NULL, NULL, NULL \
             FROM sys.key_constraints kc JOIN sys.tables t ON t.object_id = kc.parent_object_id \
             JOIN sys.schemas s ON s.schema_id = t.schema_id \
             JOIN sys.index_columns ic ON ic.object_id = kc.parent_object_id AND ic.index_id = kc.unique_index_id \
             JOIN sys.columns c ON c.object_id = ic.object_id AND c.column_id = ic.column_id \
             WHERE t.is_ms_shipped = 0{f} \
             UNION ALL \
             SELECT s.name, t.name, fk.name, 'f', c.name, fkc.constraint_column_id, rs.name, rt.name, rc.name, \
               fk.update_referential_action_desc, fk.delete_referential_action_desc \
             FROM sys.foreign_keys fk JOIN sys.tables t ON t.object_id = fk.parent_object_id \
             JOIN sys.schemas s ON s.schema_id = t.schema_id \
             JOIN sys.foreign_key_columns fkc ON fkc.constraint_object_id = fk.object_id \
             JOIN sys.columns c ON c.object_id = fkc.parent_object_id AND c.column_id = fkc.parent_column_id \
             JOIN sys.tables rt ON rt.object_id = fk.referenced_object_id JOIN sys.schemas rs ON rs.schema_id = rt.schema_id \
             JOIN sys.columns rc ON rc.object_id = fkc.referenced_object_id AND rc.column_id = fkc.referenced_column_id \
             WHERE t.is_ms_shipped = 0{f} \
             ORDER BY 1, 2, 3, 6"
        );
        let indexes = format!(
            "SELECT s.name, o.name, i.name, i.is_unique, i.is_primary_key, c.name, ic.key_ordinal \
             FROM sys.indexes i JOIN sys.objects o ON o.object_id = i.object_id JOIN sys.schemas s ON s.schema_id = o.schema_id \
             JOIN sys.index_columns ic ON ic.object_id = i.object_id AND ic.index_id = i.index_id \
             JOIN sys.columns c ON c.object_id = ic.object_id AND c.column_id = ic.column_id \
             WHERE {user} AND i.name IS NOT NULL AND ic.is_included_column = 0{f} ORDER BY 1, 2, 3, 7"
        );
        let cat = crate::schemadoc::Catalog {
            tables: q(tables).await?,
            columns: q(columns).await?,
            constraints: q(constraints).await?,
            indexes: q(indexes).await?,
        };
        Ok(crate::schemadoc::build(&db, cat))
    }

    async fn describe(&self, path: &[String]) -> Result<ResultSet> {
        let [db, schema, table, ..] = path else { bail!("Select a table or view") };
        let ctx = ExecContext { limit: 10_000, ..Default::default() };
        Ok(self.run(&Self::columns_sql(db, schema, table), &ctx).await?.pop().unwrap_or_default())
    }
}
