//! Single-machine harness: OpenAI and gateways registered in `models.json`.
//!
//! Providers are compiled into this binary. ahi does not install a Pi package
//! or a hook repository. OpenAI is the built-in public origin. Any other
//! gateway is an entry in the state directory's `models.json`.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

pub const OPENAI_ID: &str = "openai";
pub const OPENAI_ORIGIN: &str = "https://api.openai.com/v1";
const CATALOG_PATH: &str = "/pi/agent/model.json";
const BODY_CAP: u64 = 1024 * 1024;
const FETCH_TIMEOUT: Duration = Duration::from_secs(15);

/// Public OAuth client registered for the localhost callback used by browser
/// sign-in. The same client id is published in Pi's OpenAI Codex login.
const OPENAI_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const OPENAI_AUTHORIZE: &str = "https://auth.openai.com/oauth/authorize";
pub const OPENAI_TOKEN: &str = "https://auth.openai.com/oauth/token";
pub const OPENAI_REDIRECT: &str = "http://localhost:1455/auth/callback";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedProvider {
    pub id: String,
    pub base_url: String,
    pub models: Vec<String>,
    pub auth: String,
    pub catalog: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registry {
    pub providers: Vec<LoadedProvider>,
}

pub fn state_dir(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(path.to_path_buf());
    }
    if let Some(path) = std::env::var_os("AHI_STATE_DIR") {
        if !path.is_empty() {
            return Ok(PathBuf::from(path));
        }
    }
    if let Some(path) = std::env::var_os("XDG_STATE_HOME") {
        if !path.is_empty() {
            return Ok(PathBuf::from(path).join("ahi"));
        }
    }
    #[cfg(windows)]
    if let Some(path) = std::env::var_os("LOCALAPPDATA") {
        if !path.is_empty() {
            return Ok(PathBuf::from(path).join("ahi"));
        }
    }
    if let Some(path) = std::env::var_os("HOME") {
        if !path.is_empty() {
            return Ok(PathBuf::from(path).join(".local").join("share").join("ahi"));
        }
    }
    Err(Error::new(
        "no state directory: pass --state-dir, or set AHI_STATE_DIR, XDG_STATE_HOME, or HOME",
    ))
}

pub fn load_registry(state: &Path) -> Result<Registry> {
    let file = read_models(state)?;
    let mut providers = Vec::new();
    let mut openai_url = OPENAI_ORIGIN.to_string();
    if let Some(entry) = file.providers.get(OPENAI_ID) {
        check_entry(OPENAI_ID, entry)?;
        if !entry.models.is_empty() {
            return Err(Error::new(
                "the openai entry only overrides baseUrl and does not list models",
            ));
        }
        openai_url = entry.base_url.clone();
    }
    providers.push(LoadedProvider {
        id: OPENAI_ID.to_string(),
        base_url: openai_url,
        models: Vec::new(),
        auth: if credential_present(state) {
            "signed-in".to_string()
        } else {
            "signed-out".to_string()
        },
        catalog: "builtin".to_string(),
    });
    for (id, entry) in &file.providers {
        if id == OPENAI_ID {
            continue;
        }
        check_id(id)?;
        check_entry(id, entry)?;
        providers.push(load_gateway(state, id, entry)?);
    }
    Ok(Registry { providers })
}

pub fn render_text(registry: &Registry) -> String {
    let mut lines = Vec::new();
    for provider in &registry.providers {
        let models = if provider.models.is_empty() {
            "-".to_string()
        } else {
            provider.models.join(",")
        };
        lines.push(format!(
            "provider {} origin {} auth {} models {} catalog {}",
            provider.id, provider.base_url, provider.auth, models, provider.catalog
        ));
    }
    lines.join("\n")
}

fn credential_present(state: &Path) -> bool {
    state.join("credentials").join("openai.json").is_file()
}

fn read_models(state: &Path) -> Result<ModelsFile> {
    let path = state.join("models.json");
    if !path.exists() {
        return Ok(ModelsFile {
            providers: BTreeMap::new(),
        });
    }
    let text = fs::read_to_string(&path)
        .map_err(|err| Error::new(format!("failed to read {}: {err}", path.display())))?;
    if text.len() > BODY_CAP as usize {
        return Err(Error::new("models.json is larger than 1 MiB"));
    }
    serde_json::from_str(&text).map_err(|err| Error::new(format!("models.json: {err}")))
}

