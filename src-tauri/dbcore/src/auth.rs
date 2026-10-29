//! Microsoft Entra ID (Azure AD) sign-in shared by every driver.
//!
//! [`token_source`] turns a connection's auth settings into a [`TokenSource`].
//! Drivers call [`TokenSource::token`] whenever they open a connection and get
//! a cached token, a silently refreshed one, or – for the interactive methods –
//! a browser / device-code sign-in. Refresh tokens survive restarts in a small
//! cache file (see [`set_cache_path`]); access tokens only live in memory.

use crate::model::{AuthKind, ConnectionConfig, Engine};
use anyhow::{anyhow, bail, Context, Result};
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine as _;
use ring::digest::{digest, SHA1_FOR_LEGACY_USE_ONLY, SHA256};
use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{RsaKeyPair, RSA_PKCS1_SHA256};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, Notify};

/// Azure CLI's public client. It is pre-authorized for Azure SQL, the Azure
/// OSS databases and Azure Cache for Redis, allows `http://localhost`
/// redirects on any port and supports the device-code flow.
pub const AZURE_CLI_CLIENT_ID: &str = "04b07795-8ddb-461a-bbee-02f9e1bf7b46";

/// Tokens are renewed this long before they expire (clock skew, long statements).
const REFRESH_MARGIN: i64 = 5 * 60;
const LOGIN_TIMEOUT: Duration = Duration::from_secs(5 * 60);

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Numbers that some endpoints send as strings (`"expires_in": "3599"`).
fn lenient_i64(v: &Value) -> Option<i64> {
    v.as_i64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

fn enc(s: &str) -> String {
    urlencoding::encode(s).into_owned()
}

fn random_b64(len: usize) -> String {
    let mut buf = vec![0u8; len];
    let _ = SystemRandom::new().fill(&mut buf);
    URL_SAFE_NO_PAD.encode(buf)
}

// ---------------------------------------------------------------- tokens

#[derive(Clone, Debug)]
pub struct Token {
    pub secret: String,
    /// Unix seconds.
    pub expires_at: i64,
    /// Decoded JWT payload (not verified – the database does that), or null.
    pub claims: Value,
}

impl Token {
    fn new(secret: String, expires_in: Option<i64>) -> Self {
        let claims = jwt_claims(&secret).unwrap_or(Value::Null);
        let expires_at = claims
            .get("exp")
            .and_then(lenient_i64)
            .or_else(|| expires_in.map(|s| now() + s))
            .unwrap_or_else(|| now() + 3600);
        Self { secret, expires_at, claims }
    }

    pub fn is_fresh(&self) -> bool {
        self.expires_at - now() > REFRESH_MARGIN
    }

    pub fn expires_instant(&self) -> Instant {
        Instant::now() + Duration::from_secs((self.expires_at - now()).max(0) as u64)
    }

    fn claim(&self, name: &str) -> Option<String> {
        self.claims.get(name).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string)
    }

    /// Sign-in name of a user token; `None` for apps and managed identities.
    pub fn principal_name(&self) -> Option<String> {
        ["upn", "preferred_username", "unique_name", "email"].iter().find_map(|c| self.claim(c))
    }

    pub fn object_id(&self) -> Option<String> {
        self.claim("oid")
    }
}

fn jwt_claims(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
    serde_json::from_slice(&bytes).ok()
}

// ---------------------------------------------------------------- errors

/// An OAuth error from Entra ID, with a plain-language hint where we know one.
#[derive(Debug)]
pub struct EntraError {
    pub error: String,
    pub description: String,
    pub codes: Vec<i64>,
}

impl EntraError {
    fn from_body(body: &Value, status: impl fmt::Display) -> Self {
        let description = body.get("error_description").and_then(Value::as_str).unwrap_or("").to_string();
        let mut codes: Vec<i64> = body
            .get("error_codes")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_i64).collect())
            .unwrap_or_default();
        if codes.is_empty() {
            codes = aadsts_codes(&description);
        }
        let error = body.get("error").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| format!("HTTP {status}"));
        Self { error, description, codes }
    }
}

fn aadsts_codes(text: &str) -> Vec<i64> {
    text.match_indices("AADSTS")
        .filter_map(|(i, _)| {
            let digits: String = text[i + 6..].chars().take_while(char::is_ascii_digit).collect();
            digits.parse().ok()
        })
        .collect()
}

impl fmt::Display for EntraError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // descriptions end with "Trace ID: … Correlation ID: … Timestamp: …", on the same line or the next
        let first = self.description.lines().next().unwrap_or("");
        let first = first.split(" Trace ID:").next().unwrap_or("").trim();
        write!(f, "Microsoft Entra ID: {}", if first.is_empty() { self.error.as_str() } else { first })?;
        if let Some(h) = self.codes.iter().find_map(|c| hint(*c)) {
            write!(f, "\n→ {h}")?;
        }
        Ok(())
    }
}

impl std::error::Error for EntraError {}

fn hint(code: i64) -> Option<&'static str> {
    Some(match code {
        50072 | 50074 | 50076 | 50079 | 50158 => {
            "Multi-factor authentication or another interactive step is required – use \"Interactive (browser)\" or \"Device code\"."
        }
        53000 | 53001 | 53002 | 53003 => {
            "Blocked by a Conditional Access policy (compliant device, location or app restrictions). Ask your Entra admin, or use \"Interactive (browser)\" on a compliant device."
        }
        50105 => "Your account is not assigned to this application – ask an admin to assign it, or use your own app registration.",
        65001 | 65004 => "Consent for this app is missing – an admin has to grant it, or use your own app registration with the database permission.",
        700016 => "The client ID was not found in this tenant – check Client ID and Tenant, or have the app consented in that tenant.",
        50020 => "This account is not in that tenant – enter the database's tenant ID (needed for guest / B2B accounts).",
        90002 | 900023 | 90072 => "Tenant not found, or the account is not a member – check the tenant ID or domain.",
        50034 => "That account does not exist in the tenant.",
        50126 => "Invalid user name or password.",
        50053 => "The account is locked after too many sign-in attempts – wait or ask an admin.",
        50057 => "The account is disabled.",
        50055 => "The password has expired – change it, then sign in again.",
        7000215 | 7000222 => "The client secret is wrong or expired – create a new one and copy its Value (not the Secret ID).",
        700026 => "The app registration has no client secret or certificate – add one under \"Certificates & secrets\", or check the client ID.",
        700027 | 700024 | 50027 => {
            "The certificate assertion was rejected – upload the certificate's public key to the app registration and check that the system clock is correct."
        }
        500011 => "The database resource was not found in this tenant – check the cloud and scope, or that Entra authentication is enabled on the server.",
        70011 | 28000 | 28003 => "The scope is not valid – it should look like https://<resource>/.default.",
        70043 | 700082 | 700084 | 50173 | 50133 | 50132 => "The saved sign-in has expired or was revoked – sign in again.",
        7000218 => "The app requires a client credential – enable \"Allow public client flows\" on the app registration, or use a service-principal method.",
        50011 => "Redirect URI mismatch – add http://localhost as a \"Mobile and desktop applications\" redirect URI on the app registration.",
        50059 => "No tenant could be determined – enter a tenant ID.",
        _ => return None,
    })
}

