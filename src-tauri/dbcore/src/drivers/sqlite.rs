use super::sqlcommon::{self, first_value, run_raw, strings, SqlKind};
use crate::model::*;
use crate::util::{bytes_value, quote_ident, sql_lit};
use anyhow::{bail, Result};
use async_trait::async_trait;
use serde_json::Value;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions, SqliteRow};
use sqlx::{Row, TypeInfo, ValueRef};
use std::str::FromStr;

pub struct SqliteDriver {
    pool: SqlitePool,
}

impl SqliteDriver {
    pub async fn connect(cfg: &ConnectionConfig) -> Result<Self> {
        let opts = if cfg.has_url() {
            SqliteConnectOptions::from_str(cfg.url.trim())?
        } else if !cfg.file_path.trim().is_empty() {
            SqliteConnectOptions::new().filename(cfg.file_path.trim()).create_if_missing(true)
        } else {
            bail!("Choose a SQLite database file")
        };
        let pool = SqlitePoolOptions::new().max_connections(4).connect_with(opts).await?;
        Ok(Self { pool })
    }

    async fn target(&self, path: &[String]) -> Result<(String, sqlx::pool::PoolConnection<sqlx::Sqlite>)> {
        let [table, ..] = path else { bail!("Select a table") };
        Ok((quote_ident(table, '"'), self.pool.acquire().await?))
    }

    async fn run(&self, query: &str, limit: usize) -> Result<Vec<ResultSet>> {
        let mut conn = self.pool.acquire().await?;
        let (sets, truncated) = run_raw::<sqlx::Sqlite>(&mut *conn, query, limit, cell, |r| r.rows_affected()).await?;
        if truncated {
            drop(conn.detach());
        }
        Ok(sets)
    }

    fn columns_sql(table: &str) -> String {
        format!(
            "SELECT name AS \"column\", type, NOT \"notnull\" AS nullable, dflt_value AS \"default\", \
             pk AS primary_key FROM pragma_table_info({})",
            sql_lit(table)
        )
    }
}

fn cell(row: &SqliteRow, i: usize) -> Value {
    let Ok(raw) = row.try_get_raw(i) else { return Value::Null };
    if raw.is_null() {
        return Value::Null;
    }
    // the value's storage class, not the declared column type
    match raw.type_info().name() {
        "INTEGER" => row.try_get_unchecked::<i64, _>(i).map(Value::from).unwrap_or(Value::Null),
        "REAL" => row.try_get_unchecked::<f64, _>(i).map(Value::from).unwrap_or(Value::Null),
        "BLOB" => row.try_get_unchecked::<Vec<u8>, _>(i).map(|b| bytes_value(&b)).unwrap_or(Value::Null),
        _ => row.try_get_unchecked::<String, _>(i).map(Value::String).unwrap_or(Value::Null),
    }
}

#[async_trait]
impl Driver for SqliteDriver {
    async fn server_info(&self) -> Result<ServerInfo> {
        let v = first_value(&self.run("SELECT sqlite_version()", 1).await?);
        Ok(ServerInfo { version: format!("SQLite {v}") })
    }

    async fn list_children(&self, path: &[String]) -> Result<Vec<TreeNode>> {
        Ok(match path {
            [] => strings(
                &self
                    .run(
                        "SELECT name, type FROM sqlite_master WHERE type IN ('table','view') \
                         AND name NOT LIKE 'sqlite_%' ORDER BY name",
                        100_000,
                    )
                    .await?,
            )
            .into_iter()
            .map(|r| TreeNode::new(path, &r[0], &r[1], true))
            .collect(),
            [table] => strings(&self.run(&Self::columns_sql(table), 10_000).await?)
                .into_iter()
                .map(|r| {
                    let kind = if r[4] != "0" { "pk" } else { "column" };
                    TreeNode::new(path, &r[0], kind, false).detail(&r[1])
                })
                .collect(),
            _ => Vec::new(),
        })
    }

    async fn execute(&self, query: &str, ctx: &ExecContext) -> Result<Vec<ResultSet>> {
        self.run(query, ctx.limit()).await
    }

    async fn object_query(&self, path: &[String]) -> Result<ObjectQuery> {
        let [table, ..] = path else { bail!("Select a table or view") };
        Ok(ObjectQuery {
            query: format!("SELECT *\nFROM {}\nLIMIT 100;", quote_ident(table, '"')),
            ..Default::default()
        })
    }

    async fn edit_info(&self, path: &[String]) -> Result<EditInfo> {
        Ok(sqlcommon::edit_info_from_describe(&self.describe(path).await?))
    }

    async fn apply_changes(&self, path: &[String], changes: &[RowChange], dry_run: bool) -> Result<ApplyReport> {
        let (target, mut conn) = self.target(path).await?;
        let mut ex = sqlcommon::SqlxConn(&mut *conn);
        let res = sqlcommon::apply_changes(SqlKind::Sqlite, &mut ex, &target, changes, dry_run).await;
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
        let res = sqlcommon::import_rows(SqlKind::Sqlite, &mut ex, &target, columns, rows, opts, ctx).await;
        if res.is_err() {
            drop(conn.detach());
        }
        res
    }

    async fn describe(&self, path: &[String]) -> Result<ResultSet> {
        let [table, ..] = path else { bail!("Select a table or view") };
        Ok(self.run(&Self::columns_sql(table), 10_000).await?.pop().unwrap_or_default())
    }

    async fn schema_doc(&self, _database: Option<&str>, _schema: Option<&str>) -> Result<SchemaDoc> {
        let user = "m.type IN ('table', 'view') AND m.name NOT LIKE 'sqlite\\_%' ESCAPE '\\'";
        let tables = format!("SELECT 'main', m.name, m.type, NULL, NULL FROM sqlite_master m WHERE {user} ORDER BY m.name");
        let columns = format!(
            "SELECT 'main', m.name, p.cid, p.name, p.type, NOT p.\"notnull\" AND p.pk = 0, p.dflt_value, NULL \
             FROM sqlite_master m JOIN pragma_table_info(m.name) p WHERE {user} ORDER BY m.name, p.cid"
        );
        let constraints = format!(
            "SELECT 'main', m.name, 'PRIMARY KEY', 'p', p.name, p.pk, NULL, NULL, NULL, NULL, NULL \
             FROM sqlite_master m JOIN pragma_table_info(m.name) p WHERE {user} AND p.pk > 0 \
             UNION ALL \
             SELECT 'main', m.name, 'fk_' || m.name || '_' || f.id, 'f', f.\"from\", f.seq + 1, 'main', f.\"table\", f.\"to\", \
               f.on_update, f.on_delete \
             FROM sqlite_master m JOIN pragma_foreign_key_list(m.name) f WHERE {user} \
             ORDER BY 2, 3, 6"
        );
        let indexes = format!(
            "SELECT 'main', m.name, il.name, il.\"unique\", il.origin = 'pk', ii.name, ii.seqno \
             FROM sqlite_master m JOIN pragma_index_list(m.name) il JOIN pragma_index_info(il.name) ii \
             WHERE {user} ORDER BY m.name, il.name, ii.seqno"
        );
        let mut cat = Vec::new();
        for sql in [tables, columns, constraints, indexes] {
            cat.push(sqlcommon::strings(&self.run(&sql, 1_000_000).await?));
        }
        let [tables, columns, constraints, indexes] = <[_; 4]>::try_from(cat).unwrap();
        Ok(crate::schemadoc::build("main", crate::schemadoc::Catalog { tables, columns, constraints, indexes }))
    }

    async fn close(&self) {
        self.pool.close().await;
    }
}
