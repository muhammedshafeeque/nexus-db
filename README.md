# Nexus DB

A fast desktop database client built with **Tauri 2 + Rust + React**. One app for
relational, document, graph, key-value, analytics and search databases.

| Engine | Driver | Query language | Also covers |
|---|---|---|---|
| PostgreSQL | sqlx | SQL | CockroachDB, TimescaleDB, Supabase, YugabyteDB, Redshift |
| MySQL | sqlx | SQL | MariaDB, TiDB |
| SQLite | sqlx (bundled) | SQL | |
| SQL Server | tiberius | T-SQL | Azure SQL |
| MongoDB | mongodb 3 | mongo-shell syntax | |
| ArangoDB | HTTP API | AQL | |
| Redis | redis-rs | Redis commands | Valkey, KeyDB, Dragonfly |
| ClickHouse | HTTP API | SQL | |
| Elasticsearch | HTTP API | Kibana console syntax | OpenSearch |

All TLS goes through rustls, so OpenSSL is not needed.

## Install

**apt.** From a built release binary:

```bash
./packaging/build-apt-repo.sh
sudo apt install ./apt-repo/pool/main/n/nexus-db/nexus-db_*_amd64.deb
```

To publish the repository, serve the `apt-repo/` directory over HTTPS and install `apt-repo/nexus-db.sources` into `/etc/apt/sources.list.d/`. Replace `Trusted: yes` with a signed `Release` file before offering it to other people. Set `MAINTAINER="Your Name <you@example.com>"` when you build it.

**Snap.** The name `nexus-db` is free in the store. Register it at https://snapcraft.io/snaps, then:

```bash
sudo snap install snapcraft --classic
./packaging/build-snap.sh
snapcraft login
snapcraft upload --release=stable nexus-db_*_amd64.snap
```

This machine is not logged into the Snap Store, so the upload step has to be done from your Ubuntu One account.

## Features

- Lazy schema explorer (databases → schemas → tables/collections → columns/indexes)
- Multi-tab query editor (CodeMirror) with per-engine highlighting, SQL autocomplete
  from the loaded schema, `Ctrl+Enter` to run the selection or the whole script
- Virtualized results grid: sorting, filtering, column resizing, keyboard navigation,
  typed cells (NULL, booleans, numbers, dates, ObjectIds, JSON), value inspector
- JSON view for document results; export to CSV / JSON / **Excel (.xlsx)**, or copy as CSV / JSON / Markdown
- **Inline editing** of tables opened from the tree: edit cells, add / duplicate / delete rows, preview the
  statements, save with `Ctrl+S`. SQL engines save in one transaction and refuse changes whose row was
  changed or deleted meanwhile; MongoDB / ArangoDB / Elasticsearch save change by change. Tables need a
  primary key; ClickHouse and Redis are not edited in the grid.
- **Search** box in every database, schema and table list of the tree (Redis keys are searched on the server)
- **Import** from CSV / TSV / Excel (.xlsx, .xls, .xlsb) / ODS: target table picker (or new collection),
  column selection and column → column / key mapping with auto-matching, insert or upsert on chosen key
  columns, all-or-nothing transactions or skip-bad-rows, row-level error report, progress and cancel
- **Column filters** in the results grid: a filter box per column (`text`, `!text`, `=x`, `>10`, `^start`, `end$`, `/regex/`, `null`, `a | b`)
- **Database documentation** (PostgreSQL, MySQL / MariaDB, SQL Server, SQLite): every table and view with columns, types,
  defaults, comments, primary / unique / foreign keys, indexes and both directions of each relation; export as HTML or
  Markdown with a Mermaid ER diagram
- **Dump / restore** (PostgreSQL, MySQL / MariaDB, MongoDB, SQLite) through `pg_dump` / `pg_restore` / `psql`, `mysqldump` /
  `mysql`, `mongodump` / `mongorestore` and `sqlite3`, into a new or an existing database
- Several result sets per script, row-limit control, query history
- Connection manager: parameters or connection string, test button, SSL options,
  groups, colors, **read-only mode** (blocks statements that change data)
- **Microsoft Entra ID (Azure AD) sign-in** for every networked engine – see below
- Command palette (`Ctrl+K`), dark and light themes

## Microsoft Entra ID

Pick a method under **Authentication** in the connection dialog:

