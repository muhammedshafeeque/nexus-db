use super::http::{Body, Http};
use crate::model::*;
use super::sqlcommon;
use crate::util::{first_keyword, quote_ident, sql_lit, split_statements};
use anyhow::{bail, Result};
use async_trait::async_trait;
use reqwest::Method;
use serde_json::{Map, Value};
use urlencoding::encode;

pub struct ClickHouseDriver {
    http: Http,
    default_db: String,
}

impl ClickHouseDriver {
    pub async fn connect(cfg: &ConnectionConfig) -> Result<Self> {
        let default_db = if cfg.database.is_empty() { "default".to_string() } else { cfg.database.clone() };
        let driver = Self { http: Http::new(cfg, 8123)?, default_db };
        driver.server_info().await?;
        Ok(driver)
    }

    async fn run(&self, stmt: &str, db: &str, limit: usize) -> Result<ResultSet> {
        let kw = first_keyword(stmt);
        let returns_rows = matches!(kw.as_str(), "SELECT" | "WITH" | "SHOW" | "DESC" | "DESCRIBE" | "EXPLAIN" | "EXISTS");
        let has_format = stmt.to_ascii_uppercase().contains(" FORMAT ");
        let body = if returns_rows && !has_format { format!("{stmt}\nFORMAT JSONCompact") } else { stmt.to_string() };
        let path = format!(
            "/?database={}&max_result_rows={}&result_overflow_mode=break",
            encode(db),
            limit + 1
        );
        let res = self.http.json(Method::POST, &path, Body::Text(body, "text/plain")).await?;
        if !returns_rows || has_format {
            return Ok(match res {
                Value::String(s) if !s.trim().is_empty() => {
                    ResultSet::table(vec![Column::new("result", "String")], vec![vec![Value::String(s)]])
                }
                _ => ResultSet::message("OK"),
            });
        }
        let columns: Vec<Column> = res["meta"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|m| Column::new(m["name"].as_str().unwrap_or(""), m["type"].as_str().unwrap_or("")))
            .collect();
        let mut rows: Vec<Vec<Value>> = match res.get("data") {
            Some(Value::Array(rows)) => rows.iter().map(|r| r.as_array().cloned().unwrap_or_default()).collect(),
            _ => Vec::new(),
        };
        let truncated = rows.len() > limit;
        rows.truncate(limit);
        let mut rs = ResultSet::table(columns, rows);
        rs.truncated = truncated;
        Ok(rs)
    }

    async fn catalog(&self, sql: &str) -> Result<Vec<Vec<String>>> {
        let rs = self.run(sql, &self.default_db, 100_000).await?;
        Ok(rs
            .rows
            .into_iter()
            .map(|r| r.into_iter().map(|v| v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())).collect())
            .collect())
    }
}

/// JSONEachRow is strict about numbers and booleans – convert text for those columns.
fn ch_value(v: &Value, type_name: &str) -> Value {
    let base = type_name.trim_start_matches("Nullable(").trim_start_matches("LowCardinality(");
    match v {
        Value::String(s) if base.starts_with("Int") || base.starts_with("UInt") || base.starts_with("Float") => {
            crate::tabular::detect_type(s.trim()).as_number().map(|n| Value::Number(n.clone())).unwrap_or_else(|| v.clone())
        }
        Value::String(s) if base.starts_with("Bool") => match s.trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" => Value::Bool(true),
            "false" | "0" | "no" => Value::Bool(false),
            _ => v.clone(),
        },
        Value::Number(_) if base.starts_with("String") => Value::String(v.to_string()),
        _ => v.clone(),
    }
}

#[async_trait]
impl Driver for ClickHouseDriver {
    async fn server_info(&self) -> Result<ServerInfo> {
        let rs = self.run("SELECT version()", &self.default_db, 1).await?;
        let v = rs.rows.first().and_then(|r| r.first()).and_then(Value::as_str).unwrap_or("?").to_string();
        Ok(ServerInfo { version: format!("ClickHouse {v}") })
    }

    async fn list_children(&self, path: &[String]) -> Result<Vec<TreeNode>> {
        Ok(match path {
            [] => self
                .catalog("SELECT name FROM system.databases ORDER BY name IN ('system','INFORMATION_SCHEMA','information_schema'), name")
                .await?
                .into_iter()
                .map(|r| TreeNode::new(path, &r[0], "database", true))
                .collect(),
            [db] => self
                .catalog(&format!(
                    "SELECT name, engine, toString(total_rows) FROM system.tables WHERE database = {} ORDER BY name",
                    sql_lit(db)
                ))
                .await?
                .into_iter()
                .map(|r| {
                    let kind = if r[1].contains("View") { "view" } else { "table" };
                    TreeNode::new(path, &r[0], kind, true).detail(&r[1])
                })
                .collect(),
            [db, table] => self
                .catalog(&format!(
                    "SELECT name, type, toString(is_in_primary_key) FROM system.columns \
                     WHERE database = {} AND table = {} ORDER BY position",
                    sql_lit(db),
                    sql_lit(table)
                ))
                .await?
                .into_iter()
                .map(|r| TreeNode::new(path, &r[0], if r[2] == "1" { "pk" } else { "column" }, false).detail(&r[1]))
                .collect(),
            _ => Vec::new(),
        })
    }

