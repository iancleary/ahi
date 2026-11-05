//! Explicit models.json edits with optimistic conflict detection and atomic replacement.
use super::*;
use std::collections::BTreeSet;

/// Save only this provider's curated IDs/order. The caller replaces its registry
/// only on success; browsing, refresh, and editor reload never call this writer.
/// Explicit saves pretty-print JSON while retaining object key order.
pub fn save_curated_models(
    state: &Path,
    registry: &Registry,
    provider: &str,
    ids: &[String],
) -> Result<Registry> {
    check_id(provider)?;
    if !registry.providers.iter().any(|entry| entry.id == provider) {
        return Err(Error::new(format!("unknown provider `{provider}`")));
    }
    check_snapshot(state, registry)?;
    let mut document: Value = match &registry.models_bytes {
        Some(bytes) => serde_json::from_slice(bytes).map_err(|err| Error::new(err.to_string()))?,
        None => serde_json::json!({"providers": {}}),
    };
    let path = state.join("models.json");
    let providers = document
        .as_object_mut()
        .ok_or_else(|| {
            Error::new(format!(
                "{}: cannot save curated models: document root must be a JSON object; edit models.json and press e to reload before saving",
                path.display()
            ))
        })?
        .entry("providers")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| {
            Error::new(format!(
                "{}: cannot save curated models: `providers` must be a JSON object; edit models.json and press e to reload before saving",
                path.display()
            ))
        })?;
    if !providers.contains_key(provider) {
        let Some(implicit) = implicit_entry_json(provider) else {
            return Err(Error::new(format!(
                "unknown provider `{provider}` in models.json"
            )));
        };
        // An implicit built-in sorts first in the picker (no saved position);
        // materialize it at the front and leave every saved position alone.
        let mut ordered = serde_json::Map::new();
        ordered.insert(provider.to_string(), implicit);
        ordered.extend(std::mem::take(providers));
        *providers = ordered;
    }
    let entry = providers
        .get_mut(provider)
        .expect("provider was inserted above");
    let retained = entry
        .get("models")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let models = ids
        .iter()
        .map(|id| {
            retained
                .iter()
                .find(|model| model.get("id").and_then(Value::as_str) == Some(id.as_str()))
                .cloned()
                .unwrap_or_else(|| serde_json::json!({"id": id}))
        })
        .collect();
    entry
        .as_object_mut()
        .ok_or_else(|| {
            Error::new(format!(
                "{}: cannot save curated models: `providers.{provider}` must be a JSON object; edit models.json and press e to reload before saving",
                path.display()
            ))
        })?
        .insert(
            "models".to_string(),
            Value::Array(models),
        );
    save_document(state, registry, document)
}

pub(super) fn check_snapshot(state: &Path, registry: &Registry) -> Result<()> {
    if read_models_bytes(state)? != registry.models_bytes {
        return Err(Error::new(
            "models.json changed outside this view; press e to review/reload it before saving",
        ));
    }
    Ok(())
}

/// Save provider object order without changing credentials, model lists, or settings.
pub fn save_provider_order(
    state: &Path,
    registry: &Registry,
    order: &[String],
) -> Result<Registry> {
    check_snapshot(state, registry)?;
    let expected = registry
        .providers
        .iter()
        .map(|entry| entry.id.as_str())
        .collect::<BTreeSet<_>>();
    let requested = order.iter().map(String::as_str).collect::<BTreeSet<_>>();
    if expected != requested || order.len() != expected.len() {
        return Err(Error::new(
            "gateway order must contain each provider exactly once",
        ));
    }
    let mut document: Value = match &registry.models_bytes {
        Some(bytes) => {
            serde_json::from_slice(bytes).map_err(|_| Error::new("invalid models.json"))?
        }
        None => serde_json::json!({"providers": {}}),
    };
    let root = document
        .as_object_mut()
        .ok_or_else(|| Error::new("models.json root must be an object to reorder gateways"))?;
    let providers = root
        .entry("providers")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| Error::new("models.json providers must be an object to reorder gateways"))?;
    let mut reordered = serde_json::Map::new();
    for id in order {
        let entry = match providers.remove(id) {
            Some(entry) => entry,
            None if implicit_entry_json(id).is_some() => {
                let mut entry = implicit_entry_json(id).expect("checked above");
                entry["models"] = serde_json::json!([]);
                entry
            }
            None => return Err(Error::new("gateway to reorder is missing from models.json")),
        };
        reordered.insert(id.clone(), entry);
    }
    *providers = reordered;
    save_document(state, registry, document)
}