fn load_gateway(state: &Path, id: &str, entry: &ProviderEntry) -> Result<LoadedProvider> {
    let configured = entry
        .models
        .iter()
        .map(|model| model.id.clone())
        .collect::<Vec<_>>();
    let auth = match &entry.api_key {
        Some(key) if key == "gateway" => "marker",
        Some(_) => "env",
        None => "none",
    };
    match fetch_catalog(&entry.base_url, id) {
        Ok(models) => {
            write_last_good(state, id, &models)?;
            Ok(LoadedProvider {
                id: id.to_string(),
                base_url: entry.base_url.clone(),
                models,
                auth: auth.to_string(),
                catalog: "fetched".to_string(),
            })
        }
        Err(err) => {
            if !configured.is_empty() {
                Ok(LoadedProvider {
                    id: id.to_string(),
                    base_url: entry.base_url.clone(),
                    models: configured,
                    auth: auth.to_string(),
                    catalog: format!("kept-config ({err})"),
                })
            } else if let Some(models) = read_last_good(state, id)? {
                Ok(LoadedProvider {
                    id: id.to_string(),
                    base_url: entry.base_url.clone(),
                    models,
                    auth: auth.to_string(),
                    catalog: format!("last-good ({err})"),
                })
            } else {
                Ok(LoadedProvider {
                    id: id.to_string(),
                    base_url: entry.base_url.clone(),
                    models: Vec::new(),
                    auth: auth.to_string(),
                    catalog: format!("unavailable ({err})"),
                })
            }
        }
    }
}

fn catalog_url(base_url: &str) -> Result<String> {
    let origin = origin_of(base_url)?;
    Ok(format!("{origin}{CATALOG_PATH}"))
}

fn fetch_catalog(base_url: &str, gateway_id: &str) -> Result<Vec<String>> {
    let url = catalog_url(base_url)?;
    let bytes = http_get(&url)?;
    let catalog: Catalog = serde_json::from_slice(&bytes)
        .map_err(|err| Error::new(format!("catalog from {url}: {err}")))?;
    if catalog.api != "openai-completions" {
        return Err(Error::new(format!(
            "catalog from {url} uses api `{}`",
            catalog.api
        )));
    }
    check_marker("catalog apiKey", &catalog.api_key)?;
    let provider_id = match (&catalog.id, &catalog.provider) {
        (Some(id), Some(provider)) if id == provider => id.clone(),
        (Some(id), None) => id.clone(),
        (None, Some(provider)) => provider.clone(),
        (Some(id), Some(provider)) => {
            return Err(Error::new(format!(
                "catalog provider id `{id}` does not match `{provider}`"
            )));
        }
        (None, None) => {
            return Err(Error::new("catalog is missing a provider id"));
        }
    };
    if provider_id != gateway_id {
        return Err(Error::new(format!(
            "catalog provider id `{provider_id}` does not match gateway `{gateway_id}`"
        )));
    }
    let mut models = Vec::new();
    for model in catalog.models {
        if model.id.is_empty() || model.id.contains('\n') {
            return Err(Error::new("catalog model id is empty"));
        }
        if let Some(cost) = model.cost {
            if cost.input != 0 || cost.output != 0 || cost.cache_read != 0 || cost.cache_write != 0
            {
                return Err(Error::new(format!(
                    "catalog model `{}` has a non-zero cost",
                    model.id
                )));
            }
        }
        if let Some(input) = &model.input {
            for flag in input {
                if flag != "text" && flag != "image" {
                    return Err(Error::new(format!(
                        "catalog model `{}` has capability `{flag}`",
                        model.id
                    )));
                }
            }
        }
        models.push(model.id);
    }
    Ok(models)
}

