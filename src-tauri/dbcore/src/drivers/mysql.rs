use super::sqlcommon::{self, first_value, run_raw, strings, SqlKind};
use crate::auth::{self, TokenSource};
use crate::model::*;
use crate::util::{bytes_value, parse_number, quote_ident};
use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use serde_json::Value;
use sqlx::mysql::{MySqlConnectOptions, MySqlPool, MySqlPoolOptions, MySqlRow, MySqlSslMode};
use sqlx::{Row, TypeInfo, ValueRef};
use std::{collections::HashMap, str::FromStr, sync::Arc, time::Duration};
use tokio::sync::Mutex;

/// MySQL, MariaDB, TiDB, PlanetScale, SingleStore ...
pub struct MySqlDriver {
    cfg: ConnectionConfig,
    pools: Mutex<HashMap<String, MySqlPool>>,
    entra: Option<Arc<TokenSource>>,
    /// Token the pools currently open new connections with.
    pool_token: Mutex<String>,
}

fn lit(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "''"))
}

impl MySqlDriver {
    pub async fn connect(cfg: &ConnectionConfig) -> Result<Self> {
        let entra = auth::token_source(cfg)?;
        let driver = Self { cfg: cfg.clone(), pools: Mutex::default(), entra, pool_token: Mutex::default() };
        driver.pool(None).await?;
        Ok(driver)
    }

    /// Azure Database for MySQL takes the Entra token as a cleartext password,
    /// so TLS is enforced whenever one is used.
    fn options(&self, token: Option<&auth::Token>) -> Result<MySqlConnectOptions> {
        let mut o = self.base_options()?;
        if let (Some(src), Some(token)) = (&self.entra, token) {
            let configured = if self.cfg.has_url() { o.get_username().to_string() } else { self.cfg.user.clone() };
            o = o.username(&src.db_user(&configured, token)?).password(&token.secret).enable_cleartext_plugin(true);
            if !self.cfg.ssl && !self.cfg.has_url() {
                o = o.ssl_mode(MySqlSslMode::Required);
            }
        }
        Ok(o)
    }

    fn base_options(&self) -> Result<MySqlConnectOptions> {
        let cfg = &self.cfg;
        if cfg.has_url() {
            return Ok(MySqlConnectOptions::from_str(cfg.url.trim())?);
        }
        let mut o = MySqlConnectOptions::new()
            .host(cfg.host_or_default())
            .port(cfg.port_or(3306))
            .charset("utf8mb4")
            .ssl_mode(match (cfg.ssl, cfg.trust_cert) {
                (false, _) => MySqlSslMode::Preferred,
                (true, true) => MySqlSslMode::Required,
                (true, false) => MySqlSslMode::VerifyIdentity,
            });
        if !cfg.user.is_empty() {
            o = o.username(&cfg.user);
        }
        if !cfg.password.is_empty() {
            o = o.password(&cfg.password);
        }
        if !cfg.database.is_empty() {
            o = o.database(&cfg.database);
        }
        Ok(o)
    }

    async fn pool(&self, db: Option<&str>) -> Result<MySqlPool> {
        let key = db.unwrap_or("").to_string();
        let token = match &self.entra {
            Some(src) => Some(src.token().await?),
            None => None,
        };
        let options = |key: &str| {
            self.options(token.as_ref()).map(|o| if key.is_empty() { o } else { o.database(key) })
        };
        let mut pools = self.pools.lock().await;
        if let Some(t) = &token {
            // Entra tokens expire after ~1 h: open new connections with the current one
            let mut current = self.pool_token.lock().await;
            if *current != t.secret {
                for (name, pool) in pools.iter() {
                    pool.set_connect_options(options(name)?);
                }
                *current = t.secret.clone();
            }
        }
        if let Some(p) = pools.get(&key) {
            return Ok(p.clone());
        }
        let pool = MySqlPoolOptions::new()
            .max_connections(5)
            .acquire_timeout(Duration::from_secs(15))
            .connect_with(options(&key)?)
            .await
            .context("connecting to MySQL")?;
        pools.insert(key, pool.clone());
        Ok(pool)
    }

    async fn target(&self, path: &[String]) -> Result<(String, sqlx::pool::PoolConnection<sqlx::MySql>)> {
        let [db, table, ..] = path else { bail!("Select a table") };
        let target = format!("{}.{}", quote_ident(db, '`'), quote_ident(table, '`'));
        Ok((target, self.pool(None).await?.acquire().await?))
    }

    async fn run(&self, query: &str, ctx: &ExecContext) -> Result<Vec<ResultSet>> {
        let pool = self.pool(ctx.database()).await?;
        let mut conn = pool.acquire().await?;
        let (sets, truncated) =
            run_raw::<sqlx::MySql>(&mut *conn, query, ctx.limit(), cell, |r| r.rows_affected()).await?;
        if truncated {
            drop(conn.detach());
        }
        Ok(sets)
    }

