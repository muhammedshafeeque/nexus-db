//! Unified driver layer: every engine implements [`Driver`], so the app talks
//! to Postgres, MongoDB or Redis through the same five calls.

pub mod auth;
pub mod model;
pub mod tabular;
pub mod util;

pub mod drivers {
    pub mod arango;
    pub mod clickhouse;
    pub mod elastic;
    pub mod http;
    pub mod mongo;
    pub mod mssql;
    pub mod mysql;
    pub mod postgres;
    pub mod redis;
    pub mod sqlcommon;
    pub mod sqlite;
}

pub use model::*;

use anyhow::Result;
use std::sync::{Arc, Once};

/// rustls 0.23 needs a process-wide crypto provider.
pub fn init() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

pub async fn connect(cfg: &ConnectionConfig) -> Result<Arc<dyn Driver>> {
    init();
    // rejects unusable Entra settings (e.g. on SQLite) before any driver runs
    auth::token_source(cfg)?;
    use drivers::*;
    Ok(match cfg.engine {
        Engine::Postgres => Arc::new(postgres::PgDriver::connect(cfg).await?),
        Engine::Mysql => Arc::new(mysql::MySqlDriver::connect(cfg).await?),
        Engine::Sqlite => Arc::new(sqlite::SqliteDriver::connect(cfg).await?),
        Engine::Mssql => Arc::new(mssql::MssqlDriver::connect(cfg).await?),
        Engine::Mongodb => Arc::new(mongo::MongoDriver::connect(cfg).await?),
        Engine::Arangodb => Arc::new(arango::ArangoDriver::connect(cfg).await?),
        Engine::Redis => Arc::new(redis::RedisDriver::connect(cfg).await?),
        Engine::Clickhouse => Arc::new(clickhouse::ClickHouseDriver::connect(cfg).await?),
        Engine::Elasticsearch => Arc::new(elastic::ElasticDriver::connect(cfg).await?),
    })
}

/// Best-effort guard for read-only connections (SQL engines and Mongo).
pub fn is_mutating(engine: Engine, query: &str) -> bool {
    match engine {
        Engine::Mongodb => {
            let q = query.to_ascii_lowercase();
            ["insert", "update", "delete", "replace", ".drop(", "createindex", "createcollection", "dropdatabase"]
                .iter()
                .any(|k| q.contains(k))
        }
        Engine::Redis | Engine::Elasticsearch | Engine::Arangodb => {
            let q = query.to_ascii_uppercase();
            match engine {
                Engine::Redis => q.lines().any(|l| {
                    let c = l.split_whitespace().next().unwrap_or("");
                    matches!(c, "SET" | "DEL" | "HSET" | "LPUSH" | "RPUSH" | "SADD" | "ZADD" | "EXPIRE" | "FLUSHDB"
                        | "FLUSHALL" | "RENAME" | "HDEL" | "SREM" | "ZREM" | "LPOP" | "RPOP" | "INCR" | "DECR"
                        | "MSET" | "UNLINK" | "XADD" | "SETEX" | "APPEND" | "PERSIST")
                }),
                Engine::Elasticsearch => q.lines().any(|l| {
                    let l = l.trim_start();
                    l.starts_with("PUT") || l.starts_with("DELETE") || (l.starts_with("POST") && !l.contains("_SEARCH"))
                }),
                _ => ["INSERT", "UPDATE", "REMOVE", "REPLACE", "UPSERT"].iter().any(|k| q.contains(k)),
            }
        }
        _ => util::split_statements(query).iter().any(|s| {
            matches!(
                util::first_keyword(s).as_str(),
                "INSERT" | "UPDATE" | "DELETE" | "DROP" | "ALTER" | "CREATE" | "TRUNCATE" | "MERGE" | "GRANT"
                    | "REVOKE" | "REPLACE" | "RENAME" | "UPSERT" | "OPTIMIZE" | "VACUUM" | "EXEC" | "EXECUTE" | "CALL"
            )
        }),
    }
}
