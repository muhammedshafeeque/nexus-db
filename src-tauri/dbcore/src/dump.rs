//! Dumps and restores through each engine's own tools (pg_dump / pg_restore,
//! mysqldump / mysql, mongodump / mongorestore, sqlite3). SQLite database
//! copies and SQL restores need no tool and run in-process.

use crate::auth;
use crate::drivers::mongo::MongoDriver;
use crate::model::*;
use crate::util::quote_ident;
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum DumpFormat {
    /// pg_dump custom archive, gzipped mongodump archive, SQLite database file.
    #[default]
    Native,
    /// Plain SQL script.
    Sql,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct DumpOptions {
    /// Database to dump; empty uses the connection's database.
    pub database: String,
    /// Output file.
    pub path: String,
    pub format: DumpFormat,
    /// Structure only, no rows.
    pub schema_only: bool,
    /// Folder with the client tools when they are not on PATH.
    pub tool_dir: String,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct RestoreOptions {
    /// Dump file to restore.
    pub path: String,
    /// Target database (for SQLite: the database file).
    pub database: String,
    /// Create the target database first; it must not exist yet.
    pub create: bool,
    /// Drop objects that are in the dump before recreating them.
    pub clean: bool,
    pub tool_dir: String,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ToolStatus {
    pub name: String,
    pub path: Option<String>,
    pub version: Option<String>,
}

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct DumpReport {
    pub path: String,
    pub bytes: u64,
    pub elapsed_ms: u64,
    pub log: Vec<String>,
}

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct RestoreReport {
    pub database: String,
    pub elapsed_ms: u64,
    /// Errors the tool reported but skipped (missing roles, existing objects, ...).
    pub warnings: usize,
    pub log: Vec<String>,
}

pub struct JobCtx<'a> {
    /// Called with every line the tool prints.
    pub log: &'a (dyn Fn(&str) + Send + Sync),
    pub cancel: &'a AtomicBool,
}

/// The client tools an engine needs, or `None` when dumps are not supported.
pub fn tool_names(engine: Engine) -> Option<&'static [&'static str]> {
    Some(match engine {
        Engine::Postgres => &["pg_dump", "pg_restore", "psql"],
        Engine::Mysql => &["mysqldump", "mysql"],
        Engine::Mongodb => &["mongodump", "mongorestore"],
        Engine::Sqlite => &["sqlite3"],
        _ => return None,
    })
}

pub async fn tools(engine: Engine, tool_dir: &str) -> Vec<ToolStatus> {
    let mut out = Vec::new();
    for name in tool_names(engine).unwrap_or_default() {
        let path = find_tool(name, tool_dir);
        let version = match &path {
            Some(p) => tool_version(p).await,
            None => None,
        };
        out.push(ToolStatus { name: name.to_string(), path: path.map(|p| p.display().to_string()), version });
    }
    out
}

pub async fn dump(cfg: &ConnectionConfig, driver: &dyn Driver, opts: &DumpOptions, ctx: &JobCtx<'_>) -> Result<DumpReport> {
    let started = Instant::now();
    let out = PathBuf::from(opts.path.trim());
    if out.as_os_str().is_empty() {
        bail!("Choose where to save the dump.");
    }
    let mut log = Vec::new();
    let db = database_name(cfg, &opts.database);
    let result = match cfg.engine {
        Engine::Postgres => {
            let mut cmd = command(&require_tool("pg_dump", &opts.tool_dir)?, pg_env(cfg).await?);
            cmd.env("PGDATABASE", &db).arg("--verbose").arg(format!("--file={}", out.display()));
            cmd.arg(if opts.format == DumpFormat::Sql { "--format=plain" } else { "--format=custom" });
            if opts.schema_only {
                cmd.arg("--schema-only");
            }
            run(cmd, None, None, ctx, &mut log).await
        }
        Engine::Mysql => {
            let tool = require_tool("mysqldump", &opts.tool_dir)?;
            let mut cmd = command(&tool, Vec::new());
            mysql_args(cfg, &tool, &mut cmd).await?;
            cmd.args(["--single-transaction", "--quick", "--routines", "--triggers", "--no-tablespaces", "--hex-blob", "--verbose"]);
            cmd.arg(format!("--result-file={}", out.display()));
            if opts.schema_only {
                cmd.arg("--no-data");
            }
            // no --databases: the script carries no CREATE DATABASE / USE, so it restores under any name
            cmd.arg("--").arg(&db);
            run(cmd, None, None, ctx, &mut log).await
        }
        Engine::Mongodb => {
            if opts.schema_only {
                bail!("MongoDB dumps always include the documents.");
            }
            let config = MongoConfig::new(cfg)?;
            let mut cmd = command(&require_tool("mongodump", &opts.tool_dir)?, Vec::new());
            cmd.arg(format!("--config={}", config.0.display()))
                .arg(format!("--db={db}"))
                .arg(format!("--archive={}", out.display()))
                .arg("--gzip");
            run(cmd, None, None, ctx, &mut log).await
        }
        Engine::Sqlite => {
            if opts.format == DumpFormat::Native && !opts.schema_only {
                // VACUUM INTO refuses to overwrite; the save dialog already asked
                let _ = std::fs::remove_file(&out);
                let lit = crate::util::sql_lit(&out.display().to_string());
                (ctx.log)(&format!("VACUUM INTO {lit}"));
                log.push(format!("VACUUM INTO {lit}"));
                driver.execute(&format!("VACUUM INTO {lit}"), &ExecContext::default()).await.map(|_| ())
            } else {
                let file = sqlite_file(cfg)?;
                let mut cmd = command(&require_tool("sqlite3", &opts.tool_dir)?, Vec::new());
                cmd.arg("-bail").arg(&file).arg(if opts.schema_only { ".schema" } else { ".dump" });
                run(cmd, None, Some(&out), ctx, &mut log).await
            }
        }
        e => bail!("Dumps are not supported for {e:?} yet."),
    };
    if let Err(e) = result {
        let _ = std::fs::remove_file(&out);
        return Err(e);
    }
    let bytes = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
    Ok(DumpReport { path: out.display().to_string(), bytes, elapsed_ms: started.elapsed().as_millis() as u64, log })
}

pub async fn restore(cfg: &ConnectionConfig, driver: &dyn Driver, opts: &RestoreOptions, ctx: &JobCtx<'_>) -> Result<RestoreReport> {
    let started = Instant::now();
    let src = PathBuf::from(opts.path.trim());
    let head = read_head(&src).with_context(|| format!("reading {}", src.display()))?;
    let target = opts.database.trim().to_string();
    if target.is_empty() {
        bail!("Enter the database to restore into.");
    }
    let mut log = Vec::new();
    if cfg.engine == Engine::Sqlite {
        return restore_sqlite(cfg, &src, &head, opts, ctx, log, started).await;
    }
    let mut note = |line: String| {
        (ctx.log)(&line);
        log.push(line);
    };
    if opts.create {
        let sql = match cfg.engine {
            Engine::Postgres => format!("CREATE DATABASE {}", quote_ident(&target, '"')),
            Engine::Mysql => format!("CREATE DATABASE {}", quote_ident(&target, '`')),
            _ => String::new(), // MongoDB creates databases on first write
        };
        if !sql.is_empty() {
            note(sql.clone());
            driver.execute(&sql, &ExecContext::default()).await.context("creating the database")?;
        }
    }

    let mut warnings = 0;
    match cfg.engine {
        Engine::Postgres => {
            let env = pg_env(cfg).await?;
            if head.starts_with(b"PGDMP") {
                let mut cmd = command(&require_tool("pg_restore", &opts.tool_dir)?, env);
                cmd.args(["--verbose", "--no-owner", "--no-privileges"]).arg(format!("--dbname={}", pg_conninfo_db(&target)));
                if opts.clean {
                    cmd.args(["--clean", "--if-exists"]);
                }
                cmd.arg(&src);
                match run(cmd, None, None, ctx, &mut log).await {
                    Ok(()) => {}
                    // pg_restore exits 1 after errors it skipped over
                    Err(e) => match log.iter().rev().find_map(|l| ignored_errors(l)) {
                        Some(n) => warnings = n,
                        None => return Err(e),
                    },
                }
            } else {
                if head.starts_with(b"\x1f\x8b") || head.starts_with(b"PK") {
                    bail!("This file is compressed – use a plain SQL script or a pg_dump custom archive.");
                }
                let mut cmd = command(&require_tool("psql", &opts.tool_dir)?, env);
                cmd.env("PGDATABASE", &target).args(["--no-psqlrc", "--quiet", "-v", "ON_ERROR_STOP=0"]).arg(format!("--file={}", src.display()));
                if opts.clean {
                    note("Plain SQL scripts cannot be cleaned first – objects are created as the script says.".into());
                }
                run(cmd, None, None, ctx, &mut log).await?;
                warnings = log.iter().filter(|l| l.contains("ERROR:")).count();
            }
        }
        Engine::Mysql => {
            let tool = require_tool("mysql", &opts.tool_dir)?;
            let mut cmd = command(&tool, Vec::new());
            mysql_args(cfg, &tool, &mut cmd).await?;
            cmd.arg("--").arg(&target);
            run(cmd, Some(&src), None, ctx, &mut log).await?;
        }
        Engine::Mongodb => {
            let config = MongoConfig::new(cfg)?;
            let mut cmd = command(&require_tool("mongorestore", &opts.tool_dir)?, Vec::new());
            cmd.arg(format!("--config={}", config.0.display())).arg(format!("--archive={}", src.display()));
            if head.starts_with(b"\x1f\x8b") {
                cmd.arg("--gzip");
            }
            // every database in the archive lands in the target
            cmd.arg("--nsFrom=$db$.$coll$").arg(format!("--nsTo={target}.$coll$"));
            if opts.clean {
                cmd.arg("--drop");
            }
            run(cmd, None, None, ctx, &mut log).await?;
        }
        e => bail!("Restoring is not supported for {e:?} yet."),
    }
    Ok(RestoreReport { database: target, elapsed_ms: started.elapsed().as_millis() as u64, warnings, log })
}

async fn restore_sqlite(
    cfg: &ConnectionConfig,
    src: &Path,
    head: &[u8],
    opts: &RestoreOptions,
    ctx: &JobCtx<'_>,
    mut log: Vec<String>,
    started: Instant,
) -> Result<RestoreReport> {
    let target = PathBuf::from(opts.database.trim());
    let current = sqlite_file(cfg).ok().map(PathBuf::from);
    let exists = target.exists();
    if opts.create && exists {
        bail!("{} already exists – choose a new file.", target.display());
    }
    let mut note = |line: String| {
        (ctx.log)(&line);
        log.push(line);
    };
    if head.starts_with(b"SQLite format 3\0") {
        if exists {
            bail!("A database file can only be restored to a new file.");
        }
        note(format!("Copying {} → {}", src.display(), target.display()));
        tokio::fs::copy(src, &target).await?;
    } else {
        let script = tokio::fs::read_to_string(src).await.context("the dump is neither a SQLite database nor a SQL script")?;
        if current.as_deref() == Some(target.as_path()) && opts.clean {
            note("SQL scripts cannot be cleaned first – objects are created as the script says.".into());
        }
        note(format!("Running {} on {}", src.display(), target.display()));
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&target).create_if_missing(true))
            .await?;
        let result = sqlx::raw_sql(&script).execute(&pool).await;
        pool.close().await;
        if let Err(e) = result {
            if !exists {
                let _ = std::fs::remove_file(&target);
            }
            return Err(e.into());
        }
    }
    Ok(RestoreReport { database: target.display().to_string(), elapsed_ms: started.elapsed().as_millis() as u64, warnings: 0, log })
}

