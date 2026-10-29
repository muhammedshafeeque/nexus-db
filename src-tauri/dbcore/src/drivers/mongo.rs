use crate::auth;
use crate::model::*;
use crate::tabular::build_doc;
use crate::util::{docs_to_resultset, document_edit_info, infer_schema, is_simple_ident, obj};
use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use futures::{StreamExt, TryStreamExt};
use mongodb::bson::{doc, Bson, Document};
use futures::FutureExt;
use mongodb::options::oidc::{Callback, CallbackContext, IdpServerResponse};
use mongodb::options::{AuthMechanism, ClientOptions, Credential};
use mongodb::{Client, Collection, IndexModel};
use regex::Regex;
use serde_json::{Map, Value};
use std::sync::LazyLock;
use std::time::Duration;

pub struct MongoDriver {
    client: Client,
    default_db: String,
}

impl MongoDriver {
    pub async fn connect(cfg: &ConnectionConfig) -> Result<Self> {
        let entra = auth::token_source(cfg)?;
        let uri = Self::uri(cfg, entra.is_some());
        let mut opts = ClientOptions::parse(&uri).await?;
        opts.app_name = Some("Nexus DB".into());
        opts.server_selection_timeout = Some(Duration::from_secs(10));
        opts.connect_timeout = Some(Duration::from_secs(10));
        if let Some(src) = entra {
            // sign in now: the driver gives callbacks only ~1 minute, too short for a browser login
            src.token().await?;
            let mut credential = Credential::builder()
                .mechanism(AuthMechanism::MongoDbOidc)
                .username(Some(cfg.user.trim().to_string()).filter(|u| !u.is_empty()))
                .source(Some("$external".to_string()))
                .build();
            // the driver calls back on connect and on re-authentication (error 391)
            credential.oidc_callback = Callback::machine(move |_: CallbackContext| {
                let src = src.clone();
                async move {
                    let token = src.token().await.map_err(|e| mongodb::error::Error::custom(format!("{e:#}")))?;
                    let expires = token.expires_instant();
                    Ok(IdpServerResponse::builder().access_token(token.secret).expires(Some(expires)).build())
                }
                .boxed()
            });
            opts.credential = Some(credential);
        }
        let default_db = if !cfg.database.is_empty() {
            cfg.database.clone()
        } else {
            opts.default_database.clone().unwrap_or_else(|| "test".into())
        };
        let client = Client::with_options(opts)?;
        client.database("admin").run_command(doc! { "ping": 1 }).await?;
        Ok(Self { client, default_db })
    }

    /// Connection string for the driver and for mongodump / mongorestore.
    pub fn uri(cfg: &ConnectionConfig, entra: bool) -> String {
        if cfg.has_url() {
            return cfg.url.trim().to_string();
        }
        let password_auth = !cfg.user.is_empty() && !entra;
        let auth = if password_auth {
            format!("{}:{}@", urlencoding::encode(&cfg.user), urlencoding::encode(&cfg.password))
        } else {
            String::new()
        };
        let mut params = vec!["directConnection=true".to_string()];
        if password_auth {
            params.push("authSource=admin".into());
        }
        if cfg.ssl || entra {
            params.push("tls=true".into());
        }
        if cfg.trust_cert {
            params.push("tlsAllowInvalidCertificates=true".into());
        }
        format!("mongodb://{auth}{}:{}/?{}", cfg.host_or_default(), cfg.port_or(27017), params.join("&"))
    }

    fn coll(&self, db: &str, name: &str) -> Collection<Document> {
        self.client.database(db).collection(name)
    }

    async fn run_one(&self, db: &str, stmt: &str, limit: usize) -> Result<ResultSet> {
        let stmt = stmt.trim().trim_end_matches(';').trim();
        let lower = stmt.to_ascii_lowercase();
        if lower == "show dbs" || lower == "show databases" {
            let names = self.client.list_database_names().await?;
            return Ok(scalar_list("database", names.into_iter().map(Value::String).collect()));
        }
        if lower == "show collections" || lower == "show tables" {
            let mut names = self.client.database(db).list_collection_names().await?;
            names.sort();
            return Ok(scalar_list("collection", names.into_iter().map(Value::String).collect()));
        }
        if lower.starts_with("use ") {
            bail!("Use the database selector in the toolbar to switch databases");
        }
        let rest = stmt.strip_prefix("db").ok_or_else(|| anyhow!("Expected a statement starting with `db.`"))?;
        let mut calls = parse_chain(rest)?;
        if calls.is_empty() {
            bail!("Empty statement");
        }
        let first = calls.remove(0);
        let coll_name = match (&first.name[..], &first.args) {
            ("getCollection", Some(args)) => match parse_args(args)?.first() {
                Some(Bson::String(s)) => s.clone(),
                _ => bail!("getCollection() expects a collection name"),
            },
            (_, Some(args)) => return self.db_method(db, &first.name, parse_args(args)?).await,
            (name, None) => name.to_string(),
        };
        if calls.is_empty() {
            bail!("Expected a method call, e.g. db.{coll_name}.find({{}})");
        }
        let method = calls.remove(0);
        let args = parse_args(method.args.as_deref().ok_or_else(|| anyhow!("`{}` must be called", method.name))?)?;
        let modifiers = calls
            .into_iter()
            .map(|c| Ok((c.name, parse_args(c.args.as_deref().unwrap_or(""))?)))
            .collect::<Result<Vec<_>>>()?;
        self.coll_method(db, &coll_name, &method.name, args, modifiers, limit).await
    }