fn http_get(url: &str) -> Result<Vec<u8>> {
    let agent = ureq::AgentBuilder::new()
        .redirects(0)
        .timeout_connect(FETCH_TIMEOUT)
        .timeout_read(FETCH_TIMEOUT)
        .timeout_write(FETCH_TIMEOUT)
        .build();
    let response = match agent.get(url).set("Accept", "application/json").call() {
        Ok(response) => response,
        Err(ureq::Error::Status(code, _)) if (300..400).contains(&code) => {
            return Err(Error::new(format!(
                "redirects are not followed ({code} from {url})"
            )));
        }
        Err(err) => return Err(Error::new(format!("catalog fetch {url}: {err}"))),
    };
    if (300..400).contains(&response.status()) {
        return Err(Error::new(format!(
            "redirects are not followed ({} from {url})",
            response.status()
        )));
    }
    if !(200..300).contains(&response.status()) {
        return Err(Error::new(format!(
            "catalog fetch {url}: status {}",
            response.status()
        )));
    }
    let mut buf = Vec::new();
    response
        .into_reader()
        .take(BODY_CAP + 1)
        .read_to_end(&mut buf)
        .map_err(|err| Error::new(format!("catalog body {url}: {err}")))?;
    if buf.len() as u64 > BODY_CAP {
        return Err(Error::new(format!(
            "catalog from {url} is larger than 1 MiB"
        )));
    }
    Ok(buf)
}

fn write_last_good(state: &Path, id: &str, models: &[String]) -> Result<()> {
    let dir = state.join("catalogs");
    fs::create_dir_all(&dir)
        .map_err(|err| Error::new(format!("failed to create {}: {err}", dir.display())))?;
    let path = dir.join(format!("{id}.json"));
    let body = serde_json::to_vec(models).map_err(|err| Error::new(err.to_string()))?;
    fs::write(&path, body)
        .map_err(|err| Error::new(format!("failed to write {}: {err}", path.display())))?;
    Ok(())
}

fn read_last_good(state: &Path, id: &str) -> Result<Option<Vec<String>>> {
    let path = state.join("catalogs").join(format!("{id}.json"));
    if !path.is_file() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path)
        .map_err(|err| Error::new(format!("failed to read {}: {err}", path.display())))?;
    let models = serde_json::from_str(&text)
        .map_err(|err| Error::new(format!("last-good catalog for `{id}`: {err}")))?;
    Ok(Some(models))
}

fn check_id(id: &str) -> Result<()> {
    if id.is_empty()
        || !id.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_' || byte == b'.'
        })
    {
        return Err(Error::new(format!(
            "gateway id `{id}` is not an ASCII identifier"
        )));
    }
    Ok(())
}

fn check_entry(id: &str, entry: &ProviderEntry) -> Result<()> {
    if entry.api != "openai-completions" {
        return Err(Error::new(format!(
            "gateway `{id}` api `{}` is not openai-completions",
            entry.api
        )));
    }
    origin_of(&entry.base_url).map_err(|err| Error::new(format!("gateway `{id}`: {err}")))?;
    if let Some(key) = &entry.api_key {
        check_marker(&format!("gateway `{id}` apiKey"), key)?;
    }
    for (name, value) in &entry.headers {
        check_marker(&format!("gateway `{id}` header `{name}`"), value)?;
    }
    for model in &entry.models {
        if model.id.is_empty() {
            return Err(Error::new(format!("gateway `{id}` has an empty model id")));
        }
    }
    Ok(())
}

fn check_marker(label: &str, value: &str) -> Result<()> {
    if value == "gateway" {
        return Ok(());
    }
    let Some(name) = value.strip_prefix('$') else {
        return Err(Error::new(format!(
            "{label} must be the marker `gateway` or an environment reference `$NAME`"
        )));
    };
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
        || name.as_bytes()[0].is_ascii_digit()
    {
        return Err(Error::new(format!(
            "{label} environment reference `{value}` is not `$NAME`"
        )));
    }
    Ok(())
}

