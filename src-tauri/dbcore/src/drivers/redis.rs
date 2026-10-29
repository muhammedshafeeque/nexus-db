use crate::auth::{self, Token, TokenSource};
use crate::model::*;
use crate::util::{bytes_value, obj};
use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use redis::aio::MultiplexedConnection;
use redis::{IntoConnectionInfo, Value as RV};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Redis, Valkey, KeyDB, Dragonfly, Azure Cache for Redis.
pub struct RedisDriver {
    cfg: ConnectionConfig,
    client: std::sync::Mutex<redis::Client>,
    default_db: i64,
    conns: Mutex<HashMap<i64, MultiplexedConnection>>,
    entra: Option<Arc<TokenSource>>,
    /// Token the client and live connections are authenticated with.
    entra_token: Mutex<String>,
}

/// Commands whose flat RESP2 reply is really a list of pairs.
const PAIRED: &[&str] = &["HGETALL", "CONFIG", "HRANDFIELD"];

impl RedisDriver {
    pub async fn connect(cfg: &ConnectionConfig) -> Result<Self> {
        let entra = auth::token_source(cfg)?;
        let (client, token) = match &entra {
            Some(src) => {
                let token = src.token().await?;
                (Self::client(cfg, Some(&token))?, token.secret)
            }
            None => (Self::client(cfg, None)?, String::new()),
        };
        let default_db = cfg.database.trim().trim_start_matches("db").parse().unwrap_or(0);
        let driver = Self {
            cfg: cfg.clone(),
            client: std::sync::Mutex::new(client),
            default_db,
            conns: Mutex::default(),
            entra,
            entra_token: Mutex::new(token),
        };
        driver.conn(default_db).await.context("connecting to Redis")?;
        Ok(driver)
    }

    /// With an Entra token: user = configured name or the identity's object ID,
    /// password = token. Azure only serves Entra auth over TLS.
    fn client(cfg: &ConnectionConfig, token: Option<&Token>) -> Result<redis::Client> {
        let url = if cfg.has_url() {
            cfg.url.trim().to_string()
        } else {
            let auth = match (cfg.user.is_empty(), cfg.password.is_empty()) {
                _ if token.is_some() => String::new(),
                (true, true) => String::new(),
                (true, false) => format!(":{}@", urlencoding::encode(&cfg.password)),
                _ => format!("{}:{}@", urlencoding::encode(&cfg.user), urlencoding::encode(&cfg.password)),
            };
            let tls = cfg.ssl || token.is_some();
            let scheme = if tls { "rediss" } else { "redis" };
            let insecure = if tls && cfg.trust_cert { "/#insecure" } else { "" };
            format!("{scheme}://{auth}{}:{}{insecure}", cfg.host_or_default(), cfg.port_or(6379))
        };
        let mut info = url.into_connection_info()?;
        if let Some(token) = token {
            info.redis.username = Some(Self::entra_user(cfg, info.redis.username.take(), token)?);
            info.redis.password = Some(token.secret.clone());
        }
        Ok(redis::Client::open(info)?)
    }

    fn entra_user(cfg: &ConnectionConfig, url_user: Option<String>, token: &Token) -> Result<String> {
        Some(cfg.user.trim().to_string())
            .filter(|u| !u.is_empty())
            .or(url_user.filter(|u| !u.is_empty()))
            .or_else(|| token.object_id())
            .ok_or_else(|| anyhow!("Enter the object ID of the identity as the Redis user."))
    }

    /// Azure closes connections whose token expires, so re-AUTH them with each new token.
    async fn authenticate(&self) -> Result<()> {
        let Some(src) = &self.entra else { return Ok(()) };
        let token = src.token().await?;
        let mut current = self.entra_token.lock().await;
        if *current == token.secret {
            return Ok(());
        }
        let client = Self::client(&self.cfg, Some(&token))?;
        let user = client.get_connection_info().redis.username.clone().unwrap_or_default();
        *self.client.lock().unwrap_or_else(|e| e.into_inner()) = client;
        let mut conns = self.conns.lock().await;
        let mut stale = Vec::new();
        for (db, con) in conns.iter_mut() {
            let res: redis::RedisResult<RV> = redis::cmd("AUTH").arg(&user).arg(&token.secret).query_async(con).await;
            if res.is_err() {
                stale.push(*db);
            }
        }
        for db in stale {
            conns.remove(&db);
        }
        *current = token.secret;
        Ok(())
    }