| Method | Use it for |
|---|---|
| Interactive (browser) | Your work account, incl. MFA and Conditional Access (auth code + PKCE, `http://localhost` redirect) |
| Device code | Same, signing in on another device (`microsoft.com/devicelogin`) |
| Azure CLI | Reuse `az login` |
| Service principal (secret / certificate) | App registrations; certificate = PEM with RSA key (convert PFX with `openssl pkcs12 -nodes`) |
| Managed identity | When Nexus DB runs on an Azure VM, App Service, Container Apps, AKS or Arc server |
| Access token | A token obtained elsewhere |

| Engine | How the token is used | Default scope |
|---|---|---|
| SQL Server / Azure SQL | TDS federated auth, encryption forced | `https://database.windows.net/.default` |
| PostgreSQL (Azure Database for PostgreSQL) | password; user = Entra user / group / identity name (blank = signed-in user), TLS forced | `https://ossrdbms-aad.database.windows.net/.default` |
| MySQL (Azure Database for MySQL) | cleartext-plugin password over TLS; user as above | same as PostgreSQL |
| MongoDB (Cosmos DB for MongoDB vCore, OIDC servers) | `MONGODB-OIDC` with a token callback | same as PostgreSQL |
| Redis (Azure Cache for Redis / Managed Redis) | `AUTH <object id> <token>` over TLS, re-AUTH before expiry | `https://redis.azure.com/.default` |
| ClickHouse, Elasticsearch / OpenSearch, ArangoDB | `Authorization: Bearer` – for servers / gateways that validate Entra JWTs | none – enter one |
| SQLite | not applicable (local file) | |

US Government and China clouds are supported (Cloud selector). The Azure CLI public client is
used unless you enter your own Client ID – if your tenant blocks it, register a public client app
with a `http://localhost` *Mobile and desktop* redirect URI.

Tokens are refreshed ~5 minutes before they expire; pooled engines (PostgreSQL, MySQL) open new
connections with the fresh token and Redis connections re-authenticate. Refresh tokens are kept in
`entra-tokens.json` (owner-only permissions) next to `connections.json`, so you are not asked to
sign in on every start; *Sign out / use another account* in the dialog forgets them. Entra errors
(`AADSTS…`) are shown with a short explanation of what to fix.

## Setup

**Linux** (Debian/Ubuntu) system libraries:

```bash
sudo apt install libwebkit2gtk-4.1-dev build-essential curl wget file \
  libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev pkg-config
```

Rust (`rustup`) and Node 20+ are also needed. On macOS and Windows, see https://tauri.app/start/prerequisites/.

```bash
npm install
npm run tauri dev      # run the desktop app
npm run tauri build    # build release installers
```

`npm run dev` on its own runs the UI in a browser with **demo data** (no Tauri), which helps when working on the UI.

## Query cheat-sheet

```js
// MongoDB – one statement per line, or separated by ;
db.users.find({ age: { $gt: 21 } }, { name: 1 }).sort({ _id: -1 }).limit(20)
db.orders.aggregate([{ $group: { _id: "$status", n: { $sum: 1 } } }])
db.users.updateOne({ _id: ObjectId("...") }, { $set: { active: true } })
db.runCommand({ dbStats: 1 })
show collections
```

```text
# Elasticsearch / OpenSearch
GET /my-index/_search
{ "query": { "match": { "title": "rust" } } }

# Redis – one command per line
HGETALL user:42
ZRANGE leaderboard 0 9 WITHSCORES
```

## Architecture

```
src/                     React UI (zustand store, components/, lib/engines.ts)
src-tauri/src/lib.rs     Tauri commands, connection store, live connection pool
src-tauri/dbcore/        Driver layer – every engine implements the `Driver` trait
  src/model.rs           ConnectionConfig, TreeNode, ResultSet, Driver trait
  src/drivers/*.rs       postgres, mysql, sqlite, mssql, mongo, arango, redis, clickhouse, elastic
```

To add an engine, implement `Driver` (`server_info`, `list_children`, `execute`,
`object_query`, `describe`), add it to `Engine` and to `dbcore::connect`, then add a
flavor in `src/lib/engines.ts`.

Smoke-test a driver without the UI:

```bash
cd src-tauri/dbcore
cargo run --example smoke -- mongodb "mongodb://127.0.0.1:27017" "show dbs"
cargo test
```

## Notes

- Connections are saved to `connections.json` in the app config directory.
  **Passwords are stored in plain text** in that file. Moving them to the OS keychain
  (e.g. the `keyring` crate) is the next thing to add.
- Not implemented yet: SSH tunnels, cancelling a running query, editing cells
- Entra refresh tokens are stored like passwords: in a plain file (owner-only permissions), not the OS keychain.
  in the grid, Oracle/Cassandra/DuckDB drivers.
