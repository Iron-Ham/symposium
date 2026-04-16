use crate::error::{Error, Result};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use tokio::io::AsyncWriteExt;

/// OAuth 2.0 token set, cached to disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenSet {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: u64,
}

impl TokenSet {
    pub fn is_expired(&self) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        // Refresh 60s before expiry
        now + 60 >= self.expires_at
    }
}

/// OAuth server metadata from `.well-known/oauth-authorization-server`.
#[derive(Debug, Deserialize)]
struct OAuthMetadata {
    authorization_endpoint: String,
    token_endpoint: String,
    registration_endpoint: Option<String>,
}

/// Dynamic client registration response.
#[derive(Debug, Serialize, Deserialize)]
struct ClientRegistration {
    client_id: String,
    client_secret: Option<String>,
}

/// Cached OAuth state (client registration + tokens).
#[derive(Debug, Serialize, Deserialize)]
struct OAuthCache {
    server_url: String,
    client: ClientRegistration,
    tokens: Option<TokenSet>,
}

/// Manages OAuth 2.0 authorization code flow with PKCE for an MCP server.
pub struct OAuthClient {
    server_url: String,
    http: reqwest::Client,
    cache_path: PathBuf,
    cache: Option<OAuthCache>,
}

impl OAuthClient {
    pub fn new(server_url: &str) -> Self {
        let cache_dir = dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("~/.local/share"))
            .join("symposium")
            .join("oauth");

        // Derive cache filename from server URL
        let hash = {
            let mut h = Sha256::new();
            h.update(server_url.as_bytes());
            URL_SAFE_NO_PAD.encode(h.finalize())[..16].to_string()
        };
        let cache_path = cache_dir.join(format!("{hash}.json"));