// ---------------------------------------------------------------- connection settings for the tools

fn database_name(cfg: &ConnectionConfig, requested: &str) -> String {
    let requested = requested.trim();
    if !requested.is_empty() {
        return requested.to_string();
    }
    if !cfg.database.trim().is_empty() {
        return cfg.database.trim().to_string();
    }
    let from_url = parse_url(cfg).ok().map(|u| u.path().trim_start_matches('/').to_string()).filter(|d| !d.is_empty());
    from_url.unwrap_or_else(|| if cfg.engine == Engine::Postgres { "postgres".into() } else { String::new() })
}

fn parse_url(cfg: &ConnectionConfig) -> Result<reqwest::Url> {
    reqwest::Url::parse(cfg.url.trim()).context("parsing the connection string")
}

fn decoded(s: &str) -> String {
    urlencoding::decode(s).map(|c| c.into_owned()).unwrap_or_else(|_| s.to_string())
}

/// A database name for `--dbname` that libpq won't read as a connection string.
fn pg_conninfo_db(db: &str) -> String {
    format!("dbname='{}'", db.replace('\\', "\\\\").replace('\'', "\\'"))
}

/// libpq settings as environment variables, so no password shows up in the process list.
async fn pg_env(cfg: &ConnectionConfig) -> Result<Vec<(&'static str, String)>> {
    let mut env = vec![("PGAPPNAME", "Nexus DB".to_string()), ("PGCONNECT_TIMEOUT", "15".to_string())];
    let mut user = cfg.user.clone();
    if cfg.has_url() {
        let url = parse_url(cfg)?;
        if let Some(h) = url.host_str() {
            env.push(("PGHOST", h.trim_matches(['[', ']']).to_string()));
        }
        if let Some(p) = url.port() {
            env.push(("PGPORT", p.to_string()));
        }
        user = decoded(url.username());
        if let Some(p) = url.password() {
            env.push(("PGPASSWORD", decoded(p)));
        }
        for (k, v) in url.query_pairs() {
            match &*k {
                "sslmode" => env.push(("PGSSLMODE", v.into_owned())),
                "sslrootcert" => env.push(("PGSSLROOTCERT", v.into_owned())),
                _ => {}
            }
        }
    } else {
        env.push(("PGHOST", cfg.host_or_default().to_string()));
        env.push(("PGPORT", cfg.port_or(5432).to_string()));
        if !cfg.password.is_empty() {
            env.push(("PGPASSWORD", cfg.password.clone()));
        }
        let mode = match (cfg.ssl, cfg.trust_cert) {
            (false, _) => "prefer",
            (true, true) => "require",
            (true, false) => "verify-full",
        };
        env.push(("PGSSLMODE", mode.to_string()));
    }
    if let Some(src) = auth::token_source(cfg)? {
        let token = src.token().await?;
        user = src.db_user(&user, &token)?;
        env.retain(|(k, _)| *k != "PGPASSWORD");
        env.push(("PGPASSWORD", token.secret));
        if !cfg.ssl && !cfg.has_url() {
            env.retain(|(k, _)| *k != "PGSSLMODE");
            env.push(("PGSSLMODE", "require".into()));
        }
    }
    if !user.is_empty() {
        env.push(("PGUSER", user));
    }
    Ok(env)
}