    async fn execute(&self, query: &str, ctx: &ExecContext) -> Result<Vec<ResultSet>> {
        let db = ctx.database().unwrap_or(&self.default_db).to_string();
        let mut sets = Vec::new();
        for stmt in split_statements(query) {
            sets.push(self.run(&stmt, &db, ctx.limit()).await?);
        }
        Ok(sets)
    }

    async fn object_query(&self, path: &[String]) -> Result<ObjectQuery> {
        let [db, table, ..] = path else { bail!("Select a table") };
        Ok(ObjectQuery {
            query: format!("SELECT *\nFROM {}.{}\nLIMIT 100", quote_ident(db, '`'), quote_ident(table, '`')),
            database: Some(db.clone()),
            schema: None,
        })
    }

    async fn edit_info(&self, path: &[String]) -> Result<EditInfo> {
        let mut info = sqlcommon::edit_info_from_describe(&self.describe(path).await?);
        // DESCRIBE has no nullable/key columns: derive them from the types
        for c in &mut info.columns {
            c.nullable = c.type_name.starts_with("Nullable(");
            c.key = false;
        }
        info.key_columns.clear();
        info.editable = false;
        info.reason = Some("ClickHouse tables are not edited row by row – use ALTER TABLE … UPDATE / DELETE. Importing works.".into());
        info.can_upsert = false;
        info.transactional = false;
        info.upsert_note = Some("ClickHouse only appends; use a ReplacingMergeTree table to keep the latest row per key.".into());
        Ok(info)
    }

    async fn import_rows(
        &self,
        path: &[String],
        columns: &[String],
        rows: Vec<ImportRow>,
        opts: &ImportOptions,
        ctx: &ImportCtx<'_>,
    ) -> Result<ImportReport> {
        let [db, table, ..] = path else { bail!("Select a table") };
        if opts.mode == ImportMode::Upsert {
            bail!("ClickHouse only appends rows – choose Insert (a ReplacingMergeTree table keeps the latest row per key).");
        }
        let info = self.edit_info(path).await?;
        let types: Vec<String> = columns
            .iter()
            .map(|c| info.columns.iter().find(|x| &x.name == c).map(|x| x.type_name.clone()).unwrap_or_default())
            .collect();
        let insert = format!(
            "INSERT INTO {}.{} ({}) FORMAT JSONEachRow",
            quote_ident(db, '`'),
            quote_ident(table, '`'),
            columns.iter().map(|c| quote_ident(c, '`')).collect::<Vec<_>>().join(", ")
        );
        let url = format!("/?database={}&query={}", encode(db), encode(&insert));
        let body_of = |batch: &[ImportRow]| {
            batch
                .iter()
                .map(|(_, values)| {
                    let obj: Map<String, Value> = columns.iter().zip(values).zip(&types).map(|((c, v), t)| (c.clone(), ch_value(v, t))).collect();
                    Value::Object(obj).to_string()
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        let mut report = ImportReport::default();
        let mut done = 0;
        for chunk in rows.chunks(opts.batch(10_000)) {
            if ctx.cancelled() {
                report.cancelled = true;
                break;
            }
            match self.http.json(Method::POST, &url, Body::Text(body_of(chunk), "application/x-ndjson")).await {
                Ok(_) => report.written += chunk.len() as u64,
                // an INSERT is all-or-nothing: find the bad rows, keep the good ones
                Err(_) => {
                    for row in chunk {
                        match self.http.json(Method::POST, &url, Body::Text(body_of(std::slice::from_ref(row)), "application/x-ndjson")).await {
                            Ok(_) => report.written += 1,
                            Err(e) => report.fail(row.0, format!("{e:#}")),
                        }
                    }
                }
            }
            done += chunk.len();
            (ctx.progress)(done);
        }
        Ok(report)
    }

    async fn describe(&self, path: &[String]) -> Result<ResultSet> {
        let [db, table, ..] = path else { bail!("Select a table") };
        self.run(&format!("DESCRIBE TABLE {}.{}", quote_ident(db, '`'), quote_ident(table, '`')), db, 10_000)
            .await
    }
}
