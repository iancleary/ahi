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
mod gateway;
mod metadata;
mod openai;
mod protocol;
pub use metadata::{
    CompletionCompatibility, InputLimits, InputModality, ModelCost, ModelMetadata, ThinkingMode,
    TokenParameter, TokenPrice,
};
pub(crate) use protocol::CatalogRoute;
pub use protocol::Protocol;

/// Resolved only when submitting a turn. Never log or debug-print credentials.
///
/// Three axes are kept apart on purpose: the provider's identity (the id the
/// operator selected), the wire protocol it speaks, and how the request is
/// authenticated. A protocol never implies an authentication scheme, and a
/// configured gateway can never become the destination for a browser credential.
pub(crate) struct CompletionConnection {
    pub(crate) protocol: Protocol,
    pub(crate) transport: CompletionTransport,
    pub(crate) metadata: ModelMetadata,
}

/// Destination and authentication for one connection.
pub(crate) enum CompletionTransport {
    /// A configured gateway: resolved headers, any protocol.
    Gateway {
        url: String,
        headers: BTreeMap<String, String>,
    },
    /// The built-in OpenAI browser-login credential. Destination is pinned
    /// inside `openai::inference_response`; protocol is always Responses.
    OpenAiResponses,
}

impl CompletionConnection {
    /// Browser-token Responses omits request fields the subscription endpoint
    /// rejects (Pi's `isChatGPTSignIn` boundary). Gateways get the full request.
    pub(crate) fn browser_token(&self) -> bool {
        matches!(self.transport, CompletionTransport::OpenAiResponses)
    }

    /// What a session grant is bound to: the exact destination, not the id.
    pub(crate) fn destination(&self) -> &str {
        match &self.transport {
            CompletionTransport::Gateway { url, .. } => url,
            CompletionTransport::OpenAiResponses => openai::INFERENCE_URL,
        }
    }
}

#[cfg(test)]
pub(crate) use openai::fixture_inference_response;
pub(crate) use openai::inference_response;

pub(crate) fn completion_connection(
    state: &Path,
    provider: &str,
    model: &str,
) -> Result<CompletionConnection> {
    let bytes = read_models_bytes(state)?;
    let file = parse_models(state, bytes.as_deref())?;
    let entry = file
        .entry(provider)
        .ok_or_else(|| Error::new("selected gateway is not registered in models.json"))?;
    if !entry.models.iter().any(|choice| choice.id == model) {
        return Err(Error::new(
            "selected model is no longer curated in models.json; reopen /model",
        ));
    }
    let catalog = read_last_good(state, provider).ok().flatten();
    let metadata = merge_metadata(catalog.as_ref(), &entry, model);
    if provider == OPENAI_ID {
        openai::catalog_source(&entry.base_url)?;
        if entry.api != Protocol::OpenAiCompletions
            || entry.api_key.is_some()
            || !entry.headers.is_empty()
        {
            return Err(Error::new("OpenAI browser login conflicts with configured api, apiKey, or headers; use a separate gateway id for proxies"));
        }
        return Ok(CompletionConnection {
            protocol: Protocol::OpenAiResponses,
            transport: CompletionTransport::OpenAiResponses,
            metadata,
        });
    }
    if entry.base_url.contains(['?', '#', '\\']) || entry.base_url.chars().any(char::is_control) {
        return Err(Error::new("inference baseUrl must not contain a query, fragment, backslash, or control characters"));
    }
    let headers = gateway_headers(&entry)?;
    Ok(CompletionConnection {
        protocol: entry.api,
        transport: CompletionTransport::Gateway {
            url: format!(
                "{}{}",
                entry.base_url.trim_end_matches('/'),
                entry.api.inference_path()
            ),
            headers,
        },
        metadata,
    })
}

/// Resolve a gateway's authentication and extra headers. Secrets are read from
/// the environment here and nowhere earlier.
fn gateway_headers(entry: &ProviderEntry) -> Result<BTreeMap<String, String>> {
    let mut headers = BTreeMap::new();
    for (name, value) in entry.api.fixed_headers() {
        headers.insert((*name).to_string(), (*value).to_string());
    }
    if let Some(key) = &entry.api_key {
        let (name, prefix) = entry.api.api_key_header();
        headers.insert(
            name.to_string(),
            format!("{prefix}{}", resolve_marker(key)?),
        );
    }
    for (name, value) in &entry.headers {
        let name = name.to_ascii_lowercase();
        if name.is_empty()
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
            || matches!(
                name.as_str(),
                "host"
                    | "content-length"
                    | "transfer-encoding"
                    | "connection"
                    | "content-type"
                    | "accept"
            )
        {
            return Err(Error::new("invalid or reserved inference header name"));
        }
        if headers.insert(name, resolve_marker(value)?).is_some() {
            return Err(Error::new("duplicate inference header"));
        }
    }
    Ok(headers)
}