// ---------------------------------------------------------------- clouds & scopes

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Cloud {
    Public,
    UsGov,
    China,
}

impl Cloud {
    fn parse(s: &str) -> Result<Self> {
        Ok(match s.trim().to_ascii_lowercase().as_str() {
            "" | "public" | "azurecloud" | "global" => Cloud::Public,
            "usgov" | "gov" | "azureusgovernment" => Cloud::UsGov,
            "china" | "azurechinacloud" => Cloud::China,
            other => bail!("Unknown Azure cloud \"{other}\" (expected public, usgov or china)"),
        })
    }

    fn login_host(self) -> &'static str {
        match self {
            Cloud::Public => "https://login.microsoftonline.com",
            Cloud::UsGov => "https://login.microsoftonline.us",
            Cloud::China => "https://login.chinacloudapi.cn",
        }
    }

    fn sql_suffix(self) -> &'static str {
        match self {
            Cloud::Public => "database.windows.net",
            Cloud::UsGov => "database.usgovcloudapi.net",
            Cloud::China => "database.chinacloudapi.cn",
        }
    }
}

/// Built-in Entra resource for engines Azure hosts natively.
fn default_scope(engine: Engine, cloud: Cloud) -> Option<String> {
    match engine {
        Engine::Mssql => Some(format!("https://{}/.default", cloud.sql_suffix())),
        // Azure Database for PostgreSQL / MySQL and Cosmos DB for MongoDB vCore
        Engine::Postgres | Engine::Mysql | Engine::Mongodb => Some(format!("https://ossrdbms-aad.{}/.default", cloud.sql_suffix())),
        Engine::Redis if cloud == Cloud::Public => Some("https://redis.azure.com/.default".into()),
        _ => None,
    }
}

fn engine_name(engine: Engine) -> &'static str {
    match engine {
        Engine::Postgres => "PostgreSQL",
        Engine::Mysql => "MySQL",
        Engine::Sqlite => "SQLite",
        Engine::Mssql => "SQL Server",
        Engine::Mongodb => "MongoDB",
        Engine::Arangodb => "ArangoDB",
        Engine::Redis => "Redis",
        Engine::Clickhouse => "ClickHouse",
        Engine::Elasticsearch => "Elasticsearch",
    }
}

// ---------------------------------------------------------------- app hooks

/// Progress of an interactive sign-in, for the UI.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase", tag = "state")]
pub enum LoginEvent {
    /// The browser was sent to `url`; shown so it can be reopened or copied.
    #[serde(rename_all = "camelCase")]
    Browser { url: String },
    #[serde(rename_all = "camelCase")]
    DeviceCode { url: String, code: String, message: String },
    Done,
}

type LoginUi = Box<dyn Fn(LoginEvent) + Send + Sync>;
static UI: OnceLock<LoginUi> = OnceLock::new();
static CACHE_PATH: OnceLock<PathBuf> = OnceLock::new();
static CANCEL: LazyLock<Notify> = LazyLock::new(Notify::new);

pub fn set_login_ui(f: impl Fn(LoginEvent) + Send + Sync + 'static) {
    let _ = UI.set(Box::new(f));
}

/// Where refresh tokens are kept between runs.
pub fn set_cache_path(path: PathBuf) {
    let _ = CACHE_PATH.set(path);
}

/// Aborts every browser / device-code sign-in that is waiting.
pub fn cancel_login() {
    CANCEL.notify_waiters();
}

fn emit(e: LoginEvent) {
    if let Some(ui) = UI.get() {
        ui(e);
    }
}

/// Tells the UI the sign-in is over however the flow ends.
struct DoneGuard;

impl Drop for DoneGuard {
    fn drop(&mut self) {
        emit(LoginEvent::Done);
    }
}

pub fn open_browser(url: &str) -> Result<()> {
    #[cfg(target_os = "windows")]
    let mut cmd = {
        // `start` would need cmd.exe quoting for the `&`s in the URL
        let mut c = std::process::Command::new("rundll32");
        c.args(["url.dll,FileProtocolHandler", url]);
        c
    };
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = std::process::Command::new("open");
        c.arg(url);
        c
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut cmd = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(url);
        c
    };
    use std::process::Stdio;
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

// ---------------------------------------------------------------- caches

/// Access tokens by `account key | scope`.
static TOKENS: LazyLock<std::sync::Mutex<HashMap<String, Token>>> = LazyLock::new(Default::default);
/// One sign-in at a time per account, so parallel pool connects share it.
static LOCKS: LazyLock<std::sync::Mutex<HashMap<String, Arc<Mutex<()>>>>> = LazyLock::new(Default::default);
/// Refresh tokens by account key, loaded lazily from [`CACHE_PATH`].
static REFRESH: LazyLock<std::sync::Mutex<Option<HashMap<String, String>>>> = LazyLock::new(Default::default);

fn cached(key: &str) -> Option<Token> {
    TOKENS.lock().unwrap_or_else(|e| e.into_inner()).get(key).filter(|t| t.is_fresh()).cloned()
}

fn login_lock(account: &str) -> Arc<Mutex<()>> {
    LOCKS.lock().unwrap_or_else(|e| e.into_inner()).entry(account.to_string()).or_default().clone()
}

fn with_refresh<R>(f: impl FnOnce(&mut HashMap<String, String>) -> R) -> R {
    let mut guard = REFRESH.lock().unwrap_or_else(|e| e.into_inner());
    let map = guard.get_or_insert_with(|| {
        CACHE_PATH
            .get()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    });
    f(map)
}