/// Host, user and TLS flags for mysql / mysqldump; the password goes in `MYSQL_PWD`.
async fn mysql_args(cfg: &ConnectionConfig, tool: &Path, cmd: &mut Command) -> Result<()> {
    let (mut host, mut port, mut user, mut password) =
        (cfg.host_or_default().to_string(), cfg.port_or(3306), cfg.user.clone(), cfg.password.clone());
    let mut ssl = cfg.ssl;
    if cfg.has_url() {
        let url = parse_url(cfg)?;
        host = url.host_str().unwrap_or("localhost").trim_matches(['[', ']']).to_string();
        port = url.port().unwrap_or(3306);
        user = decoded(url.username());
        password = url.password().map(decoded).unwrap_or_default();
        ssl = url.query_pairs().any(|(k, v)| k.eq_ignore_ascii_case("ssl-mode") && !v.eq_ignore_ascii_case("disabled"));
    }
    let entra = auth::token_source(cfg)?;
    if let Some(src) = &entra {
        let token = src.token().await?;
        user = src.db_user(&user, &token)?;
        password = token.secret;
        ssl = true;
        cmd.arg("--enable-cleartext-plugin");
    }
    // TCP like the app itself – "localhost" would otherwise mean the Unix socket
    cmd.args(["--protocol=TCP", "--default-character-set=utf8mb4"]).arg(format!("--host={host}")).arg(format!("--port={port}"));
    if !user.is_empty() {
        cmd.arg(format!("--user={user}"));
    }
    if !password.is_empty() {
        cmd.env("MYSQL_PWD", password);
    }
    let mariadb = tool_version(tool).await.is_some_and(|v| v.contains("MariaDB"));
    match (ssl, mariadb) {
        (false, _) => {}
        (true, true) => {
            cmd.arg("--ssl");
            if !cfg.trust_cert {
                cmd.arg("--ssl-verify-server-cert");
            }
        }
        (true, false) => {
            cmd.arg(if cfg.trust_cert { "--ssl-mode=REQUIRED" } else { "--ssl-mode=VERIFY_IDENTITY" });
        }
    }
    Ok(())
}