    async fn conn(&self, db: i64) -> Result<MultiplexedConnection> {
        let mut conns = self.conns.lock().await;
        if let Some(c) = conns.get(&db) {
            return Ok(c.clone());
        }
        let client = self.client.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let mut con = client.get_multiplexed_async_connection().await?;
        if db != 0 {
            let _: RV = redis::cmd("SELECT").arg(db).query_async(&mut con).await?;
        }
        conns.insert(db, con.clone());
        Ok(con)
    }

    fn db_index(&self, s: Option<&str>) -> i64 {
        s.and_then(|s| s.trim_start_matches("db").parse().ok()).unwrap_or(self.default_db)
    }

    async fn cmd(&self, db: i64, args: &[String]) -> Result<RV> {
        self.authenticate().await?;
        let mut con = self.conn(db).await?;
        let mut cmd = redis::cmd(&args[0]);
        for a in &args[1..] {
            cmd.arg(a);
        }
        match cmd.query_async(&mut con).await {
            Ok(v) => Ok(v),
            // idle timeout, failover, expired token: reconnect on the next command
            // (no automatic retry – the command may already have run)
            Err(e) if e.is_connection_dropped() || e.is_io_error() => {
                self.conns.lock().await.remove(&db);
                bail!("{e} – the connection was reset, run it again")
            }
            Err(e) => Err(e.into()),
        }
    }

    async fn key_type(&self, db: i64, key: &str) -> Result<String> {
        match self.cmd(db, &["TYPE".into(), key.into()]).await? {
            RV::SimpleString(s) => Ok(s),
            RV::Okay => Ok("ok".into()),
            other => Ok(format!("{other:?}")),
        }
    }
}

fn placeholders(template: &str) -> Vec<String> {
    template.split('{').skip(1).filter_map(|p| p.split_once('}').map(|(name, _)| name.to_string())).collect()
}

fn text(v: &Value) -> Option<String> {
    match v {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        other => Some(other.to_string()),
    }
}

/// `HSET <key from template> field value …`, or `None` when nothing is set.
fn hash_cmd(template: &str, columns: &[String], values: &[Value]) -> Result<Option<redis::Cmd>> {
    let mut key = template.to_string();
    for name in placeholders(template) {
        let i = columns.iter().position(|c| c == &name).unwrap_or(usize::MAX);
        let v = values.get(i).and_then(text).ok_or_else(|| anyhow!("{{{name}}} is empty, so the key cannot be built"))?;
        key = key.replace(&format!("{{{name}}}"), &v);
    }
    let mut cmd = redis::cmd("HSET");
    cmd.arg(&key);
    let mut any = false;
    for (c, v) in columns.iter().zip(values) {
        if let Some(t) = text(v) {
            cmd.arg(c).arg(t);
            any = true;
        }
    }
    Ok(any.then_some(cmd))
}

fn rv(v: RV) -> Value {
    match v {
        RV::Nil => Value::Null,
        RV::Int(i) => Value::from(i),
        RV::BulkString(b) => bytes_value(&b),
        RV::SimpleString(s) => Value::String(s),
        RV::Okay => Value::String("OK".into()),
        RV::Array(items) | RV::Set(items) => Value::Array(items.into_iter().map(rv).collect()),
        RV::Map(pairs) => Value::Object(
            pairs
                .into_iter()
                .map(|(k, v)| (rv(k).as_str().map(str::to_string).unwrap_or_default(), rv(v)))
                .collect(),
        ),
        RV::Double(f) => serde_json::Number::from_f64(f).map(Value::Number).unwrap_or(Value::Null),
        RV::Boolean(b) => Value::Bool(b),
        RV::VerbatimString { text, .. } => Value::String(text),
        other => Value::String(format!("{other:?}")),
    }
}