fn refresh_token(account: &str) -> Option<String> {
    with_refresh(|m| m.get(account).cloned())
}

fn store_refresh_token(account: &str, token: Option<&str>) {
    with_refresh(|m| {
        let changed = match token {
            Some(t) => m.insert(account.to_string(), t.to_string()).as_deref() != Some(t),
            None => m.remove(account).is_some(),
        };
        if changed {
            persist_refresh(m);
        }
    });
}

fn persist_refresh(map: &HashMap<String, String>) {
    let Some(path) = CACHE_PATH.get() else { return };
    let Ok(json) = serde_json::to_string(map) else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension("tmp");
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let written = opts.open(&tmp).and_then(|mut f| std::io::Write::write_all(&mut f, json.as_bytes()));
    if written.is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

// ---------------------------------------------------------------- token source

pub struct TokenSource {
    kind: AuthKind,
    cloud: Cloud,
    tenant: String,
    client_id: String,
    client_secret: String,
    certificate_path: String,
    identity_client_id: String,
    pub scope: String,
    login_hint: String,
    pasted: String,
    /// Identifies the signed-in account / app; refresh tokens are stored under it.
    account_key: String,
    http: reqwest::Client,
}

/// `None` for password auth, otherwise a validated token source.
pub fn token_source(cfg: &ConnectionConfig) -> Result<Option<Arc<TokenSource>>> {
    if !cfg.auth.is_entra() {
        return Ok(None);
    }
    if cfg.engine == Engine::Sqlite {
        bail!("Microsoft Entra ID sign-in does not apply to SQLite – it is a local file.");
    }
    let o = &cfg.entra;
    let cloud = Cloud::parse(&o.cloud)?;
    let scope = match o.scope.trim() {
        "" => default_scope(cfg.engine, cloud).ok_or_else(|| {
            anyhow!(
                "{} has no built-in Entra ID resource{} – enter the token scope your server or gateway accepts, e.g. api://<app-id>/.default",
                engine_name(cfg.engine),
                if cfg.engine == Engine::Redis { " outside the public Azure cloud" } else { "" }
            )
        })?,
        s => s.to_string(),
    };
    let tenant = match o.tenant_id.trim() {
        "" => "organizations".to_string(),
        t if t.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.') => t.to_ascii_lowercase(),
        t => bail!("\"{t}\" is not a tenant ID or domain"),
    };
    let generic_tenant = matches!(tenant.as_str(), "organizations" | "common" | "consumers");
    let client_id = match o.client_id.trim() {
        "" => AZURE_CLI_CLIENT_ID.to_string(),
        c => c.to_string(),
    };
    match cfg.auth {
        AuthKind::EntraClientSecret | AuthKind::EntraCertificate if generic_tenant => {
            bail!("Service-principal sign-in needs the tenant ID of the directory that holds the app registration.")
        }
        AuthKind::EntraClientSecret | AuthKind::EntraCertificate if o.client_id.trim().is_empty() => {
            bail!("Enter the app registration's client (application) ID.")
        }
        AuthKind::EntraClientSecret if o.client_secret.is_empty() => bail!("Enter the client secret."),
        AuthKind::EntraCertificate if o.certificate_path.trim().is_empty() => bail!("Choose the certificate file (PEM with the private key)."),
        AuthKind::EntraAccessToken if cfg.password.trim().is_empty() => bail!("Paste an access token."),
        _ => {}
    }
    // only a UPN is a useful hint; Postgres group names or Redis object IDs are not
    let login_hint = match cfg.auth {
        AuthKind::EntraInteractive | AuthKind::EntraDeviceCode if cfg.user.contains('@') => cfg.user.trim().to_string(),
        _ => String::new(),
    };
    let secret_tag = |s: &str| digest(&SHA256, s.as_bytes()).as_ref()[..6].iter().map(|b| format!("{b:02x}")).collect::<String>();
    let detail = match cfg.auth {
        AuthKind::EntraClientSecret => secret_tag(&o.client_secret),
        AuthKind::EntraCertificate => o.certificate_path.trim().to_string(),
        AuthKind::EntraManagedIdentity => o.identity_client_id.trim().to_string(),
        _ => login_hint.clone(),
    };
    let account_key = format!("{:?}|{:?}|{tenant}|{client_id}|{detail}", cfg.auth, cloud).to_lowercase();
    let http = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()?;
    Ok(Some(Arc::new(TokenSource {
        kind: cfg.auth,
        cloud,
        tenant,
        client_id,
        client_secret: o.client_secret.clone(),
        certificate_path: o.certificate_path.trim().to_string(),
        identity_client_id: o.identity_client_id.trim().to_string(),
        scope,
        login_hint,
        pasted: cfg.password.trim().to_string(),
        account_key,
        http,
    })))
}

/// Forgets the cached tokens of a connection's account, so the next connect signs in again.
pub fn sign_out(cfg: &ConnectionConfig) {
    let Ok(Some(src)) = token_source(cfg) else { return };
    let prefix = format!("{}|", src.account_key);
    TOKENS.lock().unwrap_or_else(|e| e.into_inner()).retain(|k, _| !k.starts_with(&prefix));
    store_refresh_token(&src.account_key, None);
}

impl TokenSource {
    pub fn kind(&self) -> AuthKind {
        self.kind
    }

    /// A token valid for at least a few more minutes.
    pub async fn token(&self) -> Result<Token> {
        if self.kind == AuthKind::EntraAccessToken {
            let t = Token::new(self.pasted.clone(), None);
            if !t.claims.is_null() && t.expires_at <= now() {
                let ago = (now() - t.expires_at) / 60;
                bail!(
                    "The pasted access token expired {ago} minute(s) ago – get a new one, e.g. `az account get-access-token --scope {}`.",
                    self.scope
                );
            }
            return Ok(t);
        }
        let key = format!("{}|{}", self.account_key, self.scope);
        if let Some(t) = cached(&key) {
            return Ok(t);
        }
        let lock = login_lock(&self.account_key);
        let _guard = lock.lock().await;
        // another connection may have signed in while we waited
        if let Some(t) = cached(&key) {
            return Ok(t);
        }
        let token = self.acquire().await?;
        TOKENS.lock().unwrap_or_else(|e| e.into_inner()).insert(key, token.clone());
        Ok(token)
    }

    /// Database user for engines that take the token as a password: the
    /// configured name, else the signed-in user's name.
    pub fn db_user(&self, configured: &str, token: &Token) -> Result<String> {
        match configured.trim() {
            "" => token.principal_name().ok_or_else(|| {
                anyhow!("Enter the database user for this identity – for apps and managed identities it is the name its Entra role was created with (usually its display name).")
            }),
            u => Ok(u.to_string()),
        }
    }

    fn endpoint(&self, path: &str) -> String {
        format!("{}/{}/oauth2/v2.0/{path}", self.cloud.login_host(), self.tenant)
    }

    /// Scopes for user flows: the resource plus a refresh token.
    fn user_scope(&self) -> String {
        format!("{} offline_access openid profile", self.scope)
    }

    fn is_user_flow(&self) -> bool {
        matches!(self.kind, AuthKind::EntraInteractive | AuthKind::EntraDeviceCode)
    }

    async fn acquire(&self) -> Result<Token> {
        match self.kind {
            AuthKind::EntraInteractive | AuthKind::EntraDeviceCode => {
                if let Some(rt) = refresh_token(&self.account_key) {
                    let scope = self.user_scope();
                    match self.redeem(&[("grant_type", "refresh_token"), ("refresh_token", &rt), ("scope", &scope)]).await {
                        Ok(t) => return Ok(t),
                        // expired, revoked, new MFA / consent requirement → sign in again
                        Err(e) if e.downcast_ref::<EntraError>().is_some() => store_refresh_token(&self.account_key, None),
                        // offline etc. – a browser window would not help
                        Err(e) => return Err(e),
                    }
                }
                if self.kind == AuthKind::EntraInteractive {
                    self.browser_login().await
                } else {
                    self.device_code_login().await
                }
            }
            AuthKind::EntraClientSecret => {
                self.redeem(&[("grant_type", "client_credentials"), ("client_secret", &self.client_secret), ("scope", &self.scope)]).await
            }
            AuthKind::EntraCertificate => {
                let assertion = self.client_assertion()?;
                self.redeem(&[
                    ("grant_type", "client_credentials"),
                    ("client_assertion_type", "urn:ietf:params:oauth:client-assertion-type:jwt-bearer"),
                    ("client_assertion", &assertion),
                    ("scope", &self.scope),
                ])
                .await
            }
            AuthKind::EntraManagedIdentity => self.managed_identity().await,
            AuthKind::EntraAzureCli => self.azure_cli().await,
            AuthKind::Password | AuthKind::EntraAccessToken => unreachable!(),
        }
    }

    /// POSTs to the token endpoint; keeps any refresh token for user flows.
    async fn redeem(&self, form: &[(&str, &str)]) -> Result<Token> {
        let mut form = form.to_vec();
        form.push(("client_id", &self.client_id));
        let res = self
            .http
            .post(self.endpoint("token"))
            .form(&form)
            .send()
            .await
            .with_context(|| format!("contacting {}", self.cloud.login_host()))?;
        let status = res.status();
        let text = res.text().await.context("reading the token response")?;
        let body: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if let Some(at) = body.get("access_token").and_then(Value::as_str) {
            if self.is_user_flow() {
                if let Some(rt) = body.get("refresh_token").and_then(Value::as_str) {
                    store_refresh_token(&self.account_key, Some(rt));
                }
            }
            return Ok(Token::new(at.to_string(), body.get("expires_in").and_then(lenient_i64)));
        }
        if body.is_null() {
            bail!("Unexpected answer from {} (HTTP {status}): {}", self.cloud.login_host(), text.chars().take(300).collect::<String>());
        }
        Err(EntraError::from_body(&body, status).into())
    }

    // ---- interactive: authorization code + PKCE on a loopback redirect

    async fn browser_login(&self) -> Result<Token> {
        let v4 = TcpListener::bind(("127.0.0.1", 0)).await.context("opening a local port for the sign-in redirect")?;
        let port = v4.local_addr()?.port();
        // browsers may resolve "localhost" to ::1 first
        let v6 = TcpListener::bind(("::1", port)).await.ok();
        let redirect = format!("http://localhost:{port}");
        let verifier = random_b64(32);
        let challenge = URL_SAFE_NO_PAD.encode(digest(&SHA256, verifier.as_bytes()));
        let state = random_b64(16);
        let scope = self.user_scope();
        let mut url = format!(
            "{}?client_id={}&response_type=code&response_mode=query&redirect_uri={}&scope={}&state={state}&code_challenge={challenge}&code_challenge_method=S256",
            self.endpoint("authorize"),
            enc(&self.client_id),
            enc(&redirect),
            enc(&scope),
        );
        if self.login_hint.is_empty() {
            url.push_str("&prompt=select_account");
        } else {
            url.push_str(&format!("&login_hint={}", enc(&self.login_hint)));
        }

        let _done = DoneGuard;
        emit(LoginEvent::Browser { url: url.clone() });
        // the UI shows the link as well, in case no browser could be started
        let _ = open_browser(&url);
        let code = tokio::select! {
            r = wait_for_code(&v4, v6.as_ref(), &state) => r?,
            _ = tokio::time::sleep(LOGIN_TIMEOUT) => bail!("Browser sign-in timed out after 5 minutes."),
            _ = CANCEL.notified() => bail!("Sign-in was cancelled."),
        };
        self.redeem(&[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("redirect_uri", &redirect),
            ("code_verifier", &verifier),
            ("scope", &scope),
        ])
        .await
    }

    // ---- device code

    async fn device_code_login(&self) -> Result<Token> {
        let scope = self.user_scope();
        let res = self
            .http
            .post(self.endpoint("devicecode"))
            .form(&[("client_id", self.client_id.as_str()), ("scope", &scope)])
            .send()
            .await
            .with_context(|| format!("contacting {}", self.cloud.login_host()))?;
        let status = res.status();
        let body: Value = res.json().await.unwrap_or(Value::Null);
        let Some(device_code) = body.get("device_code").and_then(Value::as_str) else {
            return Err(EntraError::from_body(&body, status).into());
        };
        let text = |k: &str| body.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let mut interval = body.get("interval").and_then(lenient_i64).unwrap_or(5).max(1) as u64;
        let deadline = Instant::now() + Duration::from_secs(body.get("expires_in").and_then(lenient_i64).unwrap_or(900).max(60) as u64);

        let _done = DoneGuard;
        emit(LoginEvent::DeviceCode { url: text("verification_uri"), code: text("user_code"), message: text("message") });
        loop {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(interval)) => {}
                _ = CANCEL.notified() => bail!("Sign-in was cancelled."),
            }
            if Instant::now() > deadline {
                bail!("The device code expired before sign-in finished – try again.");
            }
            match self.redeem(&[("grant_type", "urn:ietf:params:oauth:grant-type:device_code"), ("device_code", device_code)]).await {
                Ok(t) => return Ok(t),
                Err(e) => match e.downcast_ref::<EntraError>().map(|e| e.error.as_str()) {
                    Some("authorization_pending") => {}
                    Some("slow_down") => interval += 5,
                    // authorization_declined, expired_token, bad_verification_code, network
                    _ => return Err(e),
                },
            }
        }
    }

    // ---- service principal with a certificate

    fn client_assertion(&self) -> Result<String> {
        let path = &self.certificate_path;
        let lower = path.to_ascii_lowercase();
        if lower.ends_with(".pfx") || lower.ends_with(".p12") {
            bail!("PFX / PKCS#12 files are not supported – convert to PEM first: openssl pkcs12 -in cert.pfx -out cert.pem -nodes");
        }
        let pem = std::fs::read_to_string(path).with_context(|| format!("reading certificate {path}"))?;
        let blocks = pem_blocks(&pem)?;
        let cert = blocks
            .iter()
            .find(|(label, _)| label == "CERTIFICATE")
            .map(|(_, der)| der)
            .ok_or_else(|| anyhow!("{path} has no CERTIFICATE block – the PEM file must contain the certificate and its private key"))?;
        let key = match blocks.iter().find(|(label, _)| label.ends_with("PRIVATE KEY")) {
            None => bail!("{path} has no private key – the PEM file must contain the certificate and its private key"),
            Some((label, _)) if label.contains("ENCRYPTED") => {
                bail!("The private key is password-protected – export it without a password: openssl pkey -in key.pem -out plain-key.pem")
            }
            Some((label, der)) if label == "RSA PRIVATE KEY" => RsaKeyPair::from_der(der),
            Some((label, der)) if label == "PRIVATE KEY" => RsaKeyPair::from_pkcs8(der),
            Some((label, _)) => bail!("Unsupported key type \"{label}\" – Entra ID certificate credentials use RSA keys."),
        }
        .map_err(|e| anyhow!("The private key could not be loaded ({e}) – Entra ID certificate credentials use RSA keys."))?;

        let header = json!({
            "alg": "RS256",
            "typ": "JWT",
            "x5t": URL_SAFE_NO_PAD.encode(digest(&SHA1_FOR_LEGACY_USE_ONLY, cert)),
            "x5t#S256": URL_SAFE_NO_PAD.encode(digest(&SHA256, cert)),
        });
        let t = now();
        let claims = json!({
            "aud": self.endpoint("token"),
            "iss": self.client_id,
            "sub": self.client_id,
            "jti": random_b64(16),
            "nbf": t - 60,
            "iat": t,
            "exp": t + 600,
        });
        let input = format!("{}.{}", URL_SAFE_NO_PAD.encode(header.to_string()), URL_SAFE_NO_PAD.encode(claims.to_string()));
        let mut sig = vec![0u8; key.public().modulus_len()];
        key.sign(&RSA_PKCS1_SHA256, &SystemRandom::new(), input.as_bytes(), &mut sig)
            .map_err(|_| anyhow!("signing the client assertion failed"))?;
        Ok(format!("{input}.{}", URL_SAFE_NO_PAD.encode(sig)))
    }

    // ---- managed identity

    async fn managed_identity(&self) -> Result<Token> {
        if self.scope.contains(char::is_whitespace) {
            bail!("Managed identity takes a single resource scope.");
        }
        let resource = self.scope.trim_end_matches("/.default");
        let id = self.identity_client_id.as_str();
        // the endpoints are link-local / localhost – a system proxy would break them
        let client = reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(30))
            .build()?;
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let (req, label) = match (env("IDENTITY_ENDPOINT"), env("IDENTITY_HEADER"), env("IMDS_ENDPOINT")) {
            // App Service, Functions, Container Apps
            (Some(endpoint), Some(header), _) => {
                let mut q = vec![("api-version", "2019-08-01"), ("resource", resource)];
                if !id.is_empty() {
                    q.push(("client_id", id));
                }
                (client.get(endpoint).query(&q).header("X-IDENTITY-HEADER", header), "App Service identity endpoint")
            }
            // Azure Arc-enabled servers
            (Some(endpoint), None, Some(_)) => return self.arc_identity(&client, &endpoint, resource).await,
            // VMs, VM scale sets, AKS (workload identity aside)
            _ => {
                let mut q = vec![("api-version", "2018-02-01"), ("resource", resource)];
                if !id.is_empty() {
                    q.push(("client_id", id));
                }
                (
                    client.get("http://169.254.169.254/metadata/identity/oauth2/token").query(&q).header("Metadata", "true"),
                    "Azure Instance Metadata Service",
                )
            }
        };
        let res = req.send().await.map_err(|e| {
            anyhow!(
                "No managed identity is reachable ({label}: {e}).\n→ Managed identity only works when Nexus DB runs on an Azure VM, App Service, Container Apps, AKS or an Arc-enabled server. On a workstation use \"Azure CLI\" or \"Interactive (browser)\"."
            )
        })?;
        self.identity_response(res).await
    }

    async fn arc_identity(&self, client: &reqwest::Client, endpoint: &str, resource: &str) -> Result<Token> {
        let q = [("api-version", "2020-06-01"), ("resource", resource)];
        let first = client.get(endpoint).query(&q).header("Metadata", "true").send().await.context("contacting the Azure Arc identity endpoint")?;
        if first.status() != reqwest::StatusCode::UNAUTHORIZED {
            return self.identity_response(first).await;
        }
        // Arc answers with the path of a key file only local admins can read
        let key_path = first
            .headers()
            .get("WWW-Authenticate")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split_once("realm="))
            .map(|(_, p)| p.trim().to_string())
            .ok_or_else(|| anyhow!("Azure Arc did not return an authentication challenge"))?;
        let secret = std::fs::read_to_string(&key_path)
            .with_context(|| format!("reading the Arc identity key {key_path} (needs membership in the himds / Hybrid agent extension applications group)"))?;
        let res = client
            .get(endpoint)
            .query(&q)
            .header("Metadata", "true")
            .header("Authorization", format!("Basic {}", secret.trim()))
            .send()
            .await?;
        self.identity_response(res).await
    }

    async fn identity_response(&self, res: reqwest::Response) -> Result<Token> {
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        let body: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if let Some(at) = body.get("access_token").and_then(Value::as_str) {
            let expires_in = body
                .get("expires_in")
                .and_then(lenient_i64)
                .or_else(|| body.get("expires_on").and_then(lenient_i64).map(|t| t - now()));
            return Ok(Token::new(at.to_string(), expires_in));
        }
        let detail = ["error_description", "message", "error"]
            .iter()
            .find_map(|k| body.get(*k).and_then(Value::as_str))
            .map(str::to_string)
            .unwrap_or_else(|| text.chars().take(300).collect());
        let mut msg = format!("Managed identity: HTTP {status}: {detail}");
        if self.identity_client_id.is_empty() && matches!(status.as_u16(), 400 | 404) {
            msg.push_str("\n→ If the identity is user-assigned, enter its client ID.");
        }
        bail!(msg)
    }

    // ---- Azure CLI

    async fn azure_cli(&self) -> Result<Token> {
        let mut args = vec!["account", "get-access-token", "--output", "json", "--scope", self.scope.as_str()];
        let explicit_tenant = !matches!(self.tenant.as_str(), "organizations" | "common" | "consumers");
        if explicit_tenant {
            args.extend(["--tenant", self.tenant.as_str()]);
        }
        for program in az_candidates() {
            let mut cmd = tokio::process::Command::new(&program);
            cmd.args(&args).kill_on_drop(true).stdin(std::process::Stdio::null());
            #[cfg(windows)]
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
            let out = match tokio::time::timeout(Duration::from_secs(60), cmd.output()).await {
                Err(_) => bail!("Azure CLI did not answer within 60 seconds."),
                Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Ok(Err(e)) => return Err(e).with_context(|| format!("running {program}")),
                Ok(Ok(out)) => out,
            };
            if !out.status.success() {
                let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
                if err.contains("az login") || err.contains("AADSTS70043") || err.contains("AADSTS700082") {
                    let login = if explicit_tenant { format!("az login --tenant {}", self.tenant) } else { "az login".to_string() };
                    bail!("Azure CLI is not signed in (or the session expired) – run `{login}` in a terminal, then try again.\n{err}");
                }
                bail!("Azure CLI: {err}");
            }
            let v: Value = serde_json::from_slice(&out.stdout).context("parsing the output of az account get-access-token")?;
            let at = v.get("accessToken").and_then(Value::as_str).ok_or_else(|| anyhow!("az returned no accessToken"))?;
            let expires_in = v.get("expires_on").and_then(lenient_i64).map(|t| t - now());
            return Ok(Token::new(at.to_string(), expires_in));
        }
        bail!("Azure CLI (az) was not found. Install it from https://aka.ms/installazurecli or choose another sign-in method.")
    }
}