        Self {
            server_url: server_url.trim_end_matches('/').to_string(),
            http: reqwest::Client::new(),
            cache_path,
            cache: None,
        }
    }

    /// Purge any cached tokens and client registration from memory and disk,
    /// then run the full interactive OAuth authorization flow (opens a
    /// browser). Intended for one-shot CLI use to recover from a revoked or
    /// expired refresh token — the daemon must never call this, since the
    /// browser flow would block the tick loop.
    pub async fn reauthorize(&mut self) -> Result<String> {
        self.cache = None;
        match tokio::fs::remove_file(&self.cache_path).await {
            Ok(_) => {
                tracing::info!(path = %self.cache_path.display(), "purged oauth cache");
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(Error::Mcp(format!(
                    "failed to purge oauth cache at {}: {e}",
                    self.cache_path.display()
                )));
            }
        }
        self.get_token().await
    }

    /// Force a refresh using the cached refresh_token, bypassing the local
    /// `expires_at` check. Intended for recovery after the server returns 401
    /// on a token that the client still considered valid (e.g. server-side
    /// rotation or revocation).
    ///
    /// Returns an error if there is no cached refresh token, or if the refresh
    /// request itself fails — callers in daemon contexts should surface this
    /// so the user can re-authorize out of band rather than blocking a request
    /// path on the interactive browser flow.
    pub async fn force_refresh(&mut self) -> Result<String> {
        if self.cache.is_none() {
            self.cache = self.load_cache().await;
        }

        let (client, refresh) = {
            let cache = self.cache.as_ref().ok_or_else(|| {
                Error::Mcp("no OAuth cache on disk; re-authorize out of band".into())
            })?;
            let tokens = cache.tokens.as_ref().ok_or_else(|| {
                Error::Mcp("no cached tokens; re-authorize out of band".into())
            })?;
            let refresh = tokens.refresh_token.clone().ok_or_else(|| {
                Error::Mcp("no refresh token available; re-authorize out of band".into())
            })?;
            (
                ClientRegistration {
                    client_id: cache.client.client_id.clone(),
                    client_secret: cache.client.client_secret.clone(),
                },
                refresh,
            )
        };

        match self.refresh_token(&client, &refresh).await {
            Ok(new_tokens) => {
                let access = new_tokens.access_token.clone();
                self.cache.as_mut().unwrap().tokens = Some(new_tokens);
                self.save_cache().await?;
                Ok(access)
            }
            Err(e) if is_invalid_grant(&e) => {
                // Refresh token is revoked or expired server-side (typical
                // after a long idle period). Purge the stale cache and fall
                // through to the interactive authorize flow — this opens a
                // browser and blocks the caller for up to ~120s while the
                // user completes consent, but it's the only way to recover
                // without manual intervention.
                tracing::warn!(
                    "refresh token rejected ({e}); launching interactive re-authorization"
                );
                self.cache = None;
                if let Err(err) = tokio::fs::remove_file(&self.cache_path).await
                    && err.kind() != std::io::ErrorKind::NotFound
                {
                    tracing::warn!("failed to purge stale oauth cache: {err}");
                }
                self.get_token().await
            }
            Err(e) => Err(e),
        }
    }

    /// Get a valid access token, refreshing or re-authorizing as needed.
    pub async fn get_token(&mut self) -> Result<String> {
        // Load cache from disk if we haven't yet
        if self.cache.is_none() {
            self.cache = self.load_cache().await;
        }

        // If we have a non-expired token, use it
        if let Some(ref cache) = self.cache
            && let Some(ref tokens) = cache.tokens
        {
            if !tokens.is_expired() {
                return Ok(tokens.access_token.clone());
            }
            // Try refresh
            if let Some(ref refresh) = tokens.refresh_token {
                match self.refresh_token(&cache.client, refresh).await {
                    Ok(new_tokens) => {
                        let access = new_tokens.access_token.clone();
                        self.cache.as_mut().unwrap().tokens = Some(new_tokens);
                        self.save_cache().await?;
                        return Ok(access);
                    }
                    Err(e) => {
                        tracing::warn!("token refresh failed, re-authorizing: {e}");
                    }
                }
            }
        }

        // Full authorization flow
        let metadata = self.fetch_metadata().await?;
        let client = self.ensure_client(&metadata).await?;
        let tokens = self.authorize(&metadata, &client).await?;
        let access = tokens.access_token.clone();

        self.cache = Some(OAuthCache {
            server_url: self.server_url.clone(),
            client,
            tokens: Some(tokens),
        });
        self.save_cache().await?;

        Ok(access)
    }

    /// Derive the OAuth base URL (scheme + host) from the MCP server URL,
    /// stripping the path per the MCP OAuth discovery spec.
    fn oauth_base_url(&self) -> String {
        // Find the end of "scheme://host[:port]"
        if let Some(rest) = self.server_url.strip_prefix("https://") {
            let host_end = rest.find('/').unwrap_or(rest.len());
            format!("https://{}", &rest[..host_end])
        } else if let Some(rest) = self.server_url.strip_prefix("http://") {
            let host_end = rest.find('/').unwrap_or(rest.len());
            format!("http://{}", &rest[..host_end])
        } else {
            self.server_url.clone()
        }
    }

    async fn fetch_metadata(&self) -> Result<OAuthMetadata> {
        let base = self.oauth_base_url();
        let url = format!("{base}/.well-known/oauth-authorization-server");
        self.http
            .get(&url)
            .send()
            .await
            .map_err(|e| Error::Mcp(format!("OAuth discovery failed: {e}")))?
            .json::<OAuthMetadata>()
            .await
            .map_err(|e| Error::Mcp(format!("OAuth discovery parse failed: {e}")))
    }

    async fn ensure_client(&mut self, metadata: &OAuthMetadata) -> Result<ClientRegistration> {
        // Reuse cached client registration if available for this server
        if let Some(ref cache) = self.cache
            && cache.server_url == self.server_url
        {
            return Ok(ClientRegistration {
                client_id: cache.client.client_id.clone(),
                client_secret: cache.client.client_secret.clone(),
            });
        }

        let reg_url = metadata.registration_endpoint.as_ref().ok_or_else(|| {
            Error::Mcp("MCP server does not support dynamic client registration".into())
        })?;

        let resp = self
            .http
            .post(reg_url)
            .json(&serde_json::json!({
                "client_name": "symposium",
                "redirect_uris": ["http://127.0.0.1:19823/callback"],
                "grant_types": ["authorization_code", "refresh_token"],
                "response_types": ["code"],
                "token_endpoint_auth_method": "none"
            }))
            .send()
            .await
            .map_err(|e| Error::Mcp(format!("client registration failed: {e}")))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Mcp(format!("client registration rejected: {body}")));
        }

        resp.json::<ClientRegistration>()
            .await
            .map_err(|e| Error::Mcp(format!("client registration parse failed: {e}")))
    }

    async fn authorize(
        &self,
        metadata: &OAuthMetadata,
        client: &ClientRegistration,
    ) -> Result<TokenSet> {
        // Generate PKCE challenge
        let verifier = generate_pkce_verifier();
        let challenge = generate_pkce_challenge(&verifier);

        // Generate state parameter
        let state = generate_random_string(32);

        // Start local callback server
        let listener = tokio::net::TcpListener::bind("127.0.0.1:19823")
            .await
            .map_err(|e| Error::Mcp(format!("failed to bind callback server: {e}")))?;

        // Build authorization URL
        let auth_url = format!(
            "{}?response_type=code&client_id={}&redirect_uri={}&state={}&code_challenge={}&code_challenge_method=S256",
            metadata.authorization_endpoint,
            urlencoded(&client.client_id),
            urlencoded("http://127.0.0.1:19823/callback"),
            urlencoded(&state),
            urlencoded(&challenge),
        );

        tracing::info!("opening browser for OAuth authorization...");
        eprintln!("\n  Authorize Symposium at:\n  {auth_url}\n");
        let _ = open::that(&auth_url);

        // Wait for callback
        let code = wait_for_callback(listener, &state).await?;

        // Exchange code for tokens
        self.exchange_code(metadata, client, &code, &verifier)
            .await
    }

    async fn exchange_code(
        &self,
        metadata: &OAuthMetadata,
        client: &ClientRegistration,
        code: &str,
        verifier: &str,
    ) -> Result<TokenSet> {
        let mut params = vec![
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", "http://127.0.0.1:19823/callback"),
            ("client_id", &client.client_id),
            ("code_verifier", verifier),
        ];
        let secret_str;
        if let Some(ref secret) = client.client_secret {
            secret_str = secret.clone();
            params.push(("client_secret", &secret_str));
        }

        let resp = self
            .http
            .post(&metadata.token_endpoint)
            .form(&params)
            .send()
            .await
            .map_err(|e| Error::Mcp(format!("token exchange failed: {e}")))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Mcp(format!("token exchange rejected: {body}")));
        }

        parse_token_response(resp).await
    }

    async fn refresh_token(
        &self,
        client: &ClientRegistration,
        refresh_token: &str,
    ) -> Result<TokenSet> {
        let metadata = self.fetch_metadata().await?;

        let mut params = vec![
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", &client.client_id),
        ];
        let secret_str;
        if let Some(ref secret) = client.client_secret {
            secret_str = secret.clone();
            params.push(("client_secret", &secret_str));
        }

        let resp = self
            .http
            .post(&metadata.token_endpoint)
            .form(&params)
            .send()
            .await
            .map_err(|e| Error::Mcp(format!("token refresh failed: {e}")))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Mcp(format!("token refresh rejected: {body}")));
        }

        parse_token_response(resp).await
    }

    async fn load_cache(&self) -> Option<OAuthCache> {
        let data = tokio::fs::read_to_string(&self.cache_path).await.ok()?;
        serde_json::from_str(&data).ok()
    }

    async fn save_cache(&self) -> Result<()> {
        let Some(ref cache) = self.cache else {
            return Ok(());
        };
        if let Some(parent) = self.cache_path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                Error::Mcp(format!("failed to create oauth cache dir: {e}"))
            })?;
        }
        let data = serde_json::to_string_pretty(cache)
            .map_err(|e| Error::Mcp(format!("serialize cache: {e}")))?;
        let mut file = tokio::fs::File::create(&self.cache_path)
            .await
            .map_err(|e| Error::Mcp(format!("create cache file: {e}")))?;
        file.write_all(data.as_bytes())
            .await
            .map_err(|e| Error::Mcp(format!("write cache file: {e}")))?;
        Ok(())
    }
}