/// mongodump / mongorestore read the connection string from a private config
/// file, so the password is not on the command line. Removed on drop.
struct MongoConfig(PathBuf);

impl MongoConfig {
    fn new(cfg: &ConnectionConfig) -> Result<Self> {
        if cfg.auth.is_entra() {
            bail!("mongodump / mongorestore cannot sign in with Microsoft Entra ID – use a connection with a password.");
        }
        let path = std::env::temp_dir().join(format!("nexus-mongo-{}-{}.yaml", std::process::id(), unique()));
        let uri = MongoDriver::uri(cfg, false);
        let yaml = format!("uri: {}\n", serde_json::to_string(&uri)?);
        write_private(&path, &yaml)?;
        Ok(Self(path))
    }
}

impl Drop for MongoConfig {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn unique() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
}

fn write_private(path: &Path, contents: &str) -> Result<()> {
    use std::io::Write;
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(path)?.write_all(contents.as_bytes())?;
    Ok(())
}

fn sqlite_file(cfg: &ConnectionConfig) -> Result<String> {
    if cfg.has_url() {
        let u = cfg.url.trim();
        let rest = u.strip_prefix("sqlite://").or_else(|| u.strip_prefix("sqlite:")).unwrap_or(u);
        return Ok(rest.split('?').next().unwrap_or(rest).to_string());
    }
    match cfg.file_path.trim() {
        "" => bail!("This connection has no database file."),
        f => Ok(f.to_string()),
    }
}