    async fn catalog(&self, sql: &str) -> Result<Vec<Vec<String>>> {
        let ctx = ExecContext { limit: 100_000, ..Default::default() };
        Ok(strings(&self.run(sql, &ctx).await?))
    }

    fn columns_sql(db: &str, table: &str) -> String {
        format!(
            "SELECT COLUMN_NAME AS `column`, COLUMN_TYPE AS `type`, IS_NULLABLE AS `nullable`, \
             COLUMN_DEFAULT AS `default`, COLUMN_KEY AS `key`, EXTRA AS `extra`, COLUMN_COMMENT AS `comment` \
             FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = {} AND TABLE_NAME = {} ORDER BY ORDINAL_POSITION",
            lit(db),
            lit(table)
        )
    }
}

fn cell(row: &MySqlRow, i: usize) -> Value {
    let Ok(raw) = row.try_get_raw(i) else { return Value::Null };
    if raw.is_null() {
        return Value::Null;
    }
    let ty = raw.type_info().name().to_ascii_uppercase();
    if ty.contains("BLOB") || ty.contains("BINARY") || ty == "BIT" || ty == "GEOMETRY" {
        return row.try_get_unchecked::<Vec<u8>, _>(i).map(|b| bytes_value(&b)).unwrap_or(Value::Null);
    }
    let s = match row.try_get_unchecked::<String, _>(i) {
        Ok(s) => s,
        Err(_) => {
            return row.try_get_unchecked::<Vec<u8>, _>(i).map(|b| bytes_value(&b)).unwrap_or(Value::Null)
        }
    };
    if ty == "BOOLEAN" {
        return Value::Bool(s == "1");
    }
    if ty.contains("INT") || ty == "YEAR" || ty == "FLOAT" || ty == "DOUBLE" {
        return parse_number(&s);
    }
    if ty == "JSON" {
        return serde_json::from_str(&s).unwrap_or(Value::String(s));
    }
    Value::String(s)
}

#[async_trait]
impl Driver for MySqlDriver {
    async fn server_info(&self) -> Result<ServerInfo> {
        let v = first_value(&self.run("SELECT VERSION()", &ExecContext::default()).await?);
        Ok(ServerInfo { version: format!("MySQL {v}") })
    }

    async fn list_children(&self, path: &[String]) -> Result<Vec<TreeNode>> {
        Ok(match path {
            [] => self
                .catalog("SELECT SCHEMA_NAME FROM information_schema.SCHEMATA ORDER BY SCHEMA_NAME")
                .await?
                .into_iter()
                .map(|r| TreeNode::new(path, &r[0], "database", true))
                .collect(),
            [db] => self
                .catalog(&format!(
                    "SELECT TABLE_NAME, TABLE_TYPE, TABLE_ROWS FROM information_schema.TABLES \
                     WHERE TABLE_SCHEMA = {} ORDER BY TABLE_NAME",
                    lit(db)
                ))
                .await?
                .into_iter()
                .map(|r| {
                    let kind = if r[1].contains("VIEW") { "view" } else { "table" };
                    let detail = if kind == "table" && !r[2].is_empty() { format!("~{} rows", r[2]) } else { String::new() };
                    TreeNode::new(path, &r[0], kind, true).detail(detail)
                })
                .collect(),
            [db, table] => self
                .catalog(&Self::columns_sql(db, table))
                .await?
                .into_iter()
                .map(|r| {
                    let kind = if r[4] == "PRI" { "pk" } else { "column" };
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
        let [db, table, ..] = path else { bail!("Select a table or view") };
        Ok(ObjectQuery {
            query: format!("SELECT *\nFROM {}.{}\nLIMIT 100;", quote_ident(db, '`'), quote_ident(table, '`')),
            database: Some(db.clone()),
            schema: None,
        })
    }

    async fn edit_info(&self, path: &[String]) -> Result<EditInfo> {
        let mut info = sqlcommon::edit_info_from_describe(&self.describe(path).await?);
        info.upsert_note = Some("MySQL matches existing rows on the table's primary key or unique indexes.".into());
        Ok(info)
    }

    async fn apply_changes(&self, path: &[String], changes: &[RowChange], dry_run: bool) -> Result<ApplyReport> {
        let (target, mut conn) = self.target(path).await?;
        let mut ex = sqlcommon::SqlxConn(&mut *conn);
        let res = sqlcommon::apply_changes(SqlKind::Mysql, &mut ex, &target, changes, dry_run).await;
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
        let res = sqlcommon::import_rows(SqlKind::Mysql, &mut ex, &target, columns, rows, opts, ctx).await;
        if res.is_err() {
            drop(conn.detach());
        }
        res
    }

    async fn describe(&self, path: &[String]) -> Result<ResultSet> {
        let [db, table, ..] = path else { bail!("Select a table or view") };
        let ctx = ExecContext { limit: 10_000, ..Default::default() };
        Ok(self.run(&Self::columns_sql(db, table), &ctx).await?.pop().unwrap_or_default())
    }

    async fn close(&self) {
        for (_, pool) in self.pools.lock().await.drain() {
            pool.close().await;
        }
    }
}