async fn parse_token_response(resp: reqwest::Response) -> Result<TokenSet> {
    #[derive(Deserialize)]
    struct TokenResponse {
        access_token: String,
        refresh_token: Option<String>,
        expires_in: Option<u64>,
    }

    let tr: TokenResponse = resp
        .json()
        .await
        .map_err(|e| Error::Mcp(format!("token response parse failed: {e}")))?;

    let expires_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        + tr.expires_in.unwrap_or(3600);

    Ok(TokenSet {
        access_token: tr.access_token,
        refresh_token: tr.refresh_token,
        expires_at,
    })
}

/// Wait for the OAuth callback on the local listener, extract the authorization code.
async fn wait_for_callback(
    listener: tokio::net::TcpListener,
    expected_state: &str,
) -> Result<String> {
    use tokio::io::AsyncReadExt;

    let timeout = tokio::time::Duration::from_secs(120);
    let (mut stream, _) = tokio::time::timeout(timeout, listener.accept())
        .await
        .map_err(|_| Error::Mcp("OAuth callback timed out after 120s".into()))?
        .map_err(|e| Error::Mcp(format!("callback accept error: {e}")))?;

    let mut buf = vec![0u8; 4096];
    let n = stream
        .read(&mut buf)
        .await
        .map_err(|e| Error::Mcp(format!("callback read error: {e}")))?;
    let request = String::from_utf8_lossy(&buf[..n]);

    // Parse the GET request line to extract query params
    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .ok_or_else(|| Error::Mcp("malformed callback request".into()))?;

    let query = path
        .split('?')
        .nth(1)
        .ok_or_else(|| Error::Mcp("no query params in callback".into()))?;

    let mut code = None;
    let mut state = None;
    let mut error = None;
    for pair in query.split('&') {
        let mut kv = pair.splitn(2, '=');
        match (kv.next(), kv.next()) {
            (Some("code"), Some(v)) => code = Some(urldecoded(v)),
            (Some("state"), Some(v)) => state = Some(urldecoded(v)),
            (Some("error"), Some(v)) => error = Some(urldecoded(v)),
            _ => {}
        }
    }

    if let Some(err) = error {
        // Send error response
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n\
             <html><body><h2>Authorization failed: {err}</h2><p>You can close this tab.</p></body></html>"
        );
        let _ = stream.write_all(response.as_bytes()).await;
        return Err(Error::Mcp(format!("OAuth authorization denied: {err}")));
    }

    let code = code.ok_or_else(|| Error::Mcp("no code in callback".into()))?;
    let state = state.ok_or_else(|| Error::Mcp("no state in callback".into()))?;

    if state != expected_state {
        return Err(Error::Mcp("OAuth state mismatch".into()));
    }

    // Send success response
    let response = "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n\
         <html><body><h2>Symposium authorized!</h2><p>You can close this tab.</p></body></html>";
    let _ = stream.write_all(response.as_bytes()).await;

    Ok(code)
}