fn resolve_marker(value: &str) -> Result<String> {
    let resolved = if let Some(name) = value.strip_prefix('$') {
        std::env::var(name).map_err(|_| {
            Error::new("inference credential environment variable is missing or not Unicode")
        })?
    } else {
        value.to_string()
    };
    if resolved.trim().is_empty() || resolved.chars().any(char::is_control) {
        return Err(Error::new(
            "inference credential is empty or contains control characters",
        ));
    }
    Ok(resolved)
}

pub use curation::{save_curated_models, save_provider_order};
pub(crate) use gateway::{
    discover_gateway, CatalogPersistence, DiscoveredGateway, GatewayRegistration,
};

pub const OPENAI_ID: &str = "openai";
pub const OPENAI_ORIGIN: &str = "https://api.openai.com/v1";
/// Built-in Anthropic provider, authenticated with an API key from the
/// environment. It appears when `ANTHROPIC_API_KEY` is set or an entry exists.
pub const ANTHROPIC_ID: &str = "anthropic";
pub const ANTHROPIC_ORIGIN: &str = "https://api.anthropic.com/v1";
pub const ANTHROPIC_KEY_ENV: &str = "ANTHROPIC_API_KEY";
// Gateway-specific discovery convention, not an OpenAI API standard.
const CATALOG_PATH: &str = "/agent/models.json";
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
    pub metadata: ModelMetadata,
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
    #[serde(default)]
    pub metadata: BTreeMap<String, ModelMetadata>,
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
    let openai = file.entry(OPENAI_ID).expect("openai is always implicit");
    providers.push(load_provider(
        state,
        OPENAI_ID,
        &openai,
        if state.join("credentials/openai.json").is_file() {
            "signed-in"
        } else {
            "signed-out"
        },
    ));
    if !file.providers.contains_key(ANTHROPIC_ID) {
        if let Some(entry) = file.entry(ANTHROPIC_ID) {
            providers.push(load_provider(state, ANTHROPIC_ID, &entry, "env"));
        }
    }
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
        providers.push(load_provider(state, id, entry, auth));
    }
    // Provider object order is the picker order. An unconfigured built-in
    // OpenAI stays first until an explicit reorder materializes its entry.
    if let Some(document) = models_bytes
        .as_deref()
        .and_then(|bytes| serde_json::from_slice::<Value>(bytes).ok())
    {
        if let Some(entries) = document.get("providers").and_then(Value::as_object) {
            let order = entries
                .keys()
                .enumerate()
                .map(|(index, id)| (id.as_str(), index + 1))
                .collect::<BTreeMap<_, _>>();
            providers.sort_by_key(|entry| order.get(entry.id.as_str()).copied().unwrap_or(0));
        }
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
            lines.push(format!(
                "  model {} status {} {}",
                model.id,
                model.status,
                model.metadata.summary()
            ));
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
            _schema: None,
            providers: BTreeMap::new(),
        }),
        Some(bytes) => serde_json::from_slice(bytes)
            .map_err(|err| Error::new(format!("{}: {err}", state.join("models.json").display()))),
    }
}

/// Load one provider from its entry and its last-good catalog. The catalog is
/// read once here; every curated model's metadata is merged from that one copy.
fn load_provider(state: &Path, id: &str, entry: &ProviderEntry, auth: &str) -> LoadedProvider {
    let (catalog, catalog_error) = match read_last_good(state, id) {
        Ok(catalog) => (catalog, None),
        Err(err) => (None, Some(err.to_string())),
    };
    let models = entry
        .models
        .iter()
        .map(|model| ModelChoice {
            id: model.id.clone(),
            metadata: merge_metadata(catalog.as_ref(), entry, &model.id),
            status: match &catalog {
                None => CatalogMembership::Unavailable,
                Some(catalog) if catalog.models.contains(&model.id) => CatalogMembership::Listed,
                Some(_) => CatalogMembership::Absent,
            },
        })
        .collect();
    LoadedProvider {
        id: id.to_string(),
        base_url: entry.base_url.clone(),
        models,
        auth: auth.to_string(),
        catalog,
        catalog_error,
        empty_reason: entry.models.is_empty().then(|| {
            "no models in models.json: press e to edit, c to browse the catalog".to_string()
        }),
    }
}