fn origin_of(base_url: &str) -> Result<String> {
    let rest = if let Some(rest) = base_url.strip_prefix("https://") {
        rest
    } else if let Some(rest) = base_url.strip_prefix("http://") {
        rest
    } else {
        return Err(Error::new(format!(
            "baseUrl `{base_url}` is not http or https"
        )));
    };
    if rest.contains('@') {
        return Err(Error::new("baseUrl must not contain userinfo"));
    }
    let (authority, _) = rest.split_once('/').unwrap_or((rest, ""));
    if authority.is_empty() {
        return Err(Error::new("baseUrl is missing a host"));
    }
    let host = if let Some(host) = authority.strip_prefix('[') {
        host.split_once(']')
            .map(|(host, _)| host)
            .ok_or_else(|| Error::new("baseUrl has a broken IPv6 host"))?
    } else {
        authority.split(':').next().unwrap_or(authority)
    };
    let loopback = host == "localhost" || host == "127.0.0.1" || host == "::1";
    if base_url.starts_with("http://") && !loopback {
        return Err(Error::new(format!(
            "baseUrl `{base_url}` is http outside loopback"
        )));
    }
    let scheme = if base_url.starts_with("https://") {
        "https"
    } else {
        "http"
    };
    Ok(format!("{scheme}://{authority}"))
}

pub fn authorization_url(verifier: &str, state: &str) -> String {
    let challenge = base64url(&Sha256::digest(verifier.as_bytes()));
    let mut url = String::from(OPENAI_AUTHORIZE);
    url.push('?');
    for (key, value) in [
        ("response_type", "code"),
        ("client_id", OPENAI_CLIENT_ID),
        ("redirect_uri", OPENAI_REDIRECT),
        ("scope", "openid profile email offline_access"),
        ("code_challenge", &challenge),
        ("code_challenge_method", "S256"),
        ("state", state),
        ("id_token_add_organizations", "true"),
        ("codex_cli_simplified_flow", "true"),
        ("originator", "ahi"),
    ] {
        if !url.ends_with('?') {
            url.push('&');
        }
        url.push_str(key);
        url.push('=');
        url.push_str(&form_escape(value));
    }
    url
}

pub fn login(state: &Path) -> Result<()> {
    let verifier = random_token(32)?;
    let oauth_state = random_token(16)?;
    let url = authorization_url(&verifier, &oauth_state);
    eprintln!("{url}");
    try_open_browser(&url);
    eprintln!(
        "waiting for {OPENAI_REDIRECT}. Paste the redirect URL or the authorization code and press enter."
    );
    let code = wait_for_code(&oauth_state)?;
    let token = exchange_code(OPENAI_TOKEN, &code, &verifier)?;
    write_credential(state, &token)
}

fn wait_for_code(expected_state: &str) -> Result<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    let callback_state = expected_state.to_string();
    let callback_tx = tx.clone();
    std::thread::spawn(move || match accept_callback(1455, &callback_state) {
        Ok(code) => {
            let _ = callback_tx.send(Ok(code));
        }
        Err(err) => eprintln!("callback: {err}"),
    });
    let paste_state = expected_state.to_string();
    std::thread::spawn(move || {
        let mut line = String::new();
        if let Err(err) = std::io::stdin().read_line(&mut line) {
            let _ = tx.send(Err(Error::new(format!(
                "failed to read login paste: {err}"
            ))));
            return;
        }
        if line.trim().is_empty() {
            return;
        }
        let _ = tx.send(code_from_paste(&line, &paste_state));
    });
    match rx.recv() {
        Ok(result) => result,
        Err(_) => Err(Error::new(
            "login ended before a browser redirect or a pasted code",
        )),
    }
}

pub fn accept_callback(port: u16, expected_state: &str) -> Result<String> {
    let listener = TcpListener::bind(("127.0.0.1", port))
        .map_err(|err| Error::new(format!("login callback on 127.0.0.1:{port}: {err}")))?;
    listener
        .set_nonblocking(false)
        .map_err(|err| Error::new(err.to_string()))?;
    let (mut stream, _) = listener
        .accept()
        .map_err(|err| Error::new(format!("login callback: {err}")))?;
    stream
        .set_read_timeout(Some(FETCH_TIMEOUT))
        .map_err(|err| Error::new(err.to_string()))?;
    let mut buf = vec![0_u8; 8192];
    let n = stream
        .read(&mut buf)
        .map_err(|err| Error::new(format!("login callback read: {err}")))?;
    let request = String::from_utf8_lossy(&buf[..n]);
    let code = code_from_request(&request, expected_state)?;
    let body = b"ahi login received the browser redirect. You can close this tab.\n";
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\nContent-Type: text/plain\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.write_all(body);
    Ok(code)
}

