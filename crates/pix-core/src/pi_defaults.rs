//! Read-only discovery of Pi's persisted model preferences.
//!
//! A draft session must be able to show the same model Pi will choose without
//! starting a disposable Pi child. Pi persists the choice in `settings.json`
//! and keeps provider metadata in `models-store.json`/`models.json`; this
//! module reads only those non-secret fields and returns a stable wire shape.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use directories::BaseDirs;
use pix_wire::{HostModelDefaults, ModelSummary, ThinkingLevel};
use serde_json::Value;

use crate::host_environment::HostEnvironment;

/// Discovers Pi's last persisted model and the models Pi can select from the
/// local catalog. Missing or malformed files produce an empty, safe result;
/// the UI can then explain that the host has not published a model yet.
#[must_use]
pub fn discover(environment: &HostEnvironment) -> HostModelDefaults {
    let agent_directory = agent_directory(environment);
    let settings = read_json(&agent_directory.join("settings.json"));

    let mut catalog = BTreeMap::new();
    if let Some(store) = read_json(&agent_directory.join("models-store.json")) {
        collect_model_store(&store, &mut catalog);
    }
    if let Some(models) = read_json(&agent_directory.join("models.json")) {
        collect_models_config(&models, &mut catalog);
    }

    let models: Vec<ModelSummary> = catalog.into_values().collect();
    resolve_defaults(settings.as_ref(), models)
}

fn resolve_defaults(settings: Option<&Value>, mut models: Vec<ModelSummary>) -> HostModelDefaults {
    let default_provider = settings
        .and_then(|value| value.get("defaultProvider"))
        .and_then(Value::as_str);
    let default_id = settings
        .and_then(|value| value.get("defaultModel"))
        .and_then(Value::as_str);
    let persisted_model = default_provider
        .zip(default_id)
        .filter(|(provider, id)| !provider.trim().is_empty() && !id.trim().is_empty())
        .map(|(provider, id)| {
            ModelSummary {
                provider: provider.to_owned(),
                id: id.to_owned(),
                // When Pi's catalog has not been written yet, keep the exact
                // persisted identifier visible instead of inventing a product
                // name. Pi uses `id` as the custom-model display name too.
                name: id.to_owned(),
                reasoning: false,
                input: Vec::new(),
                thinking_levels: Vec::new(),
            }
        });
    let model = persisted_model.as_ref().and_then(|persisted| {
        models
            .iter()
            .find(|candidate| {
                candidate.provider == persisted.provider && candidate.id == persisted.id
            })
            .cloned()
            .or_else(|| {
                models.insert(0, persisted.clone());
                Some(persisted.clone())
            })
    });
    let thinking_level = settings
        .and_then(|value| value.get("defaultThinkingLevel"))
        .and_then(Value::as_str)
        .and_then(parse_thinking_level);

    HostModelDefaults {
        model,
        models,
        thinking_level,
    }
}

fn agent_directory(environment: &HostEnvironment) -> PathBuf {
    if let Some(directory) = environment
        .value("PI_CODING_AGENT_DIR")
        .filter(|value| !value.to_string_lossy().trim().is_empty())
    {
        let directory = PathBuf::from(directory);
        if (directory == Path::new("~") || directory.starts_with("~/"))
            && let Some(home) = environment
                .value("HOME")
                .filter(|value| !value.to_string_lossy().trim().is_empty())
        {
            let suffix = directory.strip_prefix("~").expect("checked tilde prefix");
            return PathBuf::from(home).join(suffix);
        }
        return directory;
    }

    // `directories` consults the current process environment. A GUI-launched
    // host may instead be running with the login-shell capture held by
    // `HostEnvironment`, so prefer that capture for the same home directory
    // Pi receives when it is spawned.
    if let Some(home) = environment
        .value("HOME")
        .filter(|value| !value.to_string_lossy().trim().is_empty())
    {
        return PathBuf::from(home).join(".pi/agent");
    }
    BaseDirs::new()
        .map(|directories| directories.home_dir().join(".pi/agent"))
        .unwrap_or_default()
}