/// Explicitly refresh one reference catalog. Never edits models.json.
pub fn refresh_provider(state: &Path, id: &str) -> Result<()> {
    check_id(id)?;
    let file = read_models(state)?;
    let entry = file
        .entry(id)
        .ok_or_else(|| Error::new(format!("unknown provider `{id}`")))?;
    let (source, models, metadata) = if id == OPENAI_ID {
        (
            openai::catalog_source(&entry.base_url)?,
            openai::refresh(state, &entry.base_url)?,
            BTreeMap::new(),
        )
    } else {
        match entry.api.catalog_route() {
            CatalogRoute::GatewayConvention => {
                let catalog = fetch_gateway_catalog(&entry.base_url, Some(id))?;
                if catalog.provider_id != id {
                    return Err(Error::new(
                        "catalog provider id does not match registered gateway",
                    ));
                }
                (
                    catalog_url(&entry.base_url)?,
                    catalog.models,
                    catalog.metadata,
                )
            }
            CatalogRoute::StandardModels => {
                let headers = gateway_headers(&entry)?;
                let (models, mut metadata) = fetch_standard_models(&entry.base_url, &headers)?;
                // `/models` lists ids and names only. Typed metadata that
                // discovery or an earlier refresh recorded for an id that is
                // still listed survives; the fresh name overlays it.
                if let Ok(Some(previous)) = read_last_good(state, id) {
                    for (model, fresh) in metadata.iter_mut() {
                        if let Some(kept) = previous.metadata.get(model) {
                            *fresh = kept.overlay(fresh);
                        }
                    }
                }
                (standard_models_url(&entry.base_url), models, metadata)
            }
        }
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
            metadata,
        },
    )
}

/// Refresh every provider, continuing after failures. Errors are for this refresh,
/// not persisted in the catalog (a failed refresh leaves that file untouched).
pub fn refresh_all(state: &Path) -> Result<BTreeMap<String, String>> {
    let registry = load_registry(state)?;
    // Providers refresh concurrently: each writes only its own catalog file,
    // so N providers take one round trip, not N in sequence.
    let outcomes = std::thread::scope(|scope| {
        let handles: Vec<_> = registry
            .providers
            .iter()
            .map(|provider| {
                let id = provider.id.as_str();
                (id, scope.spawn(move || refresh_provider(state, id)))
            })
            .collect();
        handles
            .into_iter()
            .map(|(id, handle)| {
                let outcome = handle
                    .join()
                    .unwrap_or_else(|_| Err(Error::new("catalog refresh thread panicked")));
                (id, outcome)
            })
            .collect::<Vec<_>>()
    });
    let mut errors = BTreeMap::new();
    for (id, outcome) in outcomes {
        if let Err(err) = outcome {
            errors.insert(id.to_string(), err.to_string());
        }
    }
    Ok(errors)
}

fn catalog_url(base_url: &str) -> Result<String> {
    let origin = origin_of(base_url)?;
    Ok(format!("{origin}{CATALOG_PATH}"))
}

fn standard_models_url(base_url: &str) -> String {
    format!("{}/models", base_url.trim_end_matches('/'))
}

/// `GET <baseUrl>/models` as OpenAI and Anthropic publish it: a `data` array of
/// objects with an `id`, and for Anthropic a `display_name`. Anthropic pages
/// with `has_more`/`last_id`; a bounded number of pages is followed.
fn fetch_standard_models(
    base_url: &str,
    headers: &BTreeMap<String, String>,
) -> Result<(Vec<String>, BTreeMap<String, ModelMetadata>)> {
    #[derive(Deserialize)]
    struct Page {
        data: Vec<Entry>,
        #[serde(default)]
        has_more: bool,
        #[serde(default)]
        last_id: Option<String>,
    }
    #[derive(Deserialize)]
    struct Entry {
        id: String,
        #[serde(default)]
        display_name: Option<String>,
    }
    const MAX_PAGES: usize = 8;
    let mut models = Vec::new();
    let mut metadata = BTreeMap::new();
    let mut after: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let mut url = format!("{}?limit=1000", standard_models_url(base_url));
        if let Some(after) = &after {
            if !valid_model_id(after) || after.contains(['&', '?', '#', '/', '%']) {
                return Err(Error::protocol("invalid model catalog page cursor"));
            }
            url.push_str("&after_id=");
            url.push_str(after);
        }
        let bytes = http_get_with_headers(&url, headers)?;
        // Never propagate serde errors: a body can contain anything.
        let page: Page = serde_json::from_slice(&bytes)
            .map_err(|_| Error::protocol("invalid model catalog: expected a data array"))?;
        for entry in page.data {
            if !valid_model_id(&entry.id) {
                return Err(Error::protocol("invalid model catalog id"));
            }
            if metadata.contains_key(&entry.id) {
                return Err(Error::protocol("duplicate model catalog id"));
            }
            let model = ModelMetadata {
                name: entry
                    .display_name
                    .filter(|name| !name.trim().is_empty() && !name.chars().any(char::is_control)),
                ..ModelMetadata::default()
            };
            metadata.insert(entry.id.clone(), model);
            models.push(entry.id);
        }
        if !page.has_more {
            return Ok((models, metadata));
        }
        after = match page.last_id.or_else(|| models.last().cloned()) {
            Some(cursor) if Some(&cursor) != after.as_ref() => Some(cursor),
            _ => return Err(Error::protocol("model catalog pagination did not advance")),
        };
    }
    Err(Error::limit(
        "model catalog has more pages than ahi follows; curate models by id instead",
    ))
}