fn read_head(path: &Path) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut buf = vec![0; 16];
    let n = std::fs::File::open(path)?.read(&mut buf)?;
    buf.truncate(n);
    Ok(buf)
}

/// `pg_restore: warning: errors ignored on restore: 3` → 3
fn ignored_errors(line: &str) -> Option<usize> {
    line.split("errors ignored on restore:").nth(1)?.trim().parse().ok()
}

// ---------------------------------------------------------------- finding and running tools

fn exe(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

/// Folders the installers use when they don't put the tools on PATH.
/// Versioned folders are tried newest first – pg_dump must not be older than the server.
fn extra_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let mut versioned = |parent: &str, suffix: &str| {
        let Ok(rd) = std::fs::read_dir(parent) else { return };
        let mut found: Vec<PathBuf> = rd.flatten().map(|e| e.path().join(suffix)).filter(|p| p.is_dir()).collect();
        found.sort_by_key(|p| std::cmp::Reverse(version_key(p)));
        dirs.extend(found);
    };
    if cfg!(windows) {
        versioned(r"C:\Program Files\PostgreSQL", "bin");
        versioned(r"C:\Program Files\MySQL", "bin");
        versioned(r"C:\Program Files\MariaDB", "bin");
        versioned(r"C:\Program Files\MongoDB\Tools", "bin");
    } else {
        versioned("/usr/lib/postgresql", "bin");
        versioned("/Applications/Postgres.app/Contents/Versions", "bin");
        for d in ["/opt/homebrew/bin", "/opt/homebrew/opt/libpq/bin", "/usr/local/bin", "/usr/local/opt/libpq/bin", "/usr/local/mysql/bin", "/snap/bin"] {
            dirs.push(d.into());
        }
    }
    dirs
}