fn reply_to_set(cmd: &str, args: &[String], v: RV) -> ResultSet {
    let paired = PAIRED.contains(&cmd)
        || (cmd.starts_with('Z') && args.iter().any(|a| a.eq_ignore_ascii_case("WITHSCORES")));
    match rv(v) {
        Value::Array(items) if paired => ResultSet::table(
            vec![Column::new("field", ""), Column::new("value", "")],
            items.chunks(2).map(|c| vec![c[0].clone(), c.get(1).cloned().unwrap_or(Value::Null)]).collect(),
        ),
        Value::Array(items) => ResultSet::table(
            vec![Column::new("#", "int"), Column::new("value", "")],
            items.into_iter().enumerate().map(|(i, v)| vec![Value::from(i as u64), v]).collect(),
        ),
        Value::Object(map) => ResultSet::table(
            vec![Column::new("field", ""), Column::new("value", "")],
            map.into_iter().map(|(k, v)| vec![Value::String(k), v]).collect(),
        ),
        // INFO-style replies: "key:value" lines
        Value::String(s) if cmd == "INFO" => ResultSet::table(
            vec![Column::new("field", ""), Column::new("value", "")],
            s.lines()
                .filter_map(|l| l.split_once(':'))
                .map(|(k, v)| vec![Value::String(k.into()), Value::String(v.trim().into())])
                .collect(),
        ),
        other => ResultSet::table(vec![Column::new("result", "")], vec![vec![other]]),
    }
}

/// Shell-like tokenizer: whitespace separated, "double" (with escapes) or 'single' quotes.
fn tokenize(line: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let mut chars = line.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
            continue;
        }
        let mut tok = String::new();
        if c == '"' || c == '\'' {
            chars.next();
            let mut closed = false;
            while let Some(ch) = chars.next() {
                if ch == c {
                    closed = true;
                    break;
                }
                if ch == '\\' && c == '"' {
                    match chars.next() {
                        Some('n') => tok.push('\n'),
                        Some('t') => tok.push('\t'),
                        Some(o) => tok.push(o),
                        None => {}
                    }
                } else {
                    tok.push(ch);
                }
            }
            if !closed {
                bail!("Unterminated quote in: {line}");
            }
        } else {
            while let Some(&ch) = chars.peek() {
                if ch.is_whitespace() {
                    break;
                }
                tok.push(ch);
                chars.next();
            }
        }
        out.push(tok);
    }
    Ok(out)
}