struct GatewayCatalog {
    provider_id: String,
    protocol: Protocol,
    api_key: String,
    base_url: String,
    models: Vec<String>,
    metadata: BTreeMap<String, ModelMetadata>,
}

fn fetch_gateway_catalog(base_url: &str, registered: Option<&str>) -> Result<GatewayCatalog> {
    let bytes = http_get(&catalog_url(base_url)?)?;
    let mut document: Value =
        serde_json::from_slice(&bytes).map_err(|_| Error::new("invalid gateway catalog JSON"))?;
    if let Some(providers) = document.get("providers") {
        let providers = providers
            .as_object()
            .ok_or_else(|| Error::new("catalog providers must be an object"))?;
        let mut candidates = providers
            .iter()
            .filter(|(id, entry)| match registered {
                Some(registered) => id.as_str() == registered,
                None => entry
                    .get("api")
                    .and_then(Value::as_str)
                    .is_some_and(|api| Protocol::parse(api).is_some()),
            })
            .collect::<Vec<_>>();
        if registered.is_none() && candidates.len() > 1 {
            candidates.retain(|(_, entry)| {
                entry
                    .get("baseUrl")
                    .and_then(Value::as_str)
                    .is_some_and(|url| url.trim_end_matches('/') == base_url.trim_end_matches('/'))
            });
        }
        if candidates.len() != 1 {
            return Err(Error::new("catalog provider selection is ambiguous or missing; enter the exact inference base URL, or refresh a registered provider identifier"));
        }
        let (id, entry) = candidates[0];
        let mut entry = entry
            .as_object()
            .cloned()
            .ok_or_else(|| Error::new("catalog provider must be an object"))?;
        entry.insert("id".into(), Value::String(id.clone()));
        document = Value::Object(entry);
    }
    if let Some(models) = document.get("models").and_then(Value::as_array) {
        for model in models {
            metadata::parse_metadata(model).map_err(|error| {
                Error::new(format!(
                    "invalid gateway catalog schema or model metadata: {error}"
                ))
            })?;
        }
    }
    let catalog: Catalog = serde_json::from_value(document).map_err(|_| Error::new("invalid gateway catalog schema or provider compat (expected openai-completions, apiKey, model objects and supported compatibility fields)"))?;
    let inference_url = catalog.base_url.as_deref().unwrap_or(base_url);
    validate_inference_url(inference_url)?;
    let input_url = ureq::get(base_url)
        .request_url()
        .map_err(|_| Error::new("invalid gateway URL"))?;
    let returned_url = ureq::get(inference_url)
        .request_url()
        .map_err(|_| Error::new("invalid catalog baseUrl"))?;
    if input_url.as_url().origin() != returned_url.as_url().origin() {
        return Err(Error::new(
            "catalog baseUrl must have the same origin as the entered gateway URL",
        ));
    }
    check_marker("catalog apiKey", &catalog.api_key)
        .map_err(|_| Error::new("catalog apiKey must be gateway or a valid $NAME reference"))?;
    let provider_id = match (&catalog.id, &catalog.provider) {
        (Some(id), Some(provider)) if id == provider => id.clone(),
        (Some(id), None) => id.clone(),
        (None, Some(provider)) => provider.clone(),
        (Some(_), Some(_)) => {
            return Err(Error::new("catalog id does not match provider"));
        }
        (None, None) => {
            return Err(Error::new("catalog is missing a provider id"));
        }
    };
    check_id(&provider_id).map_err(|_| Error::new("invalid catalog provider identifier"))?;
    let mut models = Vec::new();
    let mut metadata = BTreeMap::new();
    for model in catalog.models {
        if !valid_model_id(&model.id) {
            return Err(Error::new("invalid catalog model id"));
        }
        if metadata.contains_key(&model.id) {
            return Err(Error::new("duplicate catalog model id"));
        }
        let mut effective = model.metadata;
        effective.compat = catalog.compat.overlay(&effective.compat);
        metadata.insert(model.id.clone(), effective);
        models.push(model.id);
    }
    Ok(GatewayCatalog {
        provider_id,
        protocol: catalog.api,
        api_key: catalog.api_key,
        base_url: inference_url.trim_end_matches('/').to_string(),
        models,
        metadata,
    })
}