fn code_from_request(request: &str, expected_state: &str) -> Result<String> {
    let line = request
        .lines()
        .next()
        .ok_or_else(|| Error::new("login callback was empty"))?;
    let path = line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| Error::new("login callback had no path"))?;
    code_from_paste(path, expected_state)
}

pub fn code_from_paste(text: &str, expected_state: &str) -> Result<String> {
    let text = text.trim();
    if text.is_empty() {
        return Err(Error::new("login paste was empty"));
    }
    let query = text.split_once('?').map(|(_, query)| query).unwrap_or(text);
    let mut code = None;
    let mut state = None;
    for pair in query.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            if code.is_none() && !pair.contains('/') {
                code = Some(pair.to_string());
            }
            continue;
        };
        let value = form_unescape(value);
        match key {
            "code" => code = Some(value),
            "state" => state = Some(value),
            _ => {}
        }
    }
    if let Some(state) = state {
        if state != expected_state {
            return Err(Error::new("login state did not match"));
        }
    } else if text.contains('?') {
        return Err(Error::new("login redirect is missing state"));
    }
    code.ok_or_else(|| Error::new("login redirect is missing code"))
}

pub fn exchange_code(token_url: &str, code: &str, verifier: &str) -> Result<Value> {
    let body = format!(
        "grant_type=authorization_code&client_id={}&code={}&redirect_uri={}&code_verifier={}",
        form_escape(OPENAI_CLIENT_ID),
        form_escape(code),
        form_escape(OPENAI_REDIRECT),
        form_escape(verifier)
    );
    let agent = ureq::AgentBuilder::new()
        .redirects(0)
        .timeout(FETCH_TIMEOUT)
        .build();
    let response = agent
        .post(token_url)
        .set("Content-Type", "application/x-www-form-urlencoded")
        .send_string(&body)
        .map_err(|err| Error::new(format!("token exchange failed: {err}")))?;
    let mut body = String::new();
    response
        .into_reader()
        .take(BODY_CAP)
        .read_to_string(&mut body)
        .map_err(|err| Error::new(format!("token response: {err}")))?;
    let parsed: Value =
        serde_json::from_str(&body).map_err(|err| Error::new(format!("token response: {err}")))?;
    let token = parsed
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::new("token response has no access_token"))?;
    if token.is_empty() {
        return Err(Error::new("token response has an empty access_token"));
    }
    Ok(parsed)
}

fn write_credential(state: &Path, token: &Value) -> Result<()> {
    let dir = state.join("credentials");
    fs::create_dir_all(&dir)
        .map_err(|err| Error::new(format!("failed to create {}: {err}", dir.display())))?;
    let path = dir.join("openai.json");
    let stored = serde_json::json!({
        "access_token": token.get("access_token"),
        "refresh_token": token.get("refresh_token"),
        "expires_in": token.get("expires_in"),
    });
    let mut options = OpenOptions::new();
    options.create(true).write(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&path)
        .map_err(|err| Error::new(format!("failed to write {}: {err}", path.display())))?;
    file.write_all(stored.to_string().as_bytes())
        .map_err(|err| Error::new(format!("failed to write {}: {err}", path.display())))?;
    Ok(())
}

fn try_open_browser(url: &str) {
    let mut command = if cfg!(windows) {
        let mut command = std::process::Command::new("cmd");
        command.args(["/C", "start", "", url]);
        command
    } else if cfg!(target_os = "macos") {
        let mut command = std::process::Command::new("open");
        command.arg(url);
        command
    } else {
        let mut command = std::process::Command::new("xdg-open");
        command.arg(url);
        command
    };
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let _ = command.spawn();
}

fn random_token(bytes: usize) -> Result<String> {
    let mut buf = vec![0_u8; bytes];
    getrandom::getrandom(&mut buf).map_err(|err| Error::new(format!("random: {err}")))?;
    Ok(base64url(&buf))
}

fn base64url(data: &[u8]) -> String {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    let mut index = 0;
    while index + 3 <= data.len() {
        let n =
            ((data[index] as u32) << 16) | ((data[index + 1] as u32) << 8) | data[index + 2] as u32;
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        out.push(TABLE[((n >> 6) & 63) as usize] as char);
        out.push(TABLE[(n & 63) as usize] as char);
        index += 3;
    }
    if index < data.len() {
        let rest = data.len() - index;
        let mut n = (data[index] as u32) << 16;
        if rest == 2 {
            n |= (data[index + 1] as u32) << 8;
        }
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        if rest == 2 {
            out.push(TABLE[((n >> 6) & 63) as usize] as char);
        }
    }
    out
}