fn version_key(p: &Path) -> Vec<u32> {
    let name = p.parent().and_then(|d| d.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    name.split(|c: char| !c.is_ascii_digit()).filter_map(|s| s.parse().ok()).collect()
}

pub fn find_tool(name: &str, tool_dir: &str) -> Option<PathBuf> {
    let file = exe(name);
    let mut dirs: Vec<PathBuf> = Vec::new();
    if !tool_dir.trim().is_empty() {
        dirs.push(tool_dir.trim().into());
    }
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    dirs.extend(extra_dirs());
    let found = dirs.into_iter().map(|d| d.join(&file)).find(|p| p.is_file());
    // newer MariaDB installs only ship mariadb-dump / mariadb
    found.or_else(|| match name {
        "mysqldump" => find_tool("mariadb-dump", tool_dir),
        "mysql" => find_tool("mariadb", tool_dir),
        _ => None,
    })
}

fn require_tool(name: &str, tool_dir: &str) -> Result<PathBuf> {
    find_tool(name, tool_dir).ok_or_else(|| {
        anyhow!("`{name}` was not found. Install the database's client tools, or set the folder that contains {} under Tools.", exe(name))
    })
}

async fn tool_version(path: &Path) -> Option<String> {
    let out = tokio::time::timeout(Duration::from_secs(5), Command::new(path).arg("--version").kill_on_drop(true).output()).await.ok()?.ok()?;
    let text = String::from_utf8_lossy(if out.stdout.is_empty() { &out.stderr } else { &out.stdout }).into_owned();
    text.lines().map(str::trim).find(|l| !l.is_empty()).map(str::to_string)
}

fn command(tool: &Path, env: Vec<(&'static str, String)>) -> Command {
    let mut cmd = Command::new(tool);
    cmd.envs(env).kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    cmd
}

/// Runs a tool to the end, streaming its stderr (and stdout, unless it goes to
/// `stdout_file`) into the log. Stops it when the job is cancelled.
async fn run(mut cmd: Command, stdin_file: Option<&Path>, stdout_file: Option<&Path>, ctx: &JobCtx<'_>, log: &mut Vec<String>) -> Result<()> {
    const KEEP: usize = 400;
    let shown = describe(&cmd, stdin_file, stdout_file);
    (ctx.log)(&shown);
    log.push(shown);

    cmd.stdin(match stdin_file {
        Some(p) => Stdio::from(std::fs::File::open(p).with_context(|| format!("opening {}", p.display()))?),
        None => Stdio::null(),
    });
    cmd.stdout(match stdout_file {
        Some(p) => Stdio::from(std::fs::File::create(p).with_context(|| format!("creating {}", p.display()))?),
        None => Stdio::piped(),
    });
    cmd.stderr(Stdio::piped());
    let mut child = cmd.spawn().with_context(|| format!("starting {}", cmd.as_std().get_program().to_string_lossy()))?;

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let mut readers = Vec::new();
    if let Some(out) = child.stdout.take() {
        let tx = tx.clone();
        readers.push(tokio::spawn(async move {
            let mut lines = BufReader::new(out).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                let _ = tx.send(l);
            }
        }));
    }
    if let Some(err) = child.stderr.take() {
        let tx = tx.clone();
        readers.push(tokio::spawn(async move {
            let mut lines = BufReader::new(err).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                let _ = tx.send(l);
            }
        }));
    }
    drop(tx);

    let mut tick = tokio::time::interval(Duration::from_millis(200));
    let status = loop {
        tokio::select! {
            status = child.wait() => break status?,
            Some(line) = rx.recv() => {
                (ctx.log)(&line);
                log.push(line);
                if log.len() > KEEP {
                    log.drain(1..log.len() - KEEP + 1);
                }
            }
            _ = tick.tick() => {
                if ctx.cancel.load(Ordering::Relaxed) {
                    let _ = child.kill().await;
                    bail!("Cancelled");
                }
            }
        }
    };
    for r in readers {
        let _ = r.await;
    }
    while let Ok(line) = rx.try_recv() {
        (ctx.log)(&line);
        log.push(line);
    }
    if !status.success() {
        let last = log.iter().rev().find(|l| !l.trim().is_empty() && l.as_str() != log[0]).cloned().unwrap_or_default();
        bail!("{} failed ({status}){}", cmd.as_std().get_program().to_string_lossy(), if last.is_empty() { String::new() } else { format!(": {last}") });
    }
    Ok(())
}

/// The command line as shown in the log (secrets are only ever in env vars / files).
fn describe(cmd: &Command, stdin_file: Option<&Path>, stdout_file: Option<&Path>) -> String {
    let std = cmd.as_std();
    let program = Path::new(std.get_program()).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut s = format!("$ {program}");
    for a in std.get_args() {
        let a = a.to_string_lossy();
        if a.contains(' ') {
            s.push_str(&format!(" '{a}'"));
        } else {
            s.push(' ');
            s.push_str(&a);
        }
    }
    if let Some(p) = stdin_file {
        s.push_str(&format!(" < {}", p.display()));
    }
    if let Some(p) = stdout_file {
        s.push_str(&format!(" > {}", p.display()));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pg_restore_summary() {
        assert_eq!(ignored_errors("pg_restore: warning: errors ignored on restore: 3"), Some(3));
        assert_eq!(ignored_errors("pg_restore: creating TABLE"), None);
    }

    #[test]
    fn quotes_pg_dbname() {
        assert_eq!(pg_conninfo_db("it's"), "dbname='it\\'s'");
    }
}
