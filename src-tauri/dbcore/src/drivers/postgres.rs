use super::sqlcommon::{self, first_value, run_raw, strings, SqlKind};
use crate::auth::{self, Token, TokenSource};
use crate::model::*;
use crate::util::{parse_number, quote_ident, sql_lit};
use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use serde_json::Value;
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions, PgRow, PgSslMode};
use sqlx::{Row, TypeInfo, ValueRef};
use std::{collections::HashMap, str::FromStr, sync::Arc, time::Duration};
use tokio::sync::Mutex;

pub struct PgDriver {
    cfg: ConnectionConfig,
    default_db: String,
    pools: Mutex<HashMap<String, PgPool>>,
    entra: Option<Arc<TokenSource>>,
    /// Token the pools currently open new connections with.
    pool_token: Mutex<String>,
}

impl PgDriver {
    pub async fn connect(cfg: &ConnectionConfig) -> Result<Self> {
        let entra = auth::token_source(cfg)?;
        let base = Self::options(cfg, None, None)?;
        let default_db = if !cfg.database.trim().is_empty() {
            cfg.database.trim().to_string()
        } else {
            base.get_database().unwrap_or("postgres").to_string()
        };
        let driver = Self { cfg: cfg.clone(), default_db, pools: Mutex::default(), entra, pool_token: Mutex::default() };
        driver.pool(None).await?;
        Ok(driver)
    }

    /// With an Entra token it becomes the password and – unless one is
    /// configured – the signed-in user becomes the role name.
    fn options(cfg: &ConnectionConfig, src: Option<&TokenSource>, token: Option<&Token>) -> Result<PgConnectOptions> {
        let mut o = Self::base_options(cfg)?;
        if let (Some(src), Some(token)) = (src, token) {
            let configured = if cfg.has_url() { o.get_username().to_string() } else { cfg.user.clone() };
            o = o.username(&src.db_user(&configured, token)?).password(&token.secret);
            if !cfg.ssl && !cfg.has_url() {
                o = o.ssl_mode(PgSslMode::Require);
            }
        }
        Ok(o)
    }

    fn base_options(cfg: &ConnectionConfig) -> Result<PgConnectOptions> {
        if cfg.has_url() {
            return Ok(PgConnectOptions::from_str(cfg.url.trim())?.application_name("Nexus DB"));
        }
        let mut o = PgConnectOptions::new()
            .host(cfg.host_or_default())
            .port(cfg.port_or(5432))
            .application_name("Nexus DB")
            .ssl_mode(match (cfg.ssl, cfg.trust_cert) {
                (false, _) => PgSslMode::Prefer,
                (true, true) => PgSslMode::Require,
                (true, false) => PgSslMode::VerifyFull,
            });
        if !cfg.user.is_empty() {
            o = o.username(&cfg.user);
        }
        if !cfg.password.is_empty() {
            o = o.password(&cfg.password);
        }
        Ok(o)
    }

    async fn pool(&self, db: Option<&str>) -> Result<PgPool> {
        let db = db.filter(|d| !d.is_empty()).unwrap_or(&self.default_db).to_string();
        let token = match &self.entra {
            Some(src) => Some(src.token().await?),
            None => None,
        };
        let options = |db: &str| Self::options(&self.cfg, self.entra.as_deref(), token.as_ref()).map(|o| o.database(db));
        let mut pools = self.pools.lock().await;
        if let Some(t) = &token {
            // Entra tokens expire after ~1 h: open new connections with the current one
            // (established sessions stay authenticated)
            let mut current = self.pool_token.lock().await;
            if *current != t.secret {
                for (name, pool) in pools.iter() {
                    pool.set_connect_options(options(name)?);
                }
                *current = t.secret.clone();
            }
        }
        if let Some(p) = pools.get(&db) {
            return Ok(p.clone());
        }
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .acquire_timeout(Duration::from_secs(15))
            .connect_with(options(&db)?)
            .await
            .with_context(|| format!("connecting to database \"{db}\""))?;
        pools.insert(db, pool.clone());
        Ok(pool)
    }

    /// Quoted `"schema"."table"` and a connection to its database.
    async fn target(&self, path: &[String]) -> Result<(String, sqlx::pool::PoolConnection<sqlx::Postgres>)> {
        let [db, schema, table, ..] = path else { bail!("Select a table") };
        let target = format!("{}.{}", quote_ident(schema, '"'), quote_ident(table, '"'));
        Ok((target, self.pool(Some(db)).await?.acquire().await?))
    }

    async fn run(&self, query: &str, ctx: &ExecContext) -> Result<Vec<ResultSet>> {
        let pool = self.pool(ctx.database()).await?;
        let mut conn = pool.acquire().await?;
        let search_path = match ctx.schema() {
            Some(s) => format!("SET search_path TO {}, public", quote_ident(s, '"')),
            None => "RESET search_path".to_string(),
        };
        run_raw::<sqlx::Postgres>(&mut *conn, &search_path, 1, cell, |r| r.rows_affected()).await?;
        let (sets, truncated) =
            run_raw::<sqlx::Postgres>(&mut *conn, query, ctx.limit(), cell, |r| r.rows_affected()).await?;
        if truncated {
            // the connection still has unread rows – close it instead of returning it to the pool
            drop(conn.detach());
        }
        Ok(sets)
    }