    async fn db_method(&self, db: &str, method: &str, args: Vec<Bson>) -> Result<ResultSet> {
        let database = self.client.database(db);
        match method {
            "runCommand" | "adminCommand" => {
                let cmd = as_doc(args.first())?;
                let target = if method == "adminCommand" { self.client.database("admin") } else { database };
                Ok(docs_to_resultset(vec![bson_json(Bson::Document(target.run_command(cmd).await?))]))
            }
            "getCollectionNames" => {
                let mut names = database.list_collection_names().await?;
                names.sort();
                Ok(scalar_list("collection", names.into_iter().map(Value::String).collect()))
            }
            "createCollection" => {
                let Some(Bson::String(name)) = args.first() else { bail!("createCollection(name)") };
                database.create_collection(name).await?;
                Ok(ResultSet::message(format!("Collection '{name}' created")))
            }
            "stats" => Ok(docs_to_resultset(vec![bson_json(Bson::Document(
                database.run_command(doc! { "dbStats": 1 }).await?,
            ))])),
            "getName" => Ok(scalar_list("name", vec![Value::String(db.to_string())])),
            other => bail!("Unsupported database method `{other}`"),
        }
    }

    async fn coll_method(
        &self,
        db: &str,
        name: &str,
        method: &str,
        args: Vec<Bson>,
        modifiers: Vec<(String, Vec<Bson>)>,
        limit: usize,
    ) -> Result<ResultSet> {
        let coll = self.coll(db, name);
        let arg = |i: usize| args.get(i).cloned();
        match method {
            "find" | "findOne" => {
                let mut find = coll.find(opt_doc(arg(0))?);
                if let Some(p) = arg(1) {
                    find = find.projection(as_doc(Some(&p))?);
                }
                let mut user_limit: Option<i64> = if method == "findOne" { Some(1) } else { None };
                for (m, margs) in modifiers {
                    match m.as_str() {
                        "sort" => find = find.sort(as_doc(margs.first())?),
                        "limit" => user_limit = Some(as_i64(margs.first())?),
                        "skip" => find = find.skip(as_i64(margs.first())?.max(0) as u64),
                        "projection" => find = find.projection(as_doc(margs.first())?),
                        "count" => bail!("Use db.{name}.countDocuments(filter) instead of .count()"),
                        "pretty" | "toArray" => {}
                        other => bail!("Unsupported cursor modifier `.{other}()`"),
                    }
                }
                let cap = limit as i64;
                let eff = user_limit.filter(|l| *l > 0).map(|l| l.min(cap)).unwrap_or(cap);
                let mut cursor = find.limit(eff + 1).await?;
                let mut docs = Vec::new();
                while let Some(d) = cursor.try_next().await? {
                    docs.push(bson_json(Bson::Document(d)));
                }
                let truncated = docs.len() as i64 > eff;
                docs.truncate(eff as usize);
                let mut rs = docs_to_resultset(docs);
                rs.truncated = truncated && user_limit.is_none();
                Ok(rs)
            }
            "aggregate" => {
                let pipeline = match arg(0) {
                    Some(Bson::Array(stages)) => {
                        stages.into_iter().map(|s| as_doc(Some(&s))).collect::<Result<Vec<_>>>()?
                    }
                    None => Vec::new(),
                    _ => bail!("aggregate() expects an array of stages"),
                };
                let mut cursor = coll.aggregate(pipeline).await?;
                let mut docs = Vec::new();
                let mut truncated = false;
                while let Some(d) = cursor.try_next().await? {
                    if docs.len() >= limit {
                        truncated = true;
                        break;
                    }
                    docs.push(bson_json(Bson::Document(d)));
                }
                let mut rs = docs_to_resultset(docs);
                rs.truncated = truncated;
                Ok(rs)
            }
            "countDocuments" | "count" => {
                let n = coll.count_documents(opt_doc(arg(0))?).await?;
                Ok(scalar_list("count", vec![Value::from(n)]))
            }
            "estimatedDocumentCount" => {
                Ok(scalar_list("count", vec![Value::from(coll.estimated_document_count().await?)]))
            }
            "distinct" => {
                let Some(Bson::String(field)) = arg(0) else { bail!("distinct(field, filter?)") };
                let values = coll.distinct(field.as_str(), opt_doc(arg(1))?).await?;
                Ok(scalar_list(&field, values.into_iter().map(bson_json).collect()))
            }
            "insertOne" => {
                let r = coll.insert_one(as_doc(args.first())?).await?;
                Ok(ack(1, format!("Inserted document {}", bson_json(r.inserted_id))))
            }
            "insertMany" => {
                let Some(Bson::Array(items)) = arg(0) else { bail!("insertMany([...docs])") };
                let docs = items.iter().map(|d| as_doc(Some(d))).collect::<Result<Vec<_>>>()?;
                let r = coll.insert_many(docs).await?;
                let n = r.inserted_ids.len() as u64;
                Ok(ack(n, format!("Inserted {n} documents")))
            }
            "updateOne" | "updateMany" => {
                let filter = as_doc(args.first())?;
                let r = match (method, arg(1)) {
                    ("updateOne", Some(Bson::Array(p))) => coll.update_one(filter, to_pipeline(p)?).await?,
                    ("updateMany", Some(Bson::Array(p))) => coll.update_many(filter, to_pipeline(p)?).await?,
                    ("updateOne", u) => coll.update_one(filter, as_doc(u.as_ref())?).await?,
                    (_, u) => coll.update_many(filter, as_doc(u.as_ref())?).await?,
                };
                Ok(ack(r.modified_count, format!("Matched {}, modified {}", r.matched_count, r.modified_count)))
            }
            "replaceOne" => {
                let r = coll.replace_one(as_doc(args.first())?, as_doc(args.get(1))?).await?;
                Ok(ack(r.modified_count, format!("Matched {}, modified {}", r.matched_count, r.modified_count)))
            }
            "deleteOne" | "deleteMany" => {
                let filter = as_doc(args.first())?;
                let r = if method == "deleteOne" {
                    coll.delete_one(filter).await?
                } else {
                    coll.delete_many(filter).await?
                };
                Ok(ack(r.deleted_count, format!("Deleted {} documents", r.deleted_count)))
            }
            "getIndexes" => {
                let mut cursor = coll.list_indexes().await?;
                let mut docs = Vec::new();
                while let Some(ix) = cursor.try_next().await? {
                    docs.push(obj(vec![
                        ("name", ix.options.and_then(|o| o.name).map(Value::String).unwrap_or(Value::Null)),
                        ("keys", bson_json(Bson::Document(ix.keys))),
                    ]));
                }
                Ok(docs_to_resultset(docs))
            }
            "createIndex" => {
                let r = coll.create_index(IndexModel::builder().keys(as_doc(args.first())?).build()).await?;
                Ok(ResultSet::message(format!("Created index {}", r.index_name)))
            }
            "drop" => {
                coll.drop().await?;
                Ok(ResultSet::message(format!("Dropped collection '{name}'")))
            }
            other => bail!("Unsupported collection method `{other}`"),
        }
    }
}