fn read_json(path: &Path) -> Option<Value> {
    fs::read_to_string(path)
        .ok()
        .and_then(|content| serde_json::from_str(&content).ok())
}

fn collect_model_store(value: &Value, catalog: &mut BTreeMap<String, ModelSummary>) {
    let Some(providers) = value.as_object() else {
        return;
    };
    for (provider, entry) in providers {
        let Some(models) = entry.get("models").and_then(Value::as_array) else {
            continue;
        };
        collect_models(provider, models, catalog);
    }
}

fn collect_models_config(value: &Value, catalog: &mut BTreeMap<String, ModelSummary>) {
    let Some(providers) = value.get("providers").and_then(Value::as_object) else {
        return;
    };
    for (provider, entry) in providers {
        let Some(models) = entry.get("models").and_then(Value::as_array) else {
            continue;
        };
        collect_models(provider, models, catalog);
    }
}

fn collect_models(provider: &str, models: &[Value], catalog: &mut BTreeMap<String, ModelSummary>) {
    for model in models {
        let Some(id) = model.get("id").and_then(Value::as_str) else {
            continue;
        };
        if provider.trim().is_empty() || id.trim().is_empty() {
            continue;
        }
        let name = model
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.trim().is_empty())
            .unwrap_or(id);
        let reasoning = model
            .get("reasoning")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let summary = ModelSummary {
            provider: provider.to_owned(),
            id: id.to_owned(),
            name: name.to_owned(),
            reasoning,
            input: model
                .get("input")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(Value::as_str)
                        .map(ToOwned::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
            thinking_levels: supported_thinking_levels(model, reasoning),
        };
        catalog.insert(format!("{provider}\u{1f}|{id}"), summary);
    }
}

/// Mirrors Pi's `getSupportedThinkingLevels` behavior using the persisted
/// model metadata. Standard levels remain available unless Pi explicitly
/// stores `null`; extended `xhigh`/`max` levels are available only when the
/// model maps them explicitly.
pub(crate) fn supported_thinking_levels(value: &Value, reasoning: bool) -> Vec<ThinkingLevel> {
    if !reasoning {
        return vec![ThinkingLevel::Off];
    }

    [
        ("off", ThinkingLevel::Off, false),
        ("minimal", ThinkingLevel::Minimal, false),
        ("low", ThinkingLevel::Low, false),
        ("medium", ThinkingLevel::Medium, false),
        ("high", ThinkingLevel::High, false),
        ("xhigh", ThinkingLevel::Xhigh, true),
        ("max", ThinkingLevel::Max, true),
    ]
    .into_iter()
    .filter_map(|(name, level, extended)| {
        let mapped = value
            .get("thinkingLevelMap")
            .and_then(Value::as_object)
            .and_then(|map| map.get(name));
        let supported = if extended {
            mapped.is_some_and(|value| !value.is_null())
        } else {
            !mapped.is_some_and(Value::is_null)
        };
        supported.then_some(level)
    })
    .collect()
}