/// GUI apps often start with a minimal PATH, so also try the usual install locations.
fn az_candidates() -> Vec<String> {
    if cfg!(windows) {
        vec![
            "az.cmd".into(),
            r"C:\Program Files\Microsoft SDKs\Azure\CLI2\wbin\az.cmd".into(),
            r"C:\Program Files (x86)\Microsoft SDKs\Azure\CLI2\wbin\az.cmd".into(),
        ]
    } else {
        let mut v: Vec<String> = ["az", "/opt/homebrew/bin/az", "/usr/local/bin/az", "/usr/bin/az"].map(String::from).to_vec();
        if let Ok(home) = std::env::var("HOME") {
            v.push(format!("{home}/.local/bin/az"));
        }
        v
    }
}

fn pem_blocks(pem: &str) -> Result<Vec<(String, Vec<u8>)>> {
    let mut out = Vec::new();
    let mut current: Option<(String, String)> = None;
    for line in pem.lines().map(str::trim) {
        if let Some(label) = line.strip_prefix("-----BEGIN ").and_then(|l| l.strip_suffix("-----")) {
            current = Some((label.to_string(), String::new()));
        } else if line.starts_with("-----END ") {
            if let Some((label, b64)) = current.take() {
                let der = STANDARD.decode(b64).with_context(|| format!("invalid base64 in the {label} block"))?;
                out.push((label, der));
            }
        } else if let Some((_, b64)) = current.as_mut() {
            // skip "Proc-Type:" style headers of legacy encrypted keys
            if !line.contains(':') {
                b64.push_str(line);
            }
        }
    }
    if out.is_empty() {
        bail!("not a PEM file");
    }
    Ok(out)
}