fn form_escape(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn form_unescape(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(
                std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or(""),
                16,
            ) {
                out.push(byte);
                index += 3;
                continue;
            }
        }
        out.push(if bytes[index] == b'+' {
            b' '
        } else {
            bytes[index]
        });
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelsFile {
    #[serde(default)]
    providers: BTreeMap<String, ProviderEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderEntry {
    #[serde(rename = "baseUrl")]
    base_url: String,
    api: String,
    #[serde(default, rename = "apiKey")]
    api_key: Option<String>,
    #[serde(default)]
    models: Vec<ModelId>,
    #[serde(default)]
    headers: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelId {
    id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Catalog {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    provider: Option<String>,
    api: String,
    #[serde(rename = "apiKey")]
    api_key: String,
    models: Vec<CatalogModel>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogModel {
    id: String,
    #[serde(default)]
    cost: Option<Cost>,
    #[serde(default, rename = "reasoning")]
    _reasoning: Option<bool>,
    #[serde(default)]
    input: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cost {
    input: u64,
    output: u64,
    #[serde(default, rename = "cacheRead")]
    cache_read: u64,
    #[serde(default, rename = "cacheWrite")]
    cache_write: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    fn serve(status: &str, extra: &str, body: &[u8]) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let status = status.to_string();
        let extra = extra.to_string();
        let body = body.to_vec();
        thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = [0_u8; 1024];
            let _ = sock.read(&mut buf);
            let header = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n",
                body.len()
            );
            sock.write_all(header.as_bytes()).unwrap();
            sock.write_all(&body).unwrap();
        });
        port
    }

    #[test]
    fn authorization_url_names_ahi_and_the_localhost_callback() {
        let url = authorization_url("verifier-value", "state-value");
        assert!(url.starts_with("https://auth.openai.com/oauth/authorize?"));
        assert!(url.contains("client_id=app_EMoamEEZ73f0CkXaXp7hrann"));
        assert!(url.contains("redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("originator=ahi"));
        assert!(url.contains("state=state-value"));
        assert!(!url.contains("verifier-value"));
    }

    #[test]
    fn http_is_only_allowed_on_loopback() {
        assert!(origin_of("http://127.0.0.1:11434/v1").is_ok());
        assert!(origin_of("http://example.com/v1").is_err());
        assert!(origin_of("https://example.com/v1").is_ok());
    }

    #[test]
    fn a_secret_api_key_is_refused() {
        let err = check_marker("apiKey", "sk-live-secret").unwrap_err();
        assert!(err.to_string().contains("$NAME"));
        assert!(check_marker("apiKey", "$LAB_API_KEY").is_ok());
    }

    #[test]
    fn catalog_fetch_rejects_a_redirect_a_mismatch_and_keeps_config() {
        let dir = crate::fixture_directory::create_unique_fixture_directory("harness");
        let redirect_port = serve("302 Found", "Location: http://127.0.0.1/\r\n", b"");
        fs::write(
            dir.join("models.json"),
            format!(
                r#"{{"providers":{{"lab":{{"baseUrl":"http://127.0.0.1:{redirect_port}/v1","api":"openai-completions","models":[{{"id":"kept"}}]}}}}}}"#
            ),
        )
        .unwrap();
        let registry = load_registry(&dir).unwrap();
        let lab = registry
            .providers
            .iter()
            .find(|provider| provider.id == "lab")
            .unwrap();
        assert_eq!(lab.models, vec!["kept".to_string()]);
        assert!(lab.catalog.contains("kept-config"), "{}", lab.catalog);
        assert!(lab.catalog.contains("redirects are not followed"));

        let mismatch = br#"{"id":"other","api":"openai-completions","apiKey":"gateway","models":[{"id":"m","cost":{"input":0,"output":0}}]}"#;
        let mismatch_port = serve("200 OK", "", mismatch);
        fs::write(
            dir.join("models.json"),
            format!(
                r#"{{"providers":{{"lab":{{"baseUrl":"http://127.0.0.1:{mismatch_port}/v1","api":"openai-completions"}}}}}}"#
            ),
        )
        .unwrap();
        let registry = load_registry(&dir).unwrap();
        let lab = registry
            .providers
            .iter()
            .find(|provider| provider.id == "lab")
            .unwrap();
        assert!(lab.models.is_empty());
        assert!(lab.catalog.contains("does not match"));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn catalog_fetch_records_models_and_a_later_failure_uses_last_good() {
        let dir = crate::fixture_directory::create_unique_fixture_directory("catalog");
        let body = br#"{"provider":"lab","api":"openai-completions","apiKey":"gateway","models":[{"id":"qwen-3.5","reasoning":false,"input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0}}]}"#;
        let port = serve("200 OK", "", body);
        fs::write(
            dir.join("models.json"),
            format!(
                r#"{{"providers":{{"lab":{{"baseUrl":"http://127.0.0.1:{port}/v1","api":"openai-completions"}}}}}}"#
            ),
        )
        .unwrap();
        let registry = load_registry(&dir).unwrap();
        let lab = registry
            .providers
            .iter()
            .find(|provider| provider.id == "lab")
            .unwrap();
        assert_eq!(lab.models, vec!["qwen-3.5".to_string()]);
        assert_eq!(lab.catalog, "fetched");
        let openai = &registry.providers[0];
        assert_eq!(openai.id, "openai");
        assert_eq!(openai.auth, "signed-out");

        fs::write(
            dir.join("models.json"),
            r#"{"providers":{"lab":{"baseUrl":"http://127.0.0.1:1/v1","api":"openai-completions"}}}"#,
        )
        .unwrap();
        let registry = load_registry(&dir).unwrap();
        let lab = registry
            .providers
            .iter()
            .find(|provider| provider.id == "lab")
            .unwrap();
        assert_eq!(lab.models, vec!["qwen-3.5".to_string()]);
        assert!(lab.catalog.starts_with("last-good"), "{}", lab.catalog);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn token_exchange_stores_a_credential_without_printing_it() {
        let dir = crate::fixture_directory::create_unique_fixture_directory("token");
        let body = br#"{"access_token":"token-value","refresh_token":"refresh-value","expires_in":3,"extra":true}"#;
        let port = serve("200 OK", "Content-Type: application/json\r\n", body);
        let token = exchange_code(
            &format!("http://127.0.0.1:{port}/token"),
            "code",
            "verifier",
        )
        .unwrap();
        write_credential(&dir, &token).unwrap();
        let stored = fs::read_to_string(dir.join("credentials/openai.json")).unwrap();
        assert!(stored.contains("token-value"));
        assert!(!stored.contains("extra"));
        let registry = load_registry(&dir).unwrap();
        assert_eq!(registry.providers[0].auth, "signed-in");
        let text = render_text(&registry);
        assert!(!text.contains("token-value"));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn paste_checks_state() {
        assert!(code_from_paste(
            "http://localhost:1455/auth/callback?code=abc&state=ok",
            "ok"
        )
        .is_ok());
        assert!(code_from_paste(
            "http://localhost:1455/auth/callback?code=abc&state=no",
            "ok"
        )
        .is_err());
    }

    #[test]
    fn callback_reads_the_code_and_rejects_a_mismatched_state() {
        let port = free_port();
        let handle = thread::spawn(move || accept_callback(port, "ok"));
        let mut client = connect_when_ready(port);
        client
            .write_all(b"GET /auth/callback?code=abc&state=ok HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .unwrap();
        assert_eq!(handle.join().unwrap().unwrap(), "abc");

        let port = free_port();
        let handle = thread::spawn(move || accept_callback(port, "ok"));
        let mut client = connect_when_ready(port);
        client
            .write_all(b"GET /auth/callback?code=abc&state=no HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .unwrap();
        let err = handle.join().unwrap().unwrap_err();
        assert!(err.to_string().contains("state"), "{err}");
    }

    fn free_port() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        port
    }

    fn connect_when_ready(port: u16) -> std::net::TcpStream {
        for _ in 0..50 {
            if let Ok(stream) = std::net::TcpStream::connect(("127.0.0.1", port)) {
                return stream;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("callback did not listen on {port}");
    }
}