fn ack(n: u64, msg: String) -> ResultSet {
    ResultSet { affected: Some(n), message: Some(msg), ..Default::default() }
}

fn scalar_list(col: &str, values: Vec<Value>) -> ResultSet {
    ResultSet::table(vec![Column::new(col, "")], values.into_iter().map(|v| vec![v]).collect())
}

fn as_doc(b: Option<&Bson>) -> Result<Document> {
    match b {
        Some(Bson::Document(d)) => Ok(d.clone()),
        None => Ok(Document::new()),
        Some(other) => bail!("Expected an object, got {other}"),
    }
}

fn opt_doc(b: Option<Bson>) -> Result<Document> {
    as_doc(b.as_ref())
}

fn to_pipeline(stages: Vec<Bson>) -> Result<Vec<Document>> {
    stages.iter().map(|s| as_doc(Some(s))).collect()
}

fn as_i64(b: Option<&Bson>) -> Result<i64> {
    match b {
        Some(Bson::Int32(i)) => Ok(*i as i64),
        Some(Bson::Int64(i)) => Ok(*i),
        Some(Bson::Double(f)) => Ok(*f as i64),
        _ => bail!("Expected a number"),
    }
}

/// BSON → display JSON: ObjectIds and dates become readable strings.
pub fn bson_json(b: Bson) -> Value {
    match b {
        Bson::Document(d) => Value::Object(d.into_iter().map(|(k, v)| (k, bson_json(v))).collect()),
        Bson::Array(a) => Value::Array(a.into_iter().map(bson_json).collect()),
        Bson::ObjectId(o) => Value::String(format!("ObjectId('{}')", o.to_hex())),
        Bson::DateTime(d) => Value::String(d.try_to_rfc3339_string().unwrap_or_else(|_| d.to_string())),
        Bson::Decimal128(d) => Value::String(d.to_string()),
        Bson::Int32(i) => Value::from(i),
        Bson::Int64(i) => Value::from(i),
        Bson::Double(f) => serde_json::Number::from_f64(f).map(Value::Number).unwrap_or(Value::String(f.to_string())),
        Bson::String(s) => Value::String(s),
        Bson::Boolean(b) => Value::Bool(b),
        Bson::Null | Bson::Undefined => Value::Null,
        other => other.into_relaxed_extjson(),
    }
}