fn http_get(url: &str) -> Result<Vec<u8>> {
    http_get_with_headers(url, &BTreeMap::new())
}

/// Bounded JSON GET. With headers, the response is authenticated and its
/// error text can carry upstream content, so transport errors are generic.
fn http_get_with_headers(url: &str, headers: &BTreeMap<String, String>) -> Result<Vec<u8>> {
    let agent = ureq::AgentBuilder::new()
        .redirects(0)
        .timeout_connect(FETCH_TIMEOUT)
        .timeout_read(FETCH_TIMEOUT)
        .timeout_write(FETCH_TIMEOUT)
        .build();
    let mut request = agent.get(url).set("Accept", "application/json");
    for (name, value) in headers {
        request = request.set(name, value);
    }
    let authenticated = !headers.is_empty();
    let response = match request.call() {
        Ok(response) => response,
        Err(ureq::Error::Status(code, _)) if (300..400).contains(&code) => {
            return Err(Error::transport(format!(
                "redirects are not followed ({code} from {url})"
            )));
        }
        Err(ureq::Error::Status(401 | 403, _)) if authenticated => {
            return Err(Error::auth(format!(
                "catalog fetch {url}: authentication rejected; check the configured key"
            )));
        }
        Err(ureq::Error::Status(code, _)) if authenticated => {
            return Err(Error::transport(format!(
                "catalog fetch {url}: status {code}"
            )));
        }
        Err(_) if authenticated => {
            return Err(Error::transport(format!(
                "catalog fetch {url}: network, TLS, or timeout error"
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
    let catalog = match serde_json::from_slice(&bytes).map_err(|_| {
        Error::new(format!(
            "{}: invalid catalog cache schema or metadata",
            path.display()
        ))
    })? {
        Stored::Current(catalog) => catalog,
        Stored::Legacy(models) => CatalogSnapshot {
            provider: id.to_string(),
            source: None,
            retrieved_at: None,
            models,
            metadata: BTreeMap::new(),
        },
    };
    if catalog.provider != id
        || catalog.models.iter().any(|id| !valid_model_id(id))
        || catalog
            .metadata
            .keys()
            .any(|id| !catalog.models.contains(id))
    {
        return Err(Error::new(format!(
            "{}: invalid provider or model id",
            path.display()
        )));
    }
    for metadata in catalog.metadata.values() {
        metadata::parse_metadata(
            &serde_json::to_value(metadata).map_err(|_| Error::new("invalid catalog metadata"))?,
        )?;
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
    // The reserved `openai` id keeps its legacy spelling; it selects browser
    // login and the Responses adapter regardless (see completion_connection).
    if id == OPENAI_ID && entry.api != Protocol::OpenAiCompletions {
        return Err(Error::new(format!(
            "gateway `{id}` api must be openai-completions (browser login selects the Responses transport)"
        )));
    }
    validate_inference_url(&entry.base_url)?;
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
            "{label} environment reference must be `$NAME`"
        )));
    }
    Ok(())
}

/// Effective metadata for one curated model: the cached catalog entry, then the
/// provider's compat defaults, then the model's own local fields on top. A pure
/// function over data the caller already holds; it reads no file.
fn merge_metadata(
    catalog: Option<&CatalogSnapshot>,
    entry: &ProviderEntry,
    model: &str,
) -> ModelMetadata {
    let cached = catalog
        .and_then(|catalog| catalog.metadata.get(model).cloned())
        .unwrap_or_default();
    let mut local = entry
        .models
        .iter()
        .find(|choice| choice.id == model)
        .map(|choice| choice.metadata.clone())
        .unwrap_or_default();
    local.compat = entry.compat.overlay(&local.compat);
    cached.overlay(&local)
}

fn validate_inference_url(url: &str) -> Result<()> {
    if url.len() > 8192
        || url.contains(['?', '#', '\\', '@'])
        || url
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
    {
        return Err(Error::new(
            "baseUrl must be an HTTP(S) URL without credentials, whitespace, query or fragment",
        ));
    }
    ureq::get(url)
        .request_url()
        .map_err(|_| Error::new("invalid inference baseUrl"))?;
    origin_of(url)
        .map(|_| ())
        .map_err(|_| Error::new("baseUrl requires HTTPS or loopback HTTP"))
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

pub(crate) use openai::OpenAiLogin;

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
    #[serde(default, rename = "$schema")]
    _schema: Option<String>,
}

impl ModelsFile {
    /// The configured entry, or the implicit built-in one for a reserved id.
    /// `openai` is always implicit; `anthropic` only when its key is set.
    fn entry(&self, id: &str) -> Option<std::borrow::Cow<'_, ProviderEntry>> {
        use std::borrow::Cow;
        if let Some(entry) = self.providers.get(id) {
            return Some(Cow::Borrowed(entry));
        }
        implicit_entry(id).map(Cow::Owned)
    }
}

/// The built-in entry a reserved id has before it is materialized in models.json.
fn implicit_entry(id: &str) -> Option<ProviderEntry> {
    match id {
        OPENAI_ID => Some(ProviderEntry {
            base_url: OPENAI_ORIGIN.to_string(),
            api: Protocol::OpenAiCompletions,
            api_key: None,
            models: Vec::new(),
            headers: BTreeMap::new(),
            compat: CompletionCompatibility::default(),
        }),
        ANTHROPIC_ID if anthropic_key_present() => Some(ProviderEntry {
            base_url: ANTHROPIC_ORIGIN.to_string(),
            api: Protocol::AnthropicMessages,
            api_key: Some(format!("${ANTHROPIC_KEY_ENV}")),
            models: Vec::new(),
            headers: BTreeMap::new(),
            compat: CompletionCompatibility::default(),
        }),
        _ => None,
    }
}

/// Whether `ANTHROPIC_API_KEY` is set. Tests decide this per thread, so a
/// developer's exported key cannot change what a fixture loads.
#[cfg(not(test))]
fn anthropic_key_present() -> bool {
    std::env::var_os(ANTHROPIC_KEY_ENV).is_some_and(|value| !value.is_empty())
}

#[cfg(test)]
thread_local! {
    static TEST_ANTHROPIC_KEY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn anthropic_key_present() -> bool {
    TEST_ANTHROPIC_KEY.with(std::cell::Cell::get)
}

/// Pretend `ANTHROPIC_API_KEY` is set for the calling test thread.
#[cfg(test)]
pub(crate) fn test_set_anthropic_key(present: bool) {
    TEST_ANTHROPIC_KEY.with(|cell| cell.set(present));
}

/// The JSON written when a reserved id is first materialized by a save.
pub(super) fn implicit_entry_json(id: &str) -> Option<Value> {
    let entry = implicit_entry(id)?;
    let mut value = serde_json::json!({
        "baseUrl": entry.base_url,
        "api": entry.api.as_str(),
    });
    if let Some(key) = entry.api_key {
        value["apiKey"] = Value::String(key);
    }
    Some(value)
}

fn deserialize_protocol<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Protocol, D::Error> {
    let text = String::deserialize(deserializer)?;
    Protocol::parse(&text).ok_or_else(|| {
        serde::de::Error::custom(format!("api must be one of {}", Protocol::supported_list()))
    })
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

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderEntry {
    #[serde(rename = "baseUrl")]
    base_url: String,
    #[serde(deserialize_with = "deserialize_protocol")]
    api: Protocol,
    #[serde(default, rename = "apiKey")]
    api_key: Option<String>,
    #[serde(default)]
    models: Vec<ModelId>,
    #[serde(default)]
    headers: BTreeMap<String, String>,
    #[serde(default, deserialize_with = "deserialize_compatibility")]
    compat: CompletionCompatibility,
}

fn deserialize_compatibility<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<CompletionCompatibility, D::Error> {
    let value = Value::deserialize(deserializer)?;
    serde_json::from_value(value).map_err(|_| serde::de::Error::custom("invalid provider compat: expected supported boolean flags and maxTokensField max_tokens or max_completion_tokens"))
}

#[derive(Debug, Clone)]
struct ModelId {
    id: String,
    metadata: ModelMetadata,
}

impl<'de> Deserialize<'de> for ModelId {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        let id = value
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| serde::de::Error::custom("model id must be text"))?
            .to_string();
        let metadata = metadata::parse_metadata(&value).map_err(serde::de::Error::custom)?;
        Ok(Self { id, metadata })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Catalog {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    provider: Option<String>,
    #[serde(deserialize_with = "deserialize_protocol")]
    api: Protocol,
    #[serde(rename = "apiKey")]
    api_key: String,
    models: Vec<ModelId>,
    #[serde(default, rename = "baseUrl")]
    base_url: Option<String>,
    #[serde(default)]
    compat: CompletionCompatibility,
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

    /// Look a provider up by id: the implicit `anthropic` entry appears when
    /// the developer's shell exports ANTHROPIC_API_KEY, shifting indexes.
    fn provider<'a>(registry: &'a Registry, id: &str) -> &'a LoadedProvider {
        registry
            .providers
            .iter()
            .find(|provider| provider.id == id)
            .unwrap_or_else(|| panic!("provider {id}"))
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
    fn protocols_select_the_inference_path_and_credential_header() {
        let dir = crate::fixture_directory::create_unique_fixture_directory("harness-protocols");
        fs::write(
            dir.join("models.json"),
            r#"{"providers":{
                "claude":{"baseUrl":"https://anthropic.example/v1","api":"anthropic-messages","apiKey":"gateway","models":[{"id":"claude-x"}]},
                "proxy":{"baseUrl":"https://proxy.example/v1/","api":"openai-responses","apiKey":"gateway","headers":{"X-Team":"gateway"},"models":[{"id":"gpt-x"}]},
                "home":{"baseUrl":"http://127.0.0.1:11434/v1","api":"openai-completions","models":[{"id":"qwen"}]}
            }}"#,
        )
        .unwrap();
        let claude = completion_connection(&dir, "claude", "claude-x").unwrap();
        assert_eq!(claude.protocol, Protocol::AnthropicMessages);
        assert!(!claude.browser_token());
        let CompletionTransport::Gateway { url, headers } = &claude.transport else {
            panic!("gateway transport");
        };
        assert_eq!(url, "https://anthropic.example/v1/messages");
        assert_eq!(claude.destination(), url);
        assert_eq!(headers["x-api-key"], "gateway");
        assert_eq!(headers["anthropic-version"], "2023-06-01");
        assert!(!headers.contains_key("authorization"));

        let proxy = completion_connection(&dir, "proxy", "gpt-x").unwrap();
        assert_eq!(proxy.protocol, Protocol::OpenAiResponses);
        let CompletionTransport::Gateway { url, headers } = &proxy.transport else {
            panic!("gateway transport");
        };
        assert_eq!(url, "https://proxy.example/v1/responses");
        assert_eq!(headers["authorization"], "Bearer gateway");
        assert_eq!(headers["x-team"], "gateway");

        let home = completion_connection(&dir, "home", "qwen").unwrap();
        assert_eq!(home.protocol, Protocol::OpenAiCompletions);
        assert_eq!(
            home.destination(),
            "http://127.0.0.1:11434/v1/chat/completions"
        );

        // The reserved id keeps its legacy spelling and the browser transport.
        fs::write(
            dir.join("models.json"),
            r#"{"providers":{"openai":{"baseUrl":"https://api.openai.com/v1","api":"openai-responses","models":[{"id":"m"}]}}}"#,
        )
        .unwrap();
        let error = load_registry(&dir).unwrap_err().to_string();
        assert!(error.contains("api must be openai-completions"), "{error}");
        fs::write(
            dir.join("models.json"),
            r#"{"providers":{"lab":{"baseUrl":"https://lab.example/v1","api":"grpc","models":[]}}}"#,
        )
        .unwrap();
        let error = load_registry(&dir).unwrap_err().to_string();
        assert!(
            error.contains(
                "api must be one of openai-completions, openai-responses, anthropic-messages"
            ),
            "{error}"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_implicit_anthropic_provider_follows_the_key_and_materializes_in_front() {
        let dir = crate::fixture_directory::create_unique_fixture_directory("harness-anthropic");
        fs::write(
            dir.join("models.json"),
            r#"{"providers":{"lab":{"baseUrl":"https://lab.example/v1","api":"openai-completions","models":[{"id":"m"}]}}}"#,
        )
        .unwrap();
        let ids = |registry: &Registry| {
            registry
                .providers
                .iter()
                .map(|provider| provider.id.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&load_registry(&dir).unwrap()), ["openai", "lab"]);
        test_set_anthropic_key(true);
        let registry = load_registry(&dir).unwrap();
        assert_eq!(ids(&registry), ["openai", "anthropic", "lab"]);
        let anthropic = provider(&registry, ANTHROPIC_ID);
        assert_eq!(anthropic.auth, "env");
        assert_eq!(anthropic.base_url, ANTHROPIC_ORIGIN);
        assert!(anthropic.models.is_empty());
        // Curating a model materializes the entry at the front, with the
        // environment reference and protocol, leaving saved order alone.
        let saved =
            save_curated_models(&dir, &registry, ANTHROPIC_ID, &["claude-x".to_string()]).unwrap();
        assert_eq!(ids(&saved), ["openai", "anthropic", "lab"]);
        let document: Value =
            serde_json::from_slice(&fs::read(dir.join("models.json")).unwrap()).unwrap();
        let keys: Vec<&String> = document["providers"].as_object().unwrap().keys().collect();
        assert_eq!(keys, ["anthropic", "lab"]);
        assert_eq!(
            document["providers"]["anthropic"]["apiKey"],
            format!("${ANTHROPIC_KEY_ENV}")
        );
        assert_eq!(
            document["providers"]["anthropic"]["api"],
            "anthropic-messages"
        );
        // The key is resolved from the real environment only at submission.
        match completion_connection(&dir, ANTHROPIC_ID, "claude-x") {
            Ok(connection) => {
                assert!(std::env::var_os(ANTHROPIC_KEY_ENV).is_some());
                assert_eq!(connection.protocol, Protocol::AnthropicMessages);
                assert_eq!(
                    connection.destination(),
                    format!("{ANTHROPIC_ORIGIN}/messages")
                );
            }
            Err(error) => {
                assert!(std::env::var_os(ANTHROPIC_KEY_ENV).is_none());
                assert!(error
                    .to_string()
                    .contains("environment variable is missing"));
            }
        }
        test_set_anthropic_key(false);
        assert_eq!(
            ids(&load_registry(&dir).unwrap()),
            ["openai", "anthropic", "lab"]
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn standard_models_refresh_keeps_display_names_and_sends_the_configured_key() {
        let dir = crate::fixture_directory::create_unique_fixture_directory("harness-standard");
        let body = br#"{"data":[{"id":"claude-b","display_name":"Claude B","type":"model"},{"id":"claude-a","display_name":" "}],"has_more":false}"#;
        let port = serve("200 OK", "", body);
        fs::write(
            dir.join("models.json"),
            format!(
                r#"{{"providers":{{"claude":{{"baseUrl":"http://127.0.0.1:{port}/v1","api":"anthropic-messages","apiKey":"gateway","models":[{{"id":"claude-a"}}]}}}}}}"#
            ),
        )
        .unwrap();
        refresh_provider(&dir, "claude").unwrap();
        let registry = load_registry(&dir).unwrap();
        let claude = registry
            .providers
            .iter()
            .find(|provider| provider.id == "claude")
            .unwrap();
        let catalog = claude.catalog.as_ref().unwrap();
        assert_eq!(catalog.models, ["claude-b", "claude-a"]);
        assert_eq!(
            catalog.source.as_deref(),
            Some(format!("http://127.0.0.1:{port}/v1/models").as_str())
        );
        assert_eq!(
            catalog.metadata["claude-b"].name.as_deref(),
            Some("Claude B")
        );
        assert_eq!(catalog.metadata["claude-a"].name, None);
        assert_eq!(claude.models[0].status, CatalogMembership::Listed);
        fs::remove_dir_all(dir).unwrap();
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
        let lab = provider(&registry, "lab");
        assert_eq!(
            lab.models,
            vec![ModelChoice {
                id: "kept".to_string(),
                metadata: ModelMetadata::default(),
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
        let lab = provider(&registry, "lab");
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
        let lab = provider(&registry, "lab");
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
        let lab = provider(&registry, "lab");
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
                    metadata: ModelMetadata::default(),
                    status: CatalogMembership::Listed
                },
                ModelChoice {
                    id: "first".to_string(),
                    metadata: ModelMetadata::default(),
                    status: CatalogMembership::Listed
                },
                ModelChoice {
                    id: "orphan".to_string(),
                    metadata: ModelMetadata::default(),
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
            provider(&registry, "lab").models,
            vec![ModelChoice {
                id: "local-only".to_string(),
                metadata: ModelMetadata::default(),
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
            metadata: BTreeMap::new(),
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