    async fn catalog(&self, db: Option<&str>, sql: &str) -> Result<Vec<Vec<String>>> {
        let ctx = ExecContext { database: db.map(str::to_string), schema: None, limit: 100_000 };
        Ok(strings(&self.run(sql, &ctx).await?))
    }

    fn columns_sql(schema: &str, table: &str) -> String {
        let rel = format!("{}.{}", quote_ident(schema, '"'), quote_ident(table, '"'));
        format!(
            "SELECT a.attname AS column, format_type(a.atttypid, a.atttypmod) AS type, \
             NOT a.attnotnull AS nullable, pg_get_expr(d.adbin, d.adrelid) AS default, \
             COALESCE((SELECT true FROM pg_index i WHERE i.indrelid = a.attrelid AND i.indisprimary \
               AND a.attnum = ANY(i.indkey)), false) AS primary_key, \
             col_description(a.attrelid, a.attnum) AS comment \
             FROM pg_attribute a LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
             WHERE a.attrelid = {}::regclass AND a.attnum > 0 AND NOT a.attisdropped ORDER BY a.attnum",
            sql_lit(&rel)
        )
    }
}

fn cell(row: &PgRow, i: usize) -> Value {
    let Ok(raw) = row.try_get_raw(i) else { return Value::Null };
    if raw.is_null() {
        return Value::Null;
    }
    let ty = raw.type_info().name().to_string();
    // raw_sql uses the simple-query protocol, so every value arrives as text.
    let Ok(s) = row.try_get_unchecked::<String, _>(i) else {
        return Value::String(format!("<{ty}>"));
    };
    match ty.as_str() {
        "INT2" | "INT4" | "INT8" | "OID" | "FLOAT4" | "FLOAT8" => parse_number(&s),
        "BOOL" => Value::Bool(s == "t" || s == "true"),
        "JSON" | "JSONB" => serde_json::from_str(&s).unwrap_or(Value::String(s)),
        _ => Value::String(s),
    }
}

#[async_trait]
impl Driver for PgDriver {
    async fn server_info(&self) -> Result<ServerInfo> {
        let v = first_value(&self.run("SELECT version()", &ExecContext::default()).await?);
        let short = v.split(" on ").next().unwrap_or(&v).to_string();
        Ok(ServerInfo { version: short })
    }

    async fn list_children(&self, path: &[String]) -> Result<Vec<TreeNode>> {
        Ok(match path {
            [] => self
                .catalog(None, "SELECT datname FROM pg_database WHERE datallowconn AND NOT datistemplate ORDER BY datname")
                .await?
                .into_iter()
                .map(|r| TreeNode::new(path, &r[0], "database", true))
                .collect(),
            [db] => self
                .catalog(
                    Some(db),
                    "SELECT nspname FROM pg_namespace WHERE nspname NOT LIKE 'pg\\_%' \
                     AND nspname <> 'information_schema' ORDER BY nspname = 'public' DESC, nspname",
                )
                .await?
                .into_iter()
                .map(|r| TreeNode::new(path, &r[0], "schema", true))
                .collect(),
            [db, schema] => self
                .catalog(
                    Some(db),
                    &format!(
                        "SELECT c.relname, CASE WHEN c.relkind IN ('v','m') THEN 'view' ELSE 'table' END, \
                         c.reltuples::bigint FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
                         WHERE n.nspname = {} AND c.relkind IN ('r','p','v','m','f') ORDER BY c.relname",
                        sql_lit(schema)
                    ),
                )
                .await?
                .into_iter()
                .map(|r| {
                    let rows: i64 = r[2].parse().unwrap_or(-1);
                    let detail = if r[1] == "table" && rows >= 0 { format!("~{rows} rows") } else { String::new() };
                    TreeNode::new(path, &r[0], &r[1], true).detail(detail)
                })
                .collect(),
            [db, schema, table] => self
                .catalog(Some(db), &Self::columns_sql(schema, table))
                .await?
                .into_iter()
                .map(|r| {
                    let kind = if r[4] == "true" { "pk" } else { "column" };
                    TreeNode::new(path, &r[0], kind, false).detail(&r[1])
                })
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
            query: format!("SELECT *\nFROM {}.{}\nLIMIT 100;", quote_ident(schema, '"'), quote_ident(table, '"')),
            database: Some(db.clone()),
            schema: Some(schema.clone()),
        })
    }

    async fn edit_info(&self, path: &[String]) -> Result<EditInfo> {
        Ok(sqlcommon::edit_info_from_describe(&self.describe(path).await?))
    }