// ---------------------------------------------------------------- shell parser

struct Call {
    name: String,
    args: Option<String>,
}

/// Parses `.coll.find({...}).sort({...})` / `["name"].find()` into calls.
fn parse_chain(s: &str) -> Result<Vec<Call>> {
    let chars: Vec<char> = s.chars().collect();
    let mut calls: Vec<Call> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        match c {
            '.' => {
                i += 1;
                while i < chars.len() && chars[i].is_whitespace() {
                    i += 1;
                }
                let start = i;
                while i < chars.len() && (chars[i].is_alphanumeric() || matches!(chars[i], '_' | '$' | '-')) {
                    i += 1;
                }
                if start == i {
                    bail!("Expected a name after '.'");
                }
                calls.push(Call { name: chars[start..i].iter().collect(), args: None });
            }
            '[' | '(' => {
                let close = if c == '[' { ']' } else { ')' };
                let end = matching(&chars, i, c, close)?;
                let inner: String = chars[i + 1..end].iter().collect();
                if c == '[' {
                    let name: String = json5::from_str(inner.trim()).map_err(|_| anyhow!("Invalid collection name {inner}"))?;
                    calls.push(Call { name, args: None });
                } else {
                    let last = calls.last_mut().ok_or_else(|| anyhow!("Unexpected '('"))?;
                    last.args = Some(inner);
                }
                i = end + 1;
            }
            other => bail!("Unexpected character '{other}'"),
        }
    }
    Ok(calls)
}

fn matching(chars: &[char], open_at: usize, open: char, close: char) -> Result<usize> {
    let mut depth = 0;
    let mut i = open_at;
    while i < chars.len() {
        let c = chars[i];
        if c == '"' || c == '\'' || c == '`' {
            i += 1;
            while i < chars.len() && chars[i] != c {
                if chars[i] == '\\' {
                    i += 1;
                }
                i += 1;
            }
        } else if c == open {
            depth += 1;
        } else if c == close {
            depth -= 1;
            if depth == 0 {
                return Ok(i);
            }
        }
        i += 1;
    }
    bail!("Missing closing '{close}'")
}