fn parse_thinking_level(value: &str) -> Option<ThinkingLevel> {
    match value {
        "off" => Some(ThinkingLevel::Off),
        "minimal" => Some(ThinkingLevel::Minimal),
        "low" => Some(ThinkingLevel::Low),
        "medium" => Some(ThinkingLevel::Medium),
        "high" => Some(ThinkingLevel::High),
        "xhigh" => Some(ThinkingLevel::Xhigh),
        "max" => Some(ThinkingLevel::Max),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::ffi::OsString;
    use std::fs;

    use pix_wire::ModelSummary;
    use serde_json::json;
    use tempfile::tempdir;

    use super::{
        collect_model_store, collect_models_config, discover, parse_thinking_level,
        resolve_defaults, supported_thinking_levels,
    };

    #[test]
    fn parses_persisted_model_store_metadata_without_fabricating_defaults() {
        let value = json!({
            "provider": {
                "models": [{"id": "actual-id", "name": "Actual name", "reasoning": true}]
            }
        });
        let mut catalog = BTreeMap::new();
        collect_model_store(&value, &mut catalog);
        assert_eq!(
            catalog.values().next(),
            Some(&ModelSummary {
                provider: "provider".into(),
                id: "actual-id".into(),
                name: "Actual name".into(),
                reasoning: true,
                input: Vec::new(),
                thinking_levels: vec![
                    pix_wire::ThinkingLevel::Off,
                    pix_wire::ThinkingLevel::Minimal,
                    pix_wire::ThinkingLevel::Low,
                    pix_wire::ThinkingLevel::Medium,
                    pix_wire::ThinkingLevel::High,
                ],
            })
        );
    }

    #[test]
    fn custom_models_use_pi_id_name_and_reasoning_defaults() {
        let value = json!({
            "providers": {
                "custom": {"models": [{"id": "configured-model"}]}
            }
        });
        let mut catalog = BTreeMap::new();
        collect_models_config(&value, &mut catalog);
        let model = catalog.values().next().expect("custom model");
        assert_eq!(model.name, "configured-model");
        assert!(!model.reasoning);
        assert_eq!(model.thinking_levels, vec![pix_wire::ThinkingLevel::Off]);
    }

    #[test]
    fn rejects_unknown_thinking_levels() {
        assert_eq!(parse_thinking_level("turbo"), None);
        assert_eq!(
            parse_thinking_level("high"),
            Some(pix_wire::ThinkingLevel::High)
        );
    }

    #[test]
    fn derives_pi_thinking_choices_from_model_capabilities() {
        let value = json!({
            "thinkingLevelMap": {
                "minimal": null,
                "low": null,
                "medium": null,
                "high": "high",
                "max": "max"
            }
        });
        assert_eq!(
            supported_thinking_levels(&value, true),
            vec![
                pix_wire::ThinkingLevel::Off,
                pix_wire::ThinkingLevel::High,
                pix_wire::ThinkingLevel::Max,
            ]
        );
        assert_eq!(
            supported_thinking_levels(&value, false),
            vec![pix_wire::ThinkingLevel::Off]
        );
    }

    #[test]
    fn falls_back_to_the_exact_persisted_identifier_when_catalog_is_missing() {
        let settings = json!({
            "defaultProvider": "custom-provider",
            "defaultModel": "custom-model",
            "defaultThinkingLevel": "high"
        });
        let defaults = resolve_defaults(Some(&settings), Vec::new());
        let model = defaults.model.expect("persisted default");
        assert_eq!(model.provider, "custom-provider");
        assert_eq!(model.id, "custom-model");
        assert_eq!(model.name, "custom-model");
        assert_eq!(defaults.models, vec![model]);
    }

    #[test]
    fn discovers_defaults_from_the_same_captured_agent_directory_as_pi() {
        let directory = tempdir().expect("agent directory");
        fs::write(
            directory.path().join("settings.json"),
            r#"{"defaultProvider":"provider","defaultModel":"model","defaultThinkingLevel":"high"}"#,
        )
        .expect("settings");
        fs::write(
            directory.path().join("models-store.json"),
            r#"{"provider":{"models":[{"id":"model","name":"Real model","reasoning":true}]}}"#,
        )
        .expect("model store");

        let environment = crate::host_environment::HostEnvironment::captured_for_tests(
            "/bin/zsh".into(),
            vec![(
                OsString::from("PI_CODING_AGENT_DIR"),
                directory.path().as_os_str().to_owned(),
            )],
        );
        let defaults = discover(&environment);

        assert_eq!(
            defaults.model.as_ref().map(|model| model.id.as_str()),
            Some("model")
        );
        assert_eq!(defaults.models.len(), 1);
        assert_eq!(defaults.thinking_level, Some(pix_wire::ThinkingLevel::High));
    }
}