    async fn apply_changes(&self, path: &[String], changes: &[RowChange], dry_run: bool) -> Result<ApplyReport> {
        let (target, mut conn) = self.target(path).await?;
        let mut ex = sqlcommon::SqlxConn(&mut *conn);
        let res = sqlcommon::apply_changes(SqlKind::Postgres, &mut ex, &target, changes, dry_run).await;
        if res.is_err() {
            // never hand a connection with an open transaction back to the pool
            drop(conn.detach());
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
        let (target, mut conn) = self.target(path).await?;
        let mut ex = sqlcommon::SqlxConn(&mut *conn);
        let res = sqlcommon::import_rows(SqlKind::Postgres, &mut ex, &target, columns, rows, opts, ctx).await;
        if res.is_err() {
            drop(conn.detach());
        }
        res
    }

    async fn describe(&self, path: &[String]) -> Result<ResultSet> {
        let [db, schema, table, ..] = path else { bail!("Select a table or view") };
        let ctx = ExecContext { database: Some(db.clone()), schema: None, limit: 10_000 };
        let mut sets = self.run(&Self::columns_sql(schema, table), &ctx).await?;
        Ok(sets.pop().unwrap_or_default())
    }

    async fn schema_doc(&self, database: Option<&str>, schema: Option<&str>) -> Result<SchemaDoc> {
        let db = database.filter(|d| !d.is_empty()).unwrap_or(&self.default_db).to_string();
        let f = match schema {
            Some(s) => format!("n.nspname = {}", sql_lit(s)),
            None => "n.nspname NOT LIKE 'pg\\_%' AND n.nspname <> 'information_schema'".to_string(),
        };
        let rel = "c.relkind IN ('r','p','v','m','f') AND NOT c.relispartition";
        let tables = format!(
            "SELECT n.nspname, c.relname, CASE c.relkind WHEN 'v' THEN 'view' WHEN 'm' THEN 'materialized view' \
             WHEN 'f' THEN 'foreign table' ELSE 'table' END, obj_description(c.oid, 'pg_class'), \
             CASE WHEN c.relkind IN ('r','p') THEN c.reltuples::bigint END \
             FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE {rel} AND {f} ORDER BY 1, 2"
        );
        let columns = format!(
            "SELECT n.nspname, c.relname, a.attnum, a.attname, format_type(a.atttypid, a.atttypmod), NOT a.attnotnull, \
             pg_get_expr(d.adbin, d.adrelid), col_description(c.oid, a.attnum) \
             FROM pg_attribute a JOIN pg_class c ON c.oid = a.attrelid JOIN pg_namespace n ON n.oid = c.relnamespace \
             LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
             WHERE {rel} AND a.attnum > 0 AND NOT a.attisdropped AND {f} ORDER BY 1, 2, 3"
        );
        let action = |col: &str| {
            format!(
                "CASE {col} WHEN 'r' THEN 'RESTRICT' WHEN 'c' THEN 'CASCADE' WHEN 'n' THEN 'SET NULL' \
                 WHEN 'd' THEN 'SET DEFAULT' END"
            )
        };
        let constraints = format!(
            "SELECT n.nspname, c.relname, con.conname, con.contype, a.attname, k.ord, rn.nspname, rc.relname, ra.attname, \
             {}, {} FROM pg_constraint con \
             JOIN pg_class c ON c.oid = con.conrelid JOIN pg_namespace n ON n.oid = c.relnamespace \
             CROSS JOIN LATERAL unnest(con.conkey) WITH ORDINALITY AS k(attnum, ord) \
             JOIN pg_attribute a ON a.attrelid = con.conrelid AND a.attnum = k.attnum \
             LEFT JOIN pg_class rc ON rc.oid = con.confrelid LEFT JOIN pg_namespace rn ON rn.oid = rc.relnamespace \
             LEFT JOIN pg_attribute ra ON ra.attrelid = con.confrelid AND ra.attnum = con.confkey[k.ord] \
             WHERE con.contype IN ('p','u','f') AND {f} ORDER BY 1, 2, 3, 6",
            action("con.confupdtype"),
            action("con.confdeltype")
        );
        let indexes = format!(
            "SELECT n.nspname, c.relname, i.relname, ix.indisunique, ix.indisprimary, \
             pg_get_indexdef(ix.indexrelid, k.ord, true), k.ord \
             FROM pg_index ix JOIN pg_class i ON i.oid = ix.indexrelid JOIN pg_class c ON c.oid = ix.indrelid \
             JOIN pg_namespace n ON n.oid = c.relnamespace CROSS JOIN LATERAL generate_series(1, ix.indnatts) AS k(ord) \
             WHERE {f} ORDER BY 1, 2, 3, 7"
        );
        let cat = crate::schemadoc::Catalog {
            tables: self.catalog(Some(&db), &tables).await?,
            columns: self.catalog(Some(&db), &columns).await?,
            constraints: self.catalog(Some(&db), &constraints).await?,
            indexes: self.catalog(Some(&db), &indexes).await?,
        };
        Ok(crate::schemadoc::build(&db, cat))
    }

    async fn close(&self) {
        for (_, pool) in self.pools.lock().await.drain() {
            pool.close().await;
        }
    }
}