/// Heuristic: does this error come from the token endpoint rejecting the
/// refresh grant as revoked/expired? We can't pattern-match on structured
/// fields because `refresh_token` folds the body into a string, but the
/// OAuth 2.0 spec (RFC 6749 §5.2) mandates the `"error": "invalid_grant"`
/// code for this condition, and every server we've seen returns it verbatim.
fn is_invalid_grant(err: &Error) -> bool {
    let msg = err.to_string();
    msg.contains("invalid_grant")
}

fn generate_random_string(len: usize) -> String {
    let bytes: Vec<u8> = (0..len).map(|_| rand::rng().random::<u8>()).collect();
    URL_SAFE_NO_PAD.encode(&bytes)
}

fn generate_pkce_verifier() -> String {
    generate_random_string(32)
}

fn generate_pkce_challenge(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(digest)
}

fn urlencoded(s: &str) -> String {
    s.chars()
        .flat_map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => {
                vec![c]
            }
            _ => format!("%{:02X}", c as u8).chars().collect(),
        })
        .collect()
}

fn urldecoded(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '%' {
            let hex: String = chars.by_ref().take(2).collect();
            if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                result.push(byte as char);
            } else {
                result.push('%');
                result.push_str(&hex);
            }
        } else if c == '+' {
            result.push(' ');
        } else {
            result.push(c);
        }
    }
    result
}
