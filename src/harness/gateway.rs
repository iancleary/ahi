use super::*;

pub(crate) struct DiscoveredGateway {
    pub(crate) provider_id: String,
    pub(crate) base_url: String,
    catalog: GatewayCatalog,
}

pub(crate) enum CatalogPersistence {
    Saved,
    Failed(String),
}

pub(crate) struct GatewayRegistration {
    pub(crate) provider_id: String,
    pub(crate) notice: String,
    pub(crate) catalog: CatalogPersistence,
}

pub(crate) fn discover_gateway(text: &str) -> Result<DiscoveredGateway> {
    let text = text.trim();
    if text.len() > 8192
        || text.contains('\\')
        || text
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
    {
        return Err(Error::new("enter a URL without whitespace, backslashes, or control characters (maximum 8192 bytes)"));
    }
    let url = super::parse_http_url(text)
        .map_err(|_| Error::new("enter a valid HTTP or HTTPS gateway base URL"))?;
    // `http::Uri` keeps userinfo inside the authority; `@` anywhere in the
    // text is refused outright, so no credential can ride along.
    if text.contains('@') {
        return Err(Error::new(
            "gateway URL must not contain userinfo or credentials",
        ));
    }
    if url.query().is_some() || text.contains('#') {
        return Err(Error::new(
            "gateway URL must not contain a query or fragment",
        ));
    }
    let base_url = text.trim_end_matches('/').to_string();
    origin_of(&base_url)?;
    let catalog = fetch_gateway_catalog(&base_url, None)?;
    let base_url = catalog.base_url.clone();
    if catalog.provider_id == OPENAI_ID {
        return Err(Error::new(
            "openai is reserved for browser login; use a separate gateway identifier",
        ));
    }
    Ok(DiscoveredGateway {
        provider_id: catalog.provider_id.clone(),
        base_url,
        catalog,
    })
}

impl DiscoveredGateway {
    pub(crate) fn models(&self) -> &[String] {
        &self.catalog.models
    }

    pub(crate) fn review_summary(&self, registry: &Registry) -> String {
        let document = registry
            .models_bytes
            .as_deref()
            .and_then(|bytes| serde_json::from_slice::<Value>(bytes).ok());
        let existing = document
            .as_ref()
            .and_then(|document| document.get("providers"))
            .and_then(|providers| providers.get(&self.provider_id));
        match existing {
            Some(existing) => {
                let base_url = existing
                    .get("baseUrl")
                    .and_then(Value::as_str)
                    .unwrap_or(&self.base_url);
                let authentication = existing
                    .get("apiKey")
                    .and_then(Value::as_str)
                    .unwrap_or("none");
                let headers = existing
                    .get("headers")
                    .and_then(Value::as_object)
                    .map_or(0, |headers| headers.len());
                let operation = if base_url.trim_end_matches('/') == self.base_url {
                    "Rediscover existing gateway; settings retained"
                } else {
                    "Conflict: identifier already registered at a different URL"
                };
                format!("{operation}\nGateway: {}\nInference base URL: {base_url}\nAuthentication: {authentication}; {headers} configured header(s) retained", self.provider_id)
            }
            None => format!(
                "Register new gateway\nGateway: {}\nInference base URL: {}\nProtocol: {}\nAuthentication: {}",
                self.provider_id,
                self.base_url,
                self.catalog.protocol.as_str(),
                self.catalog.api_key
            ),
        }
    }

    pub(crate) fn register(
        &self,
        state: &crate::directories::Directories,
        registry: &Registry,
    ) -> Result<GatewayRegistration> {
        curation::check_snapshot(state, registry)?;
        let snapshot = CatalogSnapshot {
            provider: self.provider_id.clone(),
            source: Some(catalog_url(&self.base_url)?),
            retrieved_at: Some(
                time::OffsetDateTime::now_utc()
                    .format(&time::format_description::well_known::Rfc3339)
                    .map_err(|error| Error::new(format!("catalog timestamp: {error}")))?,
            ),
            models: self.catalog.models.clone(),
            metadata: self.catalog.metadata.clone(),
        };
        let mut document: Value = match &registry.models_bytes {
            Some(bytes) => {
                serde_json::from_slice(bytes).map_err(|_| Error::new("invalid models.json"))?
            }
            None => serde_json::json!({"providers": {}}),
        };
        let providers = document
            .as_object_mut()
            .ok_or_else(|| Error::new("models.json root must be an object to register a gateway"))?
            .entry("providers")
            .or_insert_with(|| serde_json::json!({}))
            .as_object_mut()
            .ok_or_else(|| {
                Error::new("models.json providers must be an object to register a gateway")
            })?;
        if let Some(existing) = providers.get(&self.provider_id) {
            if existing
                .get("baseUrl")
                .and_then(Value::as_str)
                .map(|url| url.trim_end_matches('/'))
                != Some(self.base_url.as_str())
            {
                return Err(Error::new(format!(
                    "gateway {} is already registered at a different URL; existing settings unchanged",
                    self.provider_id
                )));
            }
        } else {
            providers.insert(
                self.provider_id.clone(),
                serde_json::json!({
                    "baseUrl": self.base_url,
                    "api": self.catalog.protocol.as_str(),
                    "apiKey": self.catalog.api_key,
                    "models": [],
                }),
            );
            curation::save_document(state, registry, document)?;
        }
        let (catalog, notice) = match write_last_good(state, &snapshot) {
            Ok(()) => (
                CatalogPersistence::Saved,
                format!(
                    "Gateway {} registered; choose models with Space",
                    self.provider_id
                ),
            ),
            Err(error) => (
                CatalogPersistence::Failed(error.to_string()),
                format!(
                    "Gateway {} registered, but catalog save failed: {error}",
                    self.provider_id
                ),
            ),
        };
        Ok(GatewayRegistration {
            provider_id: self.provider_id.clone(),
            notice,
            catalog,
        })
    }
}