// ---------------------------------------------------------------- loopback redirect

async fn wait_for_code(v4: &TcpListener, v6: Option<&TcpListener>, state: &str) -> Result<String> {
    loop {
        let (mut stream, _) = match v6 {
            Some(v6) => tokio::select! { r = v4.accept() => r?, r = v6.accept() => r? },
            None => v4.accept().await?,
        };
        let Some(target) = read_request_target(&mut stream).await else { continue };
        let params = query_params(&target);
        let get = |k: &str| params.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        if let Some(error) = get("error") {
            let description = get("error_description").unwrap_or("").to_string();
            let e = EntraError { error: error.to_string(), codes: aadsts_codes(&description), description };
            respond(&mut stream, false, &e.to_string()).await;
            return Err(e.into());
        }
        match (get("code"), get("state")) {
            (Some(code), Some(s)) if s == state => {
                respond(&mut stream, true, "You are signed in. You can close this tab and return to Nexus DB.").await;
                return Ok(code.to_string());
            }
            // a tab from an earlier attempt – keep waiting for the current one
            (Some(_), _) => respond(&mut stream, false, "This sign-in page belongs to an earlier attempt. Return to Nexus DB and use the latest window.").await,
            // favicon.ico, probes …
            _ => {
                let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
            }
        }
    }
}