pub(super) fn save_document(
    state: &Path,
    registry: &Registry,
    document: Value,
) -> Result<Registry> {
    let path = state.join("models.json");
    let mut bytes = serde_json::to_vec_pretty(&document)
        .map_err(|err| Error::new(format!("models.json serialization: {err}")))?;
    bytes.push(b'\n');
    if bytes.len() as u64 > BODY_CAP {
        return Err(Error::new("models.json would be larger than 1 MiB"));
    }
    // Retain the same strict schema and validation as a normal local load.
    let file = parse_models(state, Some(&bytes))?;
    let next = registry_from_models(state, file, Some(bytes.clone()));
    fs::create_dir_all(state)
        .map_err(|err| Error::new(format!("failed to create {}: {err}", state.display())))?;
    let temp = state.join("models.json.tmp");
    // A fixed, exclusive temp also prevents two ahi writers from sharing a save.
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(|err| {
            Error::new(format!(
                "cannot create {} (another save or stale temp file; retry after reviewing it): {err}",
                temp.display()
            ))
        })?;
    let result = (|| -> Result<()> {
        output
            .write_all(&bytes)
            .and_then(|()| output.sync_all())
            .map_err(|err| Error::new(format!("cannot write {}: {err}", temp.display())))?;
        drop(output);
        // Check again immediately before replacing, including edits made while
        // preparing the save. External editors do not participate in our lock.
        check_snapshot(state, registry)?;
        fs::rename(&temp, &path)
            .map_err(|err| Error::new(format!("cannot replace {}: {err}", path.display())))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result?;
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(registry: &Registry, provider: usize) -> Vec<&str> {
        registry.providers[provider]
            .models
            .iter()
            .map(|model| model.id.as_str())
            .collect()
    }

    #[test]
    fn explicit_save_creates_valid_openai_config_and_order_survives_reload() {
        let dir = crate::fixture_directory::create_unique_fixture_directory("curation-create");
        fs::create_dir_all(dir.join("catalogs")).unwrap();
        fs::write(
            dir.join("catalogs/openai.json"),
            r#"["first","vendor/model/variant","unselected"]"#,
        )
        .unwrap();
        let registry = load_registry(&dir).unwrap();
        assert!(!dir.join("models.json").exists());
        let next = save_curated_models(
            &dir,
            &registry,
            "openai",
            &[
                "missing-from-catalog".into(),
                "vendor/model/variant".into(),
                "first".into(),
            ],
        )
        .unwrap();
        assert_eq!(
            ids(&next, 0),
            ["missing-from-catalog", "vendor/model/variant", "first"]
        );
        assert_eq!(
            next.providers[0].models[0].status,
            CatalogMembership::Absent
        );
        let body: Value =
            serde_json::from_slice(&fs::read(dir.join("models.json")).unwrap()).unwrap();
        assert_eq!(
            body,
            serde_json::json!({"providers":{"openai":{
                "baseUrl":"https://api.openai.com/v1", "api":"openai-completions",
                "models":[{"id":"missing-from-catalog"},{"id":"vendor/model/variant"},{"id":"first"}]
            }}})
        );
        assert_eq!(load_registry(&dir).unwrap(), next);
        let next = save_curated_models(
            &dir,
            &next,
            "openai",
            &["first".into(), "missing-from-catalog".into()],
        )
        .unwrap();
        assert_eq!(
            ids(&load_registry(&dir).unwrap(), 0),
            ["first", "missing-from-catalog"]
        );
        assert_eq!(ids(&next, 0), ["first", "missing-from-catalog"]);
        assert!(!dir.join("models.json.tmp").exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn saves_preserve_other_providers_settings_and_object_key_order() {
        let dir = crate::fixture_directory::create_unique_fixture_directory("curation-settings");
        let before = r#"{"providers":{"z-gateway":{"headers":{"Z":"$Z_HEADER","A":"gateway"},"models":[{"id":"vendor/kept"}],"apiKey":"$GATEWAY_KEY","api":"openai-completions","baseUrl":"https://gateway.example/v1"},"openai":{"models":[{"id":"old"}],"headers":{"X":"$OPENAI_HEADER"},"apiKey":"$OPENAI_KEY","api":"openai-completions","baseUrl":"https://custom.example/v1"}}}"#;
        fs::write(dir.join("models.json"), before).unwrap();
        let registry = load_registry(&dir).unwrap();
        save_curated_models(&dir, &registry, "openai", &["new/id".into()]).unwrap();
        let actual: Value =
            serde_json::from_slice(&fs::read(dir.join("models.json")).unwrap()).unwrap();
        let mut expected: Value = serde_json::from_str(before).unwrap();
        expected["providers"]["openai"]["models"] = serde_json::json!([{"id":"new/id"}]);
        assert_eq!(actual, expected);
        assert_eq!(
            actual["providers"]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["z-gateway", "openai"]
        );
        assert_eq!(
            actual["providers"]["openai"]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["models", "headers", "apiKey", "api", "baseUrl"]
        );
        assert_eq!(
            actual["providers"]["z-gateway"]["headers"]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["Z", "A"]
        );
        let next = load_registry(&dir).unwrap();
        save_curated_models(
            &dir,
            &next,
            "z-gateway",
            &["vendor/reordered".into(), "vendor/kept".into()],
        )
        .unwrap();
        let reloaded = load_registry(&dir).unwrap();
        assert_eq!(
            reloaded
                .providers
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            ["z-gateway", "openai"]
        );
        assert_eq!(ids(&reloaded, 1), ["new/id"]);
        assert_eq!(ids(&reloaded, 0), ["vendor/reordered", "vendor/kept"]);
        assert_eq!(reloaded.providers[0].base_url, "https://gateway.example/v1");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn sequence_shaped_configs_return_actionable_save_errors_without_changes() {
        let dir = crate::fixture_directory::create_unique_fixture_directory("curation-sequence");
        fs::create_dir_all(dir.join("catalogs")).unwrap();
        fs::write(dir.join("catalogs/openai.json"), r#"["fixture-model"]"#).unwrap();
        let path = dir.join("models.json");
        for (before, expected) in [
            (
                "[{}]",
                "cannot save curated models: document root must be a JSON object; edit models.json and press e to reload before saving",
            ),
            (
                r#"{"providers":{"openai":["https://api.openai.com/v1","openai-completions"]}}"#,
                "cannot save curated models: `providers.openai` must be a JSON object; edit models.json and press e to reload before saving",
            ),
        ] {
            fs::write(&path, before).unwrap();
            let registry = load_registry(&dir).unwrap();
            let original = registry.clone();
            let error =
                save_curated_models(&dir, &registry, "openai", &["fixture-model".into()])
                    .unwrap_err();
            assert_eq!(error.to_string(), format!("{}: {expected}", path.display()));
            assert_eq!(fs::read(&path).unwrap(), before.as_bytes());
            assert_eq!(registry, original);
            assert_eq!(load_registry(&dir).unwrap(), original);
            assert!(!dir.join("models.json.tmp").exists());
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn stale_and_failed_saves_preserve_file_bytes_and_loaded_registry() {
        let dir = crate::fixture_directory::create_unique_fixture_directory("curation-failure");
        let registry = load_registry(&dir).unwrap();
        let external = b"{\"providers\":{}}\n";
        fs::write(dir.join("models.json"), external).unwrap();
        let error = save_curated_models(&dir, &registry, "openai", &["new".into()]).unwrap_err();
        assert!(error
            .to_string()
            .contains("changed outside this view; press e"));
        assert_eq!(fs::read(dir.join("models.json")).unwrap(), external);
        assert_eq!(ids(&registry, 0), Vec::<&str>::new());
        let registry = load_registry(&dir).unwrap();
        fs::write(dir.join("models.json.tmp"), "other writer").unwrap();
        let error = save_curated_models(&dir, &registry, "openai", &["new".into()]).unwrap_err();
        assert!(error
            .to_string()
            .contains("another save or stale temp file"));
        assert_eq!(fs::read(dir.join("models.json")).unwrap(), external);
        assert_eq!(
            fs::read_to_string(dir.join("models.json.tmp")).unwrap(),
            "other writer"
        );
        assert_eq!(load_registry(&dir).unwrap(), registry);
        fs::remove_file(dir.join("models.json.tmp")).unwrap();
        let error =
            save_curated_models(&dir, &registry, "openai", &["invalid id".into()]).unwrap_err();
        assert!(error.to_string().contains("invalid model id"));
        assert_eq!(fs::read(dir.join("models.json")).unwrap(), external);
        assert_eq!(load_registry(&dir).unwrap(), registry);
        fs::write(dir.join("models.json"), "{broken external edit}").unwrap();
        assert!(save_curated_models(&dir, &registry, "openai", &["new".into()]).is_err());
        assert_eq!(
            fs::read_to_string(dir.join("models.json")).unwrap(),
            "{broken external edit}"
        );
        assert!(!dir.join("models.json.tmp").exists());
        fs::remove_dir_all(dir).unwrap();
    }
}
