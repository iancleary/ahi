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

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

mod curation;
mod openai;

pub use curation::save_curated_models;

pub const OPENAI_ID: &str = "openai";
pub const OPENAI_ORIGIN: &str = "https://api.openai.com/v1";
const CATALOG_PATH: &str = "/pi/agent/model.json";
const BODY_CAP: u64 = 1024 * 1024;
const FETCH_TIMEOUT: Duration = Duration::from_secs(15);

// Current Sign in with ChatGPT flow (not the legacy Codex public client).
const OPENAI_CLIENT_ID: &str = "dynamic_agent_client";
const OPENAI_AUTHORIZE: &str = "https://auth.openai.com/api/accounts/authorize";
pub const OPENAI_TOKEN: &str = "https://auth.openai.com/api/accounts/oauth/token";
pub const OPENAI_REDIRECT: &str = "http://127.0.0.1:1455/auth/callback";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedProvider {
    pub id: String,
    pub base_url: String,
    pub models: Vec<ModelChoice>,
    pub auth: String,
    pub catalog: Option<CatalogSnapshot>,
    pub catalog_error: Option<String>,
    pub empty_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelChoice {
    pub id: String,
    pub status: CatalogMembership,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogMembership {
    Listed,
    Absent,
    Unavailable,
}

impl std::fmt::Display for CatalogMembership {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Listed => "listed",
            Self::Absent => "absent from catalog",
            Self::Unavailable => "catalog unavailable",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogSnapshot {
    pub provider: String,
    pub source: Option<String>,
    pub retrieved_at: Option<String>,
    pub models: Vec<String>,
}

impl LoadedProvider {
    pub fn catalog_age(&self) -> String {
        match self
            .catalog
            .as_ref()
            .and_then(|catalog| catalog.retrieved_at.as_deref())
        {
            None if self.catalog.is_none() => "unavailable".to_string(),
            None => "retrieval time unknown (old catalog format)".to_string(),
            Some(stamp) => {
                let age = time::OffsetDateTime::parse(
                    stamp,
                    &time::format_description::well_known::Rfc3339,
                )
                .map(|then| {
                    (time::OffsetDateTime::now_utc() - then)
                        .whole_seconds()
                        .max(0)
                })
                .unwrap_or(0);
                format!("{age}s old (retrieved {stamp})")
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registry {
    pub providers: Vec<LoadedProvider>,
    // Exact local config observed at load time, used to reject stale curation saves.
    models_bytes: Option<Vec<u8>>,
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
    let models_bytes = read_models_bytes(state)?;
    let file = parse_models(state, models_bytes.as_deref())?;
    Ok(registry_from_models(state, file, models_bytes))
}

fn registry_from_models(state: &Path, file: ModelsFile, models_bytes: Option<Vec<u8>>) -> Registry {
    let mut providers = Vec::new();
    let openai = file.providers.get(OPENAI_ID);
    providers.push(load_provider(
        state,
        OPENAI_ID,
        openai
            .map(|entry| entry.base_url.as_str())
            .unwrap_or(OPENAI_ORIGIN),
        openai
            .map(|entry| entry.models.as_slice())
            .unwrap_or_default(),
        if state.join("credentials/openai.json").is_file() {
            "signed-in"
        } else {
            "signed-out"
        },
    ));
    for (id, entry) in &file.providers {
        if id == OPENAI_ID {
            continue;
        }
        let auth = if entry
            .api_key
            .iter()
            .chain(entry.headers.values())
            .any(|value| value.starts_with('$'))
        {
            "env"
        } else if entry.api_key.is_some() || !entry.headers.is_empty() {
            "marker"
        } else {
            "none"
        };
        providers.push(load_provider(
            state,
            id,
            &entry.base_url,
            &entry.models,
            auth,
        ));
    }
    Registry {
        providers,
        models_bytes,
    }
}

pub fn render_text(registry: &Registry) -> String {
    let mut lines = Vec::new();
    for provider in &registry.providers {
        let models = if provider.models.is_empty() {
            "-".to_string()
        } else {
            provider
                .models
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>()
                .join(",")
        };
        lines.push(format!(
            "provider {} origin {} auth {} models {} catalog {}",
            provider.id,
            provider.base_url,
            provider.auth,
            models,
            provider.catalog_age()
        ));
        for model in &provider.models {
            lines.push(format!("  model {} status {}", model.id, model.status));
        }
        if let Some(reason) = &provider.empty_reason {
            lines.push(format!("  {reason}"));
        }
        if let Some(error) = &provider.catalog_error {
            lines.push(format!("  catalog error: {error}"));
        }
    }
    lines.join("\n")
}

fn read_models(state: &Path) -> Result<ModelsFile> {
    parse_models(state, read_models_bytes(state)?.as_deref())
}

fn read_models_bytes(state: &Path) -> Result<Option<Vec<u8>>> {
    let path = state.join("models.json");
    let file = match fs::File::open(&path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(Error::new(format!(
                "failed to read {}: {err}",
                path.display()
            )))
        }
    };
    let mut bytes = Vec::new();
    file.take(BODY_CAP + 1)
        .read_to_end(&mut bytes)
        .map_err(|err| Error::new(format!("failed to read {}: {err}", path.display())))?;
    if bytes.len() > BODY_CAP as usize {
        return Err(Error::new("models.json is larger than 1 MiB"));
    }
    Ok(Some(bytes))
}

fn parse_models(state: &Path, bytes: Option<&[u8]>) -> Result<ModelsFile> {
    match bytes {
        None => Ok(ModelsFile {
            providers: BTreeMap::new(),
        }),
        Some(bytes) => serde_json::from_slice(bytes)
            .map_err(|err| Error::new(format!("{}: {err}", state.join("models.json").display()))),
    }
}

fn load_provider(
    state: &Path,
    id: &str,
    base_url: &str,
    configured: &[ModelId],
    auth: &str,
) -> LoadedProvider {
    let (catalog, catalog_error) = match read_last_good(state, id) {
        Ok(catalog) => (catalog, None),
        Err(err) => (None, Some(err.to_string())),
    };
    let models = configured
        .iter()
        .map(|model| ModelChoice {
            id: model.id.clone(),
            status: match &catalog {
                None => CatalogMembership::Unavailable,
                Some(catalog) if catalog.models.contains(&model.id) => CatalogMembership::Listed,
                Some(_) => CatalogMembership::Absent,
            },
        })
        .collect();
    LoadedProvider {
        id: id.to_string(),
        base_url: base_url.to_string(),
        models,
        auth: auth.to_string(),
        catalog,
        catalog_error,
        empty_reason: configured.is_empty().then(|| {
            "no models in models.json: press e to edit, c to browse the catalog".to_string()
        }),
    }
}

/// Explicitly refresh one reference catalog. Never edits models.json.
pub fn refresh_provider(state: &Path, id: &str) -> Result<()> {
    check_id(id)?;
    let file = read_models(state)?;
    let (source, models) = if id == OPENAI_ID {
        let base_url = file
            .providers
            .get(id)
            .map(|entry| entry.base_url.as_str())
            .unwrap_or(OPENAI_ORIGIN);
        (
            openai::catalog_source(base_url)?,
            openai::refresh(state, base_url)?,
        )
    } else {
        let entry = file
            .providers
            .get(id)
            .ok_or_else(|| Error::new(format!("unknown provider `{id}`")))?;
        (
            catalog_url(&entry.base_url)?,
            fetch_catalog(&entry.base_url, id)?,
        )
    };
    write_last_good(
        state,
        &CatalogSnapshot {
            provider: id.to_string(),
            source: Some(source),
            retrieved_at: Some(
                time::OffsetDateTime::now_utc()
                    .format(&time::format_description::well_known::Rfc3339)
                    .map_err(|err| Error::new(format!("catalog timestamp: {err}")))?,
            ),
            models,
        },
    )
}

/// Refresh every provider, continuing after failures. Errors are for this refresh,
/// not persisted in the catalog (a failed refresh leaves that file untouched).
pub fn refresh_all(state: &Path) -> Result<BTreeMap<String, String>> {
    let registry = load_registry(state)?;
    let mut errors = BTreeMap::new();
    for provider in registry.providers {
        if let Err(err) = refresh_provider(state, &provider.id) {
            errors.insert(provider.id, err.to_string());
        }
    }
    Ok(errors)
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
        if !valid_model_id(&model.id) {
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

fn write_last_good(state: &Path, catalog: &CatalogSnapshot) -> Result<()> {
    let dir = state.join("catalogs");
    fs::create_dir_all(&dir)
        .map_err(|err| Error::new(format!("failed to create {}: {err}", dir.display())))?;
    let path = dir.join(format!("{}.json", catalog.provider));
    let temp = path.with_extension("json.tmp");
    let body = serde_json::to_vec_pretty(catalog).map_err(|err| Error::new(err.to_string()))?;
    if body.len() as u64 > BODY_CAP {
        return Err(Error::new("stored catalog is larger than 1 MiB"));
    }
    // Exclusive creation prevents concurrent refreshes from sharing a temp file.
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(|err| {
            Error::new(format!(
                "cannot create {} (another refresh or stale temp file?): {err}",
                temp.display()
            ))
        })?;
    let result = (|| -> std::io::Result<()> {
        file.write_all(&body)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, &path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result.map_err(|err| Error::new(format!("failed to replace {}: {err}", path.display())))
}

fn read_last_good(state: &Path, id: &str) -> Result<Option<CatalogSnapshot>> {
    let path = state.join("catalogs").join(format!("{id}.json"));
    let file = match fs::File::open(&path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(Error::new(format!("{}: {err}", path.display()))),
    };
    let mut bytes = Vec::new();
    file.take(BODY_CAP + 1)
        .read_to_end(&mut bytes)
        .map_err(|err| Error::new(format!("{}: {err}", path.display())))?;
    if bytes.len() as u64 > BODY_CAP {
        return Err(Error::new(format!("{}: larger than 1 MiB", path.display())));
    }
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Stored {
        Current(CatalogSnapshot),
        Legacy(Vec<String>),
    }
    let catalog = match serde_json::from_slice(&bytes)
        .map_err(|err| Error::new(format!("{}: {err}", path.display())))?
    {
        Stored::Current(catalog) => catalog,
        Stored::Legacy(models) => CatalogSnapshot {
            provider: id.to_string(),
            source: None,
            retrieved_at: None,
            models,
        },
    };
    if catalog.provider != id || catalog.models.iter().any(|id| !valid_model_id(id)) {
        return Err(Error::new(format!(
            "{}: invalid provider or model id",
            path.display()
        )));
    }
    if let Some(stamp) = &catalog.retrieved_at {
        time::OffsetDateTime::parse(stamp, &time::format_description::well_known::Rfc3339)
            .map_err(|_| {
                Error::new(format!(
                    "{}: invalid retrieved_at timestamp",
                    path.display()
                ))
            })?;
    }
    Ok(Some(catalog))
}

fn valid_model_id(id: &str) -> bool {
    !id.is_empty() && !id.chars().any(|ch| ch.is_control() || ch.is_whitespace())
}

fn check_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id == "."
        || id == ".."
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
        if !valid_model_id(&model.id) {
            return Err(Error::new(format!(
                "gateway `{id}` has an invalid model id"
            )));
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

pub fn login(state: &Path) -> Result<()> {
    openai::login(state)
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
    #[serde(default, deserialize_with = "deserialize_providers")]
    providers: BTreeMap<String, ProviderEntry>,
}

// Validate while serde still has the input position, so validation errors carry
// real JSON line/column locations rather than guessed string-search offsets.
fn deserialize_providers<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<BTreeMap<String, ProviderEntry>, D::Error> {
    use serde::de::{Error as _, MapAccess, Visitor};
    struct Providers;
    impl<'de> Visitor<'de> for Providers {
        type Value = BTreeMap<String, ProviderEntry>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a provider map")
        }

        fn visit_map<A: MapAccess<'de>>(
            self,
            mut map: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            let mut providers = BTreeMap::new();
            while let Some(id) = map.next_key::<String>()? {
                check_id(&id).map_err(A::Error::custom)?;
                let entry = map.next_value::<ProviderEntry>()?;
                check_entry(&id, &entry).map_err(A::Error::custom)?;
                if providers.insert(id.clone(), entry).is_some() {
                    return Err(A::Error::custom(format!("duplicate provider `{id}`")));
                }
            }
            Ok(providers)
        }
    }
    deserializer.deserialize_map(Providers)
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
            // A short read leaves request bytes queued. Closing that socket makes
            // macOS reset the connection, and the client's next read returns
            // EINVAL (os error 22) instead of the status line.
            sock.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            sock.set_nodelay(true).unwrap();
            read_http_request(&mut sock);
            let header = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n",
                body.len()
            );
            sock.write_all(header.as_bytes()).unwrap();
            sock.write_all(&body).unwrap();
            sock.flush().unwrap();
            let _ = sock.shutdown(std::net::Shutdown::Write);
            let mut discard = [0_u8; 1024];
            loop {
                match sock.read(&mut discard) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        });
        port
    }

    fn read_http_request(sock: &mut std::net::TcpStream) {
        let mut buf = Vec::new();
        let mut tmp = [0_u8; 1024];
        let header_end = loop {
            let n = sock.read(&mut tmp).unwrap_or(0);
            if n == 0 {
                return;
            }
            buf.extend_from_slice(&tmp[..n]);
            if let Some(pos) = buf.windows(4).position(|window| window == b"\r\n\r\n") {
                break pos + 4;
            }
            if buf.len() > 64 * 1024 {
                return;
            }
        };
        let headers = String::from_utf8_lossy(&buf[..header_end]);
        let mut content_length = 0usize;
        for line in headers.lines() {
            let Some((name, value)) = line.split_once(':') else {
                continue;
            };
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse().unwrap_or(0);
            }
        }
        let mut received = buf.len() - header_end;
        while received < content_length {
            let n = sock.read(&mut tmp).unwrap_or(0);
            if n == 0 {
                break;
            }
            received += n;
        }
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
    fn refresh_rejects_a_redirect_and_a_provider_mismatch_without_changing_curated_ids() {
        let dir = crate::fixture_directory::create_unique_fixture_directory("harness");
        let redirect_port = serve("302 Found", "Location: http://127.0.0.1/\r\n", b"");
        fs::write(
            dir.join("models.json"),
            format!(
                r#"{{"providers":{{"lab":{{"baseUrl":"http://127.0.0.1:{redirect_port}/v1","api":"openai-completions","models":[{{"id":"kept"}}]}}}}}}"#
            ),
        )
        .unwrap();
        let error = refresh_provider(&dir, "lab").unwrap_err().to_string();
        assert!(error.contains("redirects are not followed"), "{error}");
        let registry = load_registry(&dir).unwrap();
        let lab = &registry.providers[1];
        assert_eq!(
            lab.models,
            vec![ModelChoice {
                id: "kept".to_string(),
                status: CatalogMembership::Unavailable
            }]
        );

        let mismatch = br#"{"id":"other","api":"openai-completions","apiKey":"gateway","models":[{"id":"m","cost":{"input":0,"output":0}}]}"#;
        let mismatch_port = serve("200 OK", "", mismatch);
        fs::write(
            dir.join("models.json"),
            format!(
                r#"{{"providers":{{"lab":{{"baseUrl":"http://127.0.0.1:{mismatch_port}/v1","api":"openai-completions"}}}}}}"#
            ),
        )
        .unwrap();
        assert!(refresh_provider(&dir, "lab")
            .unwrap_err()
            .to_string()
            .contains("does not match"));
        let registry = load_registry(&dir).unwrap();
        let lab = &registry.providers[1];
        assert!(lab.models.is_empty());
        assert_eq!(
            lab.empty_reason.as_deref(),
            Some("no models in models.json: press e to edit, c to browse the catalog")
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn refresh_never_fills_curated_list_and_failure_preserves_catalog_bytes() {
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
        refresh_provider(&dir, "lab").unwrap();
        let saved = fs::read(dir.join("catalogs/lab.json")).unwrap();
        let registry = load_registry(&dir).unwrap();
        let lab = &registry.providers[1];
        assert!(lab.models.is_empty());
        assert_eq!(lab.catalog.as_ref().unwrap().models, ["qwen-3.5"]);
        assert!(lab
            .empty_reason
            .as_ref()
            .unwrap()
            .contains("no models in models.json"));
        let openai = &registry.providers[0];
        assert_eq!(openai.id, "openai");
        assert_eq!(openai.auth, "signed-out");

        fs::write(
            dir.join("models.json"),
            r#"{"providers":{"lab":{"baseUrl":"http://127.0.0.1:1/v1","api":"openai-completions"}}}"#,
        )
        .unwrap();
        assert!(refresh_provider(&dir, "lab").is_err());
        assert_eq!(fs::read(dir.join("catalogs/lab.json")).unwrap(), saved);
        let registry = load_registry(&dir).unwrap();
        let lab = &registry.providers[1];
        assert!(lab.models.is_empty());
        assert_eq!(lab.catalog.as_ref().unwrap().models, ["qwen-3.5"]);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn load_is_local_and_curated_ids_keep_order_and_catalog_membership() {
        let dir = crate::fixture_directory::create_unique_fixture_directory("curated-local");
        fs::create_dir_all(dir.join("catalogs")).unwrap();
        fs::write(
            dir.join("catalogs/openai.json"),
            r#"["second","first","unselected"]"#,
        )
        .unwrap();
        let watched = TcpListener::bind("127.0.0.1:0").unwrap();
        watched.set_nonblocking(true).unwrap();
        let config = serde_json::json!({"providers": {
            "openai": {"baseUrl": OPENAI_ORIGIN, "api": "openai-completions", "models": [{"id":"second"},{"id":"first"},{"id":"orphan"}]},
            "lab": {"baseUrl":"https://192.0.2.1/v1", "api":"openai-completions", "models":[{"id":"local-only"}]},
            "watched": {"baseUrl":format!("http://{}/v1", watched.local_addr().unwrap()), "api":"openai-completions"},
        }});
        fs::write(dir.join("models.json"), config.to_string()).unwrap();
        let start = std::time::Instant::now();
        let registry = load_registry(&dir).unwrap();
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "local load must not wait on unreachable gateway"
        );
        assert_eq!(
            watched.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock,
            "load must not even open a gateway connection"
        );
        assert_eq!(
            registry.providers[0].models,
            vec![
                ModelChoice {
                    id: "second".to_string(),
                    status: CatalogMembership::Listed
                },
                ModelChoice {
                    id: "first".to_string(),
                    status: CatalogMembership::Listed
                },
                ModelChoice {
                    id: "orphan".to_string(),
                    status: CatalogMembership::Absent
                },
            ]
        );
        assert_eq!(registry.providers[0].catalog.as_ref().unwrap().source, None);
        assert_eq!(
            registry.providers[0].catalog_age(),
            "retrieval time unknown (old catalog format)"
        );
        assert_eq!(
            registry.providers[1].models,
            vec![ModelChoice {
                id: "local-only".to_string(),
                status: CatalogMembership::Unavailable
            }]
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_concurrent_or_stale_temp_file_cannot_clobber_the_previous_catalog() {
        let dir = crate::fixture_directory::create_unique_fixture_directory("atomic-catalog");
        fs::create_dir_all(dir.join("catalogs")).unwrap();
        fs::write(dir.join("catalogs/lab.json"), b"[\"old\"]\n").unwrap();
        fs::write(dir.join("catalogs/lab.json.tmp"), b"other writer").unwrap();
        let catalog = CatalogSnapshot {
            provider: "lab".to_string(),
            source: None,
            retrieved_at: None,
            models: vec!["new".to_string()],
        };
        assert!(write_last_good(&dir, &catalog).is_err());
        assert_eq!(
            fs::read(dir.join("catalogs/lab.json")).unwrap(),
            b"[\"old\"]\n"
        );
        assert_eq!(
            fs::read(dir.join("catalogs/lab.json.tmp")).unwrap(),
            b"other writer"
        );
        fs::remove_file(dir.join("catalogs/lab.json.tmp")).unwrap();
        write_last_good(&dir, &catalog).unwrap();
        assert_eq!(read_last_good(&dir, "lab").unwrap(), Some(catalog));
        assert!(!dir.join("catalogs/lab.json.tmp").exists());
        fs::remove_dir_all(dir).unwrap();
    }
}