async fn read_request_target(stream: &mut TcpStream) -> Option<String> {
    let mut buf = Vec::with_capacity(2048);
    let mut chunk = [0u8; 2048];
    let read = async {
        while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < 16 * 1024 {
            let n = stream.read(&mut chunk).await.ok()?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        Some(())
    };
    tokio::time::timeout(Duration::from_secs(10), read).await.ok()??;
    let head = String::from_utf8_lossy(&buf);
    let mut parts = head.lines().next()?.split_whitespace();
    (parts.next()? == "GET").then(|| parts.next().map(str::to_string))?
}

fn query_params(target: &str) -> Vec<(String, String)> {
    let query = target.split_once('?').map(|(_, q)| q).unwrap_or("");
    let decode = |s: &str| urlencoding::decode(&s.replace('+', " ")).map(|c| c.into_owned()).unwrap_or_default();
    query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (decode(k), decode(v))
        })
        .collect()
}

async fn respond(stream: &mut TcpStream, ok: bool, message: &str) {
    let escaped = message.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('\n', "<br>");
    let (title, color) = if ok { ("Signed in", "#16a34a") } else { ("Sign-in failed", "#dc2626") };
    let html = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Nexus DB – {title}</title></head>\
         <body style=\"font-family:system-ui,sans-serif;display:grid;place-items:center;height:100vh;margin:0;background:#f8fafc;color:#0f172a\">\
         <div style=\"max-width:520px;padding:32px;text-align:center\"><h1 style=\"color:{color};font-size:22px\">{title}</h1>\
         <p style=\"line-height:1.5\">{escaped}</p></div></body></html>"
    );
    let res = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{html}",
        html.len()
    );
    let _ = stream.write_all(res.as_bytes()).await;
    let _ = stream.shutdown().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::EntraOptions;

    fn cfg(engine: Engine, auth: AuthKind, entra: EntraOptions) -> ConnectionConfig {
        ConnectionConfig { engine, auth, entra, ..Default::default() }
    }

    #[test]
    fn password_auth_has_no_source() {
        assert!(token_source(&ConnectionConfig::default()).unwrap().is_none());
    }

    #[test]
    fn default_scopes_per_engine_and_cloud() {
        let scope = |engine, cloud: &str| {
            let entra = EntraOptions { cloud: cloud.into(), ..Default::default() };
            token_source(&cfg(engine, AuthKind::EntraAzureCli, entra)).map(|s| s.unwrap().scope.clone())
        };
        assert_eq!(scope(Engine::Mssql, "").unwrap(), "https://database.windows.net/.default");
        assert_eq!(scope(Engine::Postgres, "usgov").unwrap(), "https://ossrdbms-aad.database.usgovcloudapi.net/.default");
        assert_eq!(scope(Engine::Mysql, "china").unwrap(), "https://ossrdbms-aad.database.chinacloudapi.cn/.default");
        assert_eq!(scope(Engine::Redis, "").unwrap(), "https://redis.azure.com/.default");
        assert!(scope(Engine::Redis, "usgov").is_err());
        assert!(scope(Engine::Elasticsearch, "").is_err());
        assert!(scope(Engine::Sqlite, "").is_err());
        assert!(scope(Engine::Mssql, "mars").is_err());
    }

    #[test]
    fn custom_scope_enables_any_http_engine() {
        let entra = EntraOptions { scope: "api://1234/.default".into(), ..Default::default() };
        let src = token_source(&cfg(Engine::Elasticsearch, AuthKind::EntraInteractive, entra)).unwrap().unwrap();
        assert_eq!(src.scope, "api://1234/.default");
    }

    #[test]
    fn service_principal_validation() {
        let sp = |entra| token_source(&cfg(Engine::Mssql, AuthKind::EntraClientSecret, entra));
        assert!(sp(EntraOptions { client_id: "a".into(), client_secret: "s".into(), ..Default::default() }).is_err(), "needs a tenant");
        assert!(sp(EntraOptions { tenant_id: "contoso.com".into(), client_secret: "s".into(), ..Default::default() }).is_err(), "needs a client id");
        assert!(sp(EntraOptions { tenant_id: "contoso.com".into(), client_id: "a".into(), ..Default::default() }).is_err(), "needs a secret");
        assert!(sp(EntraOptions { tenant_id: "contoso.com".into(), client_id: "a".into(), client_secret: "s".into(), ..Default::default() }).is_ok());
        assert!(sp(EntraOptions { tenant_id: "bad tenant".into(), client_id: "a".into(), client_secret: "s".into(), ..Default::default() }).is_err());
    }

    #[test]
    fn rotated_secret_does_not_reuse_cached_token() {
        let key = |secret: &str| {
            let entra = EntraOptions { tenant_id: "t".into(), client_id: "a".into(), client_secret: secret.into(), ..Default::default() };
            token_source(&cfg(Engine::Mssql, AuthKind::EntraClientSecret, entra)).unwrap().unwrap().account_key.clone()
        };
        assert_ne!(key("old"), key("new"));
        assert!(!key("old").contains("old"), "the secret itself must not end up in the cache key");
    }

    #[test]
    fn login_hint_only_for_upns() {
        let hint = |user: &str| {
            let c = ConnectionConfig { user: user.into(), ..cfg(Engine::Postgres, AuthKind::EntraInteractive, Default::default()) };
            token_source(&c).unwrap().unwrap().login_hint.clone()
        };
        assert_eq!(hint("ada@contoso.com"), "ada@contoso.com");
        assert_eq!(hint("db-admins"), "");
    }

    fn fake_jwt(claims: Value) -> String {
        format!("e30.{}.sig", URL_SAFE_NO_PAD.encode(claims.to_string()))
    }

    #[tokio::test]
    async fn pasted_token_expiry_is_checked() {
        let paste = |tok: String| ConnectionConfig { password: tok, ..cfg(Engine::Mssql, AuthKind::EntraAccessToken, Default::default()) };
        let expired = token_source(&paste(fake_jwt(json!({ "exp": now() - 120 })))).unwrap().unwrap();
        assert!(expired.token().await.unwrap_err().to_string().contains("expired"));
        let valid = token_source(&paste(fake_jwt(json!({ "exp": now() + 3600, "upn": "ada@contoso.com", "oid": "42" })))).unwrap().unwrap();
        let t = valid.token().await.unwrap();
        assert_eq!(t.principal_name().as_deref(), Some("ada@contoso.com"));
        assert_eq!(t.object_id().as_deref(), Some("42"));
        assert_eq!(valid.db_user("", &t).unwrap(), "ada@contoso.com");
        assert_eq!(valid.db_user("aad_admins", &t).unwrap(), "aad_admins");
        // opaque (non-JWT) tokens are passed through untouched
        assert!(token_source(&paste("opaque".into())).unwrap().unwrap().token().await.is_ok());
    }

    #[test]
    fn app_token_needs_explicit_db_user() {
        let c = cfg(Engine::Postgres, AuthKind::EntraAzureCli, Default::default());
        let src = token_source(&c).unwrap().unwrap();
        let app = Token::new(fake_jwt(json!({ "exp": now() + 3600, "oid": "1" })), None);
        assert!(src.db_user("", &app).is_err());
    }

    #[test]
    fn errors_get_hints() {
        let body = json!({
            "error": "invalid_grant",
            "error_description": "AADSTS50076: Due to a configuration change made by your administrator, you must use multi-factor authentication.\r\nTrace ID: x",
            "error_codes": [50076]
        });
        let msg = EntraError::from_body(&body, 400).to_string();
        assert!(msg.contains("AADSTS50076") && msg.contains("Interactive") && !msg.contains("Trace ID"));
        assert_eq!(aadsts_codes("AADSTS700016: app not found"), vec![700016]);
        let one_line = json!({ "error": "invalid_client", "error_description": "AADSTS700026: Client application has no configured keys. Trace ID: 6b07 Correlation ID: e06f" });
        let msg = EntraError::from_body(&one_line, 401).to_string();
        assert!(msg.contains("no configured keys.") && !msg.contains("Trace ID") && msg.contains("Certificates & secrets"));
    }

    #[test]
    fn redirect_query_parsing() {
        let p = query_params("/?code=0.AX%2Bb&state=abc&session_state=x+y");
        assert_eq!(p[0], ("code".into(), "0.AX+b".into()));
        assert_eq!(p[2].1, "x y");
    }

    #[tokio::test]
    async fn loopback_redirect_skips_noise_and_stale_attempts() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let browser = tokio::spawn(async move {
            let mut pages = Vec::new();
            for target in ["/favicon.ico", "/?code=old&state=previous", "/?code=fresh&state=current"] {
                let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
                s.write_all(format!("GET {target} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes()).await.unwrap();
                let mut page = String::new();
                s.read_to_string(&mut page).await.unwrap();
                pages.push(page);
            }
            pages
        });
        assert_eq!(wait_for_code(&listener, None, "current").await.unwrap(), "fresh");
        let pages = browser.await.unwrap();
        assert!(pages[0].starts_with("HTTP/1.1 404"));
        assert!(pages[1].contains("earlier attempt"));
        assert!(pages[2].contains("You are signed in"));
    }

    #[tokio::test]
    async fn loopback_redirect_reports_entra_errors() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
            let target = "/?error=access_denied&error_description=AADSTS65004%3A+User+declined+to+consent&state=s";
            s.write_all(format!("GET {target} HTTP/1.1\r\n\r\n").as_bytes()).await.unwrap();
            let mut sink = Vec::new();
            let _ = s.read_to_end(&mut sink).await;
        });
        let err = wait_for_code(&listener, None, "s").await.unwrap_err();
        let entra = err.downcast_ref::<EntraError>().unwrap();
        assert_eq!(entra.codes, vec![65004]);
        assert!(err.to_string().contains("Consent"));
    }

    /// Needs the `openssl` binary to create a throw-away certificate; skipped without it.
    #[test]
    fn certificate_assertion_is_a_valid_rs256_jwt() {
        let dir = std::env::temp_dir().join(format!("nexus-entra-{}", random_b64(6)));
        std::fs::create_dir_all(&dir).unwrap();
        let pem = dir.join("sp.pem");
        let key = dir.join("key.pem");
        let crt = dir.join("crt.pem");
        let made = std::process::Command::new("openssl")
            .args(["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1", "-subj", "/CN=nexus-test"])
            .arg("-keyout")
            .arg(&key)
            .arg("-out")
            .arg(&crt)
            .output();
        if !made.map(|o| o.status.success()).unwrap_or(false) {
            eprintln!("openssl not available – skipping");
            return;
        }
        let both = std::fs::read_to_string(&crt).unwrap() + &std::fs::read_to_string(&key).unwrap();
        std::fs::write(&pem, both).unwrap();

        let entra = EntraOptions {
            tenant_id: "contoso.onmicrosoft.com".into(),
            client_id: "app-id".into(),
            certificate_path: pem.to_string_lossy().into(),
            ..Default::default()
        };
        let src = token_source(&cfg(Engine::Mssql, AuthKind::EntraCertificate, entra)).unwrap().unwrap();
        let jwt = src.client_assertion().unwrap();
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3);
        let header: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[0]).unwrap()).unwrap();
        assert_eq!(header["alg"], "RS256");
        let cert_der = &pem_blocks(&std::fs::read_to_string(&crt).unwrap()).unwrap()[0].1;
        assert_eq!(header["x5t"], URL_SAFE_NO_PAD.encode(digest(&SHA1_FOR_LEGACY_USE_ONLY, cert_der)));
        let claims = jwt_claims(&jwt).unwrap();
        assert_eq!(claims["aud"], "https://login.microsoftonline.com/contoso.onmicrosoft.com/oauth2/v2.0/token");
        assert_eq!(claims["iss"], "app-id");

        let key_der = &pem_blocks(&std::fs::read_to_string(&key).unwrap()).unwrap()[0].1;
        let pair = RsaKeyPair::from_pkcs8(key_der).unwrap();
        use ring::signature::KeyPair;
        let public = ring::signature::UnparsedPublicKey::new(&ring::signature::RSA_PKCS1_2048_8192_SHA256, pair.public_key().as_ref());
        let sig = URL_SAFE_NO_PAD.decode(parts[2]).unwrap();
        public.verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &sig).expect("signature verifies");

        // a key-only file is rejected with a helpful message
        let only_key = token_source(&ConnectionConfig {
            entra: EntraOptions { certificate_path: key.to_string_lossy().into(), ..src_opts() },
            ..cfg(Engine::Mssql, AuthKind::EntraCertificate, Default::default())
        })
        .unwrap()
        .unwrap();
        assert!(only_key.client_assertion().unwrap_err().to_string().contains("no CERTIFICATE"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn src_opts() -> EntraOptions {
        EntraOptions { tenant_id: "t".into(), client_id: "a".into(), ..Default::default() }
    }

    #[test]
    fn token_expiry_from_jwt_or_expires_in() {
        let t = Token::new(fake_jwt(json!({ "exp": now() + 100 })), Some(9999));
        assert!(!t.is_fresh(), "inside the refresh margin");
        let opaque = Token::new("opaque".into(), Some(3600));
        assert!(opaque.is_fresh());
    }
}