static OBJECT_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"ObjectId\(\s*["']([0-9a-fA-F]{24})["']\s*\)"#).unwrap());
static DATE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?:ISODate|new\s+Date)\(\s*["']([^"']+)["']\s*\)"#).unwrap());
static NOW: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?:new\s+Date|ISODate)\(\s*\)"#).unwrap());
static LONG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"NumberLong\(\s*["']?(-?\d+)["']?\s*\)"#).unwrap());
static INT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"NumberInt\(\s*["']?(-?\d+)["']?\s*\)"#).unwrap());
static DECIMAL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"NumberDecimal\(\s*["']([^"']+)["']\s*\)"#).unwrap());
static UUID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"UUID\(\s*["']([^"']+)["']\s*\)"#).unwrap());

/// Rewrites mongo-shell helpers into extended JSON, then parses JSON5 args.
fn parse_args(raw: &str) -> Result<Vec<Bson>> {
    let s = OBJECT_ID.replace_all(raw, r#"{"$$oid":"$1"}"#);
    let s = DATE.replace_all(&s, |c: &regex::Captures| {
        let d = &c[1];
        let iso = if d.len() == 10 { format!("{d}T00:00:00Z") } else { d.to_string() };
        format!(r#"{{"$date":"{iso}"}}"#)
    });
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let s = NOW.replace_all(&s, format!(r#"{{"$$date":"{now}"}}"#).as_str());
    let s = LONG.replace_all(&s, r#"{"$$numberLong":"$1"}"#);
    let s = INT.replace_all(&s, "$1");
    let s = DECIMAL.replace_all(&s, r#"{"$$numberDecimal":"$1"}"#);
    let s = UUID.replace_all(&s, r#"{"$$uuid":"$1"}"#);
    let value: Value = json5::from_str(&format!("[{s}]")).map_err(|e| anyhow!("Invalid arguments: {e}"))?;
    let Value::Array(items) = value else { unreachable!() };
    items
        .into_iter()
        .map(|v| Bson::try_from(v).map_err(|e| anyhow!("Invalid extended JSON: {e}")))
        .collect()
}

/// Splits a mongo script into statements on `;` and on new lines that start a
/// new `db.` / `show` statement at nesting depth 0.
fn split_mongo(src: &str) -> Vec<String> {
    let cleaned: String = src
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    let chars: Vec<char> = cleaned.chars().collect();
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0i32;
    let mut i = 0;
    let push = |cur: &mut String, out: &mut Vec<String>| {
        if !cur.trim().is_empty() {
            out.push(cur.trim().to_string());
        }
        cur.clear();
    };
    while i < chars.len() {
        let c = chars[i];
        match c {
            '"' | '\'' | '`' => {
                cur.push(c);
                i += 1;
                while i < chars.len() && chars[i] != c {
                    if chars[i] == '\\' && i + 1 < chars.len() {
                        cur.push(chars[i]);
                        i += 1;
                    }
                    cur.push(chars[i]);
                    i += 1;
                }
                if i < chars.len() {
                    cur.push(c);
                }
            }
            '(' | '[' | '{' => {
                depth += 1;
                cur.push(c)
            }
            ')' | ']' | '}' => {
                depth -= 1;
                cur.push(c)
            }
            ';' if depth <= 0 => push(&mut cur, &mut out),
            '\n' if depth <= 0 => {
                let rest: String = chars[i + 1..].iter().take(8).collect();
                let rest = rest.trim_start();
                if rest.starts_with("db.") || rest.starts_with("db[") || rest.starts_with("show ") {
                    push(&mut cur, &mut out);
                } else {
                    cur.push(c);
                }
            }
            _ => cur.push(c),
        }
        i += 1;
    }
    push(&mut cur, &mut out);
    out
}

// ---------------------------------------------------------------- grid edits & import

/// Grid / import JSON → BSON. `ObjectId('…')` and `ISODate('…')` strings as
/// shown in the grid become real ObjectIds and dates.
fn json_to_bson(v: &Value) -> Result<Bson> {
    Ok(match v {
        Value::String(s) => {
            if let Some(c) = OBJECT_ID.captures(s).filter(|c| c.get(0).map(|m| m.as_str().len()) == Some(s.len())) {
                Bson::ObjectId(mongodb::bson::oid::ObjectId::parse_str(&c[1])?)
            } else if let Some(c) = DATE.captures(s).filter(|c| c.get(0).map(|m| m.as_str().len()) == Some(s.len())) {
                Bson::DateTime(parse_date(&c[1]).ok_or_else(|| anyhow!("Invalid date {}", &c[1]))?)
            } else {
                Bson::String(s.clone())
            }
        }
        Value::Array(a) => Bson::Array(a.iter().map(json_to_bson).collect::<Result<_>>()?),
        Value::Object(m) if m.keys().any(|k| k.starts_with('$')) => {
            Bson::try_from(v.clone()).map_err(|e| anyhow!("Invalid extended JSON: {e}"))?
        }
        Value::Object(m) => Bson::Document(json_doc(m)?),
        Value::Number(n) => match n.as_i64() {
            Some(i) if i32::try_from(i).is_ok() => Bson::Int32(i as i32),
            Some(i) => Bson::Int64(i),
            None => Bson::Double(n.as_f64().unwrap_or(0.0)),
        },
        Value::Bool(b) => Bson::Boolean(*b),
        Value::Null => Bson::Null,
    })
}

fn json_doc(m: &Map<String, Value>) -> Result<Document> {
    m.iter().map(|(k, v)| Ok((k.clone(), json_to_bson(v)?))).collect()
}

fn parse_date(s: &str) -> Option<mongodb::bson::DateTime> {
    let s = s.trim();
    if let Ok(d) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(mongodb::bson::DateTime::from_millis(d.timestamp_millis()));
    }
    ["%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M"]
        .iter()
        .find_map(|f| chrono::NaiveDateTime::parse_from_str(s, f).ok())
        .or_else(|| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok().and_then(|d| d.and_hms_opt(0, 0, 0)))
        .map(|d| mongodb::bson::DateTime::from_millis(d.and_utc().timestamp_millis()))
}

/// Keeps the stored BSON type when the grid sends text or a JSON number:
/// dates stay dates, Int32 stays Int32, Decimal128 stays Decimal128 …
fn coerce_like(existing: Option<&Bson>, new: Bson) -> Bson {
    use std::str::FromStr;
    let num = |s: &str| s.trim().parse::<f64>().ok();
    match (existing, new) {
        (Some(Bson::DateTime(_)), Bson::String(s)) => parse_date(&s).map(Bson::DateTime).unwrap_or(Bson::String(s)),
        (Some(Bson::ObjectId(_)), Bson::String(s)) if s.len() == 24 => {
            mongodb::bson::oid::ObjectId::parse_str(&s).map(Bson::ObjectId).unwrap_or(Bson::String(s))
        }
        (Some(Bson::Boolean(_)), Bson::String(s)) if s.eq_ignore_ascii_case("true") || s.eq_ignore_ascii_case("false") => {
            Bson::Boolean(s.eq_ignore_ascii_case("true"))
        }
        (Some(Bson::Int32(_)), Bson::Int64(i)) => i32::try_from(i).map(Bson::Int32).unwrap_or(Bson::Int64(i)),
        (Some(Bson::Int64(_)), Bson::Int32(i)) => Bson::Int64(i as i64),
        (Some(Bson::Double(_)), Bson::Int32(i)) => Bson::Double(i as f64),
        (Some(Bson::Double(_)), Bson::Int64(i)) => Bson::Double(i as f64),
        (Some(Bson::Int32(_)), Bson::String(s)) => s.trim().parse::<i32>().map(Bson::Int32).unwrap_or(Bson::String(s)),
        (Some(Bson::Int64(_)), Bson::String(s)) => s.trim().parse::<i64>().map(Bson::Int64).unwrap_or(Bson::String(s)),
        (Some(Bson::Double(_)), Bson::String(s)) => num(&s).map(Bson::Double).unwrap_or(Bson::String(s)),
        (Some(Bson::Decimal128(_)), new @ (Bson::String(_) | Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_))) => {
            let text = match &new {
                Bson::String(s) => s.trim().to_string(),
                other => other.to_string(),
            };
            mongodb::bson::Decimal128::from_str(&text).map(Bson::Decimal128).unwrap_or(new)
        }
        (_, new) => new,
    }
}

fn key_filter(key: &Map<String, Value>) -> Result<Document> {
    if key.is_empty() {
        bail!("The document has no _id – refresh the data and try again.");
    }
    json_doc(key)
}

fn describe_change(coll: &str, c: &RowChange) -> String {
    match c {
        RowChange::Update { key, values } => format!("db.{coll}.updateOne({}, {{ $set: {} }})", Value::Object(key.clone()), Value::Object(values.clone())),
        RowChange::Insert { values } => format!("db.{coll}.insertOne({})", Value::Object(values.clone())),
        RowChange::Delete { key } => format!("db.{coll}.deleteOne({})", Value::Object(key.clone())),
    }
}

impl MongoDriver {
    async fn apply_one(&self, coll: &Collection<Document>, change: &RowChange) -> Result<()> {
        match change {
            RowChange::Update { key, values } => {
                let filter = key_filter(key)?;
                let existing = coll
                    .find_one(filter.clone())
                    .await?
                    .ok_or_else(|| anyhow!("document not found – it may have been deleted. Refresh and try again."))?;
                let mut set = Document::new();
                for (k, v) in values {
                    set.insert(k.clone(), coerce_like(existing.get(k), json_to_bson(v)?));
                }
                let res = coll.update_one(filter, doc! { "$set": set }).await?;
                if res.matched_count != 1 {
                    bail!("document not found – it may have been deleted. Refresh and try again.");
                }
            }
            RowChange::Insert { values } => {
                coll.insert_one(json_doc(values)?).await?;
            }
            RowChange::Delete { key } => {
                if coll.delete_one(key_filter(key)?).await?.deleted_count != 1 {
                    bail!("document not found – it may already have been deleted.");
                }
            }
        }
        Ok(())
    }
}

#[async_trait]
impl Driver for MongoDriver {
    async fn server_info(&self) -> Result<ServerInfo> {
        let info = self.client.database("admin").run_command(doc! { "buildInfo": 1 }).await?;
        Ok(ServerInfo { version: format!("MongoDB {}", info.get_str("version").unwrap_or("?")) })
    }

    async fn list_children(&self, path: &[String]) -> Result<Vec<TreeNode>> {
        Ok(match path {
            [] => self
                .client
                .list_database_names()
                .await?
                .into_iter()
                .map(|n| TreeNode::new(path, n, "database", true))
                .collect(),
            [db] => {
                let mut names = self.client.database(db).list_collection_names().await?;
                names.sort();
                names.into_iter().map(|n| TreeNode::new(path, n, "collection", true)).collect()
            }
            [db, coll] => {
                let mut cursor = self.coll(db, coll).list_indexes().await?;
                let mut nodes = Vec::new();
                while let Some(ix) = cursor.try_next().await? {
                    let keys = ix.keys.keys().cloned().collect::<Vec<_>>().join(", ");
                    let name = ix.options.and_then(|o| o.name).unwrap_or_else(|| keys.clone());
                    nodes.push(TreeNode::new(path, name, "index", false).detail(keys));
                }
                nodes
            }
            _ => Vec::new(),
        })
    }

    async fn execute(&self, query: &str, ctx: &ExecContext) -> Result<Vec<ResultSet>> {
        let db = ctx.database().unwrap_or(&self.default_db).to_string();
        let mut sets = Vec::new();
        for stmt in split_mongo(query) {
            let rs = self
                .run_one(&db, &stmt, ctx.limit())
                .await
                .with_context(|| format!("in `{}`", stmt.chars().take(80).collect::<String>()))?;
            sets.push(rs);
        }
        Ok(sets)
    }

    async fn object_query(&self, path: &[String]) -> Result<ObjectQuery> {
        let [db, coll, ..] = path else { bail!("Select a collection") };
        let target = if is_simple_ident(coll) { format!("db.{coll}") } else { format!("db.getCollection({coll:?})") };
        Ok(ObjectQuery {
            query: format!("{target}.find({{}})\n  .sort({{ _id: -1 }})\n  .limit(100)"),
            database: Some(db.clone()),
            schema: None,
        })
    }

    async fn edit_info(&self, path: &[String]) -> Result<EditInfo> {
        let [_, _, ..] = path else { bail!("Select a collection") };
        // a collection that does not exist yet (new import target) samples as empty
        let described = self.describe(path).await.ok();
        let mut info = document_edit_info(described.as_ref(), &["_id"]);
        info.reason = Some("Changes are saved one by one (MongoDB multi-document transactions need a replica set).".into());
        Ok(info)
    }

    async fn apply_changes(&self, path: &[String], changes: &[RowChange], dry_run: bool) -> Result<ApplyReport> {
        let [db, coll_name, ..] = path else { bail!("Select a collection") };
        let coll = self.coll(db, coll_name);
        let mut report = ApplyReport { statements: changes.iter().map(|c| describe_change(coll_name, c)).collect(), ..Default::default() };
        if dry_run {
            return Ok(report);
        }
        for (i, change) in changes.iter().enumerate() {
            if let Err(e) = self.apply_one(&coll, change).await {
                report.error = Some(format!(
                    "Change {} of {}: {e:#}\n{i} earlier change(s) were saved.",
                    i + 1,
                    changes.len()
                ));
                return Ok(report);
            }
            report.applied = i + 1;
        }
        Ok(report)
    }

    async fn import_rows(
        &self,
        path: &[String],
        columns: &[String],
        rows: Vec<ImportRow>,
        opts: &ImportOptions,
        ctx: &ImportCtx<'_>,
    ) -> Result<ImportReport> {
        let [db, coll_name, ..] = path else { bail!("Select a collection") };
        let coll = self.coll(db, coll_name);
        let key_idx: Vec<usize> = opts.key_columns.iter().filter_map(|k| columns.iter().position(|c| c == k)).collect();
        if opts.mode == ImportMode::Upsert && key_idx.is_empty() {
            bail!("Choose the key field(s) used to match existing documents.");
        }
        let mut report = ImportReport::default();
        let mut done = 0;
        for chunk in rows.chunks(opts.batch(1000)) {
            if ctx.cancelled() {
                report.cancelled = true;
                break;
            }
            // (row number, filter, document)
            let mut docs: Vec<(usize, Document, Document)> = Vec::with_capacity(chunk.len());
            for (row, values) in chunk {
                let built = build_doc(columns, values.clone()).and_then(|d| json_doc(&d));
                let filter = key_idx
                    .iter()
                    .map(|&i| match &values[i] {
                        Value::Null => Err(anyhow!("key field \"{}\" is empty", columns[i])),
                        v => Ok((columns[i].clone(), json_to_bson(v)?)),
                    })
                    .collect::<Result<Document>>();
                match (built, filter) {
                    (Ok(d), Ok(f)) => docs.push((*row, f, d)),
                    (Err(e), _) | (_, Err(e)) => report.fail(*row, format!("{e:#}")),
                }
            }
            if opts.mode == ImportMode::Insert {
                if !docs.is_empty() {
                    match coll.insert_many(docs.iter().map(|(_, _, d)| d)).ordered(false).await {
                        Ok(_) => report.written += docs.len() as u64,
                        Err(e) => match *e.kind {
                            mongodb::error::ErrorKind::InsertMany(ref im) => {
                                let errors = im.write_errors.clone().unwrap_or_default();
                                report.written += (docs.len() - errors.len().min(docs.len())) as u64;
                                for w in errors {
                                    report.fail(docs.get(w.index).map(|d| d.0).unwrap_or(0), w.message);
                                }
                            }
                            _ => return Err(e.into()),
                        },
                    }
                }
            } else {
                let results: Vec<(usize, Result<()>)> = futures::stream::iter(docs)
                    .map(|(row, filter, doc)| {
                        let coll = coll.clone();
                        async move {
                            let res = coll.update_one(filter, doc! { "$set": doc }).upsert(true).await;
                            (row, res.map(|_| ()).map_err(anyhow::Error::from))
                        }
                    })
                    .buffer_unordered(16)
                    .collect()
                    .await;
                for (row, res) in results {
                    match res {
                        Ok(()) => report.written += 1,
                        Err(e) => report.fail(row, format!("{e:#}")),
                    }
                }
            }
            done += chunk.len();
            (ctx.progress)(done);
        }
        report.errors.sort_by_key(|e| e.row);
        Ok(report)
    }

    async fn describe(&self, path: &[String]) -> Result<ResultSet> {
        let [db, coll, ..] = path else { bail!("Select a collection") };
        let mut cursor = self.coll(db, coll).aggregate(vec![doc! { "$sample": { "size": 200 } }]).await?;
        let mut docs = Vec::new();
        while let Some(d) = cursor.try_next().await? {
            docs.push(bson_json(Bson::Document(d)));
        }
        Ok(infer_schema(&docs))
    }

    async fn close(&self) {
        self.client.clone().shutdown().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_chain() {
        let calls = parse_chain(".users.find({ age: { $gt: 3 } }).sort({_id: -1}).limit(5)").unwrap();
        let names: Vec<_> = calls.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["users", "find", "sort", "limit"]);
        assert_eq!(calls[3].args.as_deref(), Some("5"));
    }

    #[test]
    fn grid_values_keep_bson_types() {
        let oid = json_to_bson(&serde_json::json!("ObjectId('507f1f77bcf86cd799439011')")).unwrap();
        assert!(matches!(oid, Bson::ObjectId(_)));
        assert!(matches!(json_to_bson(&serde_json::json!("text ObjectId('507f1f77bcf86cd799439011')")).unwrap(), Bson::String(_)));
        let now = Bson::DateTime(mongodb::bson::DateTime::now());
        assert!(matches!(coerce_like(Some(&now), Bson::String("2024-05-01T10:00:00Z".into())), Bson::DateTime(_)));
        assert!(matches!(coerce_like(Some(&now), Bson::String("not a date".into())), Bson::String(_)));
        assert!(matches!(coerce_like(Some(&Bson::Int64(1)), Bson::Int32(5)), Bson::Int64(5)));
        assert!(matches!(coerce_like(Some(&Bson::Int32(1)), Bson::String("12".into())), Bson::Int32(12)));
        assert!(matches!(coerce_like(Some(&Bson::Double(1.0)), Bson::Int32(2)), Bson::Double(_)));
        assert!(matches!(coerce_like(None, Bson::String("x".into())), Bson::String(_)));
        let dec = Bson::Decimal128("1.5".parse().unwrap());
        assert!(matches!(coerce_like(Some(&dec), Bson::String("2.25".into())), Bson::Decimal128(_)));
    }

    #[test]
    fn parses_shell_helpers() {
        let args = parse_args(r#"{ _id: ObjectId("507f1f77bcf86cd799439011"), at: ISODate("2024-01-01") }, 3"#).unwrap();
        let Bson::Document(d) = &args[0] else { panic!() };
        assert!(matches!(d.get("_id"), Some(Bson::ObjectId(_))));
        assert!(matches!(d.get("at"), Some(Bson::DateTime(_))));
        assert_eq!(args.len(), 2);
    }

    #[test]
    fn splits_statements() {
        let parts = split_mongo("db.a.find({\n x: 1\n})\ndb.b.countDocuments()\n// c\nshow collections");
        assert_eq!(parts.len(), 3);
    }
}