fn quote_key(k: &str) -> String {
    if k.chars().any(|c| c.is_whitespace() || c == '"' || c == '\'') || k.is_empty() {
        format!("\"{}\"", k.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        k.to_string()
    }
}

#[async_trait]
impl Driver for RedisDriver {
    async fn server_info(&self) -> Result<ServerInfo> {
        let RV::BulkString(info) = self.cmd(self.default_db, &["INFO".into(), "server".into()]).await? else {
            return Ok(ServerInfo { version: "Redis".into() });
        };
        let info = String::from_utf8_lossy(&info);
        let get = |k: &str| info.lines().find_map(|l| l.strip_prefix(k)).map(|s| s.trim().to_string());
        let version = get("valkey_version:")
            .map(|v| format!("Valkey {v}"))
            .or_else(|| get("redis_version:").map(|v| format!("Redis {v}")))
            .unwrap_or_else(|| "Redis".into());
        Ok(ServerInfo { version })
    }

    async fn list_children(&self, path: &[String]) -> Result<Vec<TreeNode>> {
        match path {
            [] => {
                let count = match self.cmd(self.default_db, &["CONFIG".into(), "GET".into(), "databases".into()]).await {
                    Ok(v) => match rv(v) {
                        Value::Array(a) => a.get(1).and_then(|v| v.as_str()?.parse().ok()).unwrap_or(16),
                        Value::Object(m) => m.get("databases").and_then(|v| v.as_str()?.parse().ok()).unwrap_or(16),
                        _ => 16,
                    },
                    Err(_) => 16, // CONFIG is often disabled on managed services
                };
                let keyspace = match self.cmd(self.default_db, &["INFO".into(), "keyspace".into()]).await? {
                    RV::BulkString(b) => String::from_utf8_lossy(&b).to_string(),
                    _ => String::new(),
                };
                let keys: HashMap<String, String> = keyspace
                    .lines()
                    .filter_map(|l| {
                        let (db, rest) = l.split_once(':')?;
                        let n = rest.split(',').find_map(|p| p.strip_prefix("keys="))?;
                        Some((db.to_string(), n.to_string()))
                    })
                    .collect();
                Ok((0..count.min(64))
                    .map(|i| {
                        let name = format!("db{i}");
                        let detail = keys.get(&name).map(|n| format!("{n} keys")).unwrap_or_default();
                        TreeNode::new(path, name, "database", true).detail(detail)
                    })
                    .collect())
            }
            [db] => {
                let db = self.db_index(Some(db));
                let mut con = self.conn(db).await?;
                let mut keys: Vec<String> = Vec::new();
                let mut cursor: u64 = 0;
                loop {
                    let (next, batch): (u64, Vec<String>) = redis::cmd("SCAN")
                        .arg(cursor)
                        .arg("COUNT")
                        .arg(1000)
                        .query_async(&mut con)
                        .await?;
                    keys.extend(batch);
                    cursor = next;
                    if cursor == 0 || keys.len() >= 2000 {
                        break;
                    }
                }
                keys.sort();
                keys.dedup();
                let mut pipe = redis::pipe();
                for k in &keys {
                    pipe.cmd("TYPE").arg(k);
                }
                let types: Vec<String> = if keys.is_empty() { Vec::new() } else { pipe.query_async(&mut con).await? };
                Ok(keys
                    .into_iter()
                    .zip(types)
                    .map(|(k, t)| TreeNode::new(path, k, "key", false).detail(t))
                    .collect())
            }
            _ => Ok(Vec::new()),
        }
    }

    async fn execute(&self, query: &str, ctx: &ExecContext) -> Result<Vec<ResultSet>> {
        let db = self.db_index(ctx.database());
        let mut sets = Vec::new();
        for line in query.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with("//") {
                continue;
            }
            let args = tokenize(line)?;
            let cmd = args[0].to_ascii_uppercase();
            if cmd == "SELECT" {
                bail!("Use the database selector in the toolbar to switch databases");
            }
            let reply = self.cmd(db, &args).await.map_err(|e| anyhow!("{line}: {e}"))?;
            let mut rs = reply_to_set(&cmd, &args, reply);
            if rs.rows.len() > ctx.limit() {
                rs.rows.truncate(ctx.limit());
                rs.truncated = true;
            }
            sets.push(rs);
        }
        Ok(sets)
    }

    async fn search_children(&self, path: &[String], pattern: &str) -> Result<Vec<TreeNode>> {
        let [db] = path else {
            let p = pattern.to_lowercase();
            return Ok(self.list_children(path).await?.into_iter().filter(|n| n.name.to_lowercase().contains(&p)).collect());
        };
        // the tree only lists the first keys – search the whole keyspace
        let db = self.db_index(Some(db));
        let mut con = self.conn(db).await?;
        let glob: String = pattern.chars().map(|c| if "*?[]\\".contains(c) { format!("\\{c}") } else { c.to_string() }).collect();
        let mut keys: Vec<String> = Vec::new();
        let mut cursor: u64 = 0;
        loop {
            let (next, batch): (u64, Vec<String>) =
                redis::cmd("SCAN").arg(cursor).arg("MATCH").arg(format!("*{glob}*")).arg("COUNT").arg(1000).query_async(&mut con).await?;
            keys.extend(batch);
            cursor = next;
            if cursor == 0 || keys.len() >= 2000 {
                break;
            }
        }
        keys.sort();
        keys.dedup();
        let mut pipe = redis::pipe();
        for k in &keys {
            pipe.cmd("TYPE").arg(k);
        }
        let types: Vec<String> = if keys.is_empty() { Vec::new() } else { pipe.query_async(&mut con).await? };
        Ok(keys.into_iter().zip(types).map(|(k, t)| TreeNode::new(path, k, "key", false).detail(t)).collect())
    }

    async fn edit_info(&self, _path: &[String]) -> Result<EditInfo> {
        Ok(EditInfo {
            documents: true,
            can_upsert: true,
            reason: Some("Edit Redis values with commands (SET, HSET …). Imports write one hash per row.".into()),
            ..Default::default()
        })
    }

    /// One hash per row; the key comes from a template such as `user:{id}`.
    async fn import_rows(
        &self,
        path: &[String],
        columns: &[String],
        rows: Vec<ImportRow>,
        opts: &ImportOptions,
        ctx: &ImportCtx<'_>,
    ) -> Result<ImportReport> {
        let db = self.db_index(path.first().map(String::as_str));
        let template = opts.key_template.trim();
        if !template.contains('{') {
            bail!("Enter a key template with a column placeholder, e.g. user:{{id}} – otherwise every row would write the same key.");
        }
        for name in placeholders(template) {
            if !columns.contains(&name) {
                bail!("The key template uses {{{name}}}, which is not a mapped column.");
            }
        }
        self.authenticate().await?;
        let mut con = self.conn(db).await?;
        let mut report = ImportReport::default();
        let mut done = 0;
        for chunk in rows.chunks(opts.batch(1000)) {
            if ctx.cancelled() {
                report.cancelled = true;
                break;
            }
            let mut cmds: Vec<(usize, redis::Cmd)> = Vec::new();
            for (row, values) in chunk {
                match hash_cmd(template, columns, values) {
                    Ok(Some(cmd)) => cmds.push((*row, cmd)),
                    Ok(None) => report.fail(*row, "all mapped cells are empty"),
                    Err(e) => report.fail(*row, format!("{e:#}")),
                }
            }
            let mut pipe = redis::pipe();
            for (_, c) in &cmds {
                pipe.add_command(c.clone()).ignore();
            }
            let batch: redis::RedisResult<()> = if cmds.is_empty() { Ok(()) } else { pipe.query_async(&mut con).await };
            match batch {
                Ok(()) => report.written += cmds.len() as u64,
                // one bad key (WRONGTYPE …) fails the pipeline – retry one by one
                Err(_) => {
                    for (row, c) in cmds {
                        match c.query_async::<RV>(&mut con).await {
                            Ok(_) => report.written += 1,
                            Err(e) => report.fail(row, e.to_string()),
                        }
                    }
                }
            }
            done += chunk.len();
            (ctx.progress)(done);
        }
        Ok(report)
    }

    async fn object_query(&self, path: &[String]) -> Result<ObjectQuery> {
        let [db, key, ..] = path else { bail!("Select a key") };
        let ty = self.key_type(self.db_index(Some(db)), key).await?;
        let k = quote_key(key);
        let query = match ty.as_str() {
            "string" => format!("GET {k}"),
            "hash" => format!("HGETALL {k}"),
            "list" => format!("LRANGE {k} 0 199"),
            "set" => format!("SSCAN {k} 0 COUNT 200"),
            "zset" => format!("ZRANGE {k} 0 199 WITHSCORES"),
            "stream" => format!("XRANGE {k} - + COUNT 100"),
            "ReJSON-RL" => format!("JSON.GET {k}"),
            _ => format!("TYPE {k}"),
        };
        Ok(ObjectQuery { query: format!("{query}\nTTL {k}"), database: Some(db.clone()), schema: None })
    }

    async fn describe(&self, path: &[String]) -> Result<ResultSet> {
        let [db, key, ..] = path else { bail!("Select a key") };
        let db = self.db_index(Some(db));
        let mut rows = Vec::new();
        for (label, args) in [
            ("type", vec!["TYPE"]),
            ("ttl (s)", vec!["TTL"]),
            ("encoding", vec!["OBJECT", "ENCODING"]),
            ("memory (bytes)", vec!["MEMORY", "USAGE"]),
        ] {
            let mut a: Vec<String> = args.into_iter().map(String::from).collect();
            a.push(key.clone());
            let v = self.cmd(db, &a).await.map(rv).unwrap_or(Value::Null);
            rows.push(obj(vec![("property", Value::String(label.into())), ("value", v)]));
        }
        Ok(crate::util::docs_to_resultset(rows))
    }
}

#[cfg(test)]
mod tests {
    use super::{hash_cmd, placeholders};
    use serde_json::Value;

    #[test]
    fn import_keys_from_template() {
        let cols = vec!["id".to_string(), "name".to_string(), "note".to_string()];
        let cmd = hash_cmd("user:{id}", &cols, &[serde_json::json!(7), serde_json::json!("Ada"), Value::Null]).unwrap().unwrap();
        let args: Vec<String> = cmd.args_iter().map(|a| match a { redis::Arg::Simple(b) => String::from_utf8_lossy(b).into_owned(), _ => String::new() }).collect();
        assert_eq!(args, vec!["HSET", "user:7", "id", "7", "name", "Ada"]);
        assert!(hash_cmd("user:{id}", &cols, &[Value::Null, serde_json::json!("x"), Value::Null]).is_err());
        assert_eq!(placeholders("{a}:{b}:x"), vec!["a", "b"]);
    }

    #[test]
    fn tokenizes_quotes() {
        let t = super::tokenize(r#"SET "my key" 'a b' plain"#).unwrap();
        assert_eq!(t, ["SET", "my key", "a b", "plain"]);
    }
}
