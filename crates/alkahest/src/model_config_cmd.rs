//! `alkahest config model <name> [show|set|unset|path]`: the per-model configuration overlay
//! (ALK-US-0014, ALK-SP-0010.R4).

use std::process::ExitCode;
use std::str::FromStr;

use alkahest_core::config::Loaded;
use alkahest_store::{
    Catalog, ModelName, ModelOverlays, ModelRef, ModelSettings, ScopedRef, StoreError,
};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::json;

use crate::cli::ModelConfigAction;
use crate::models_cmd::open_catalog;

/// Runs the command. The model may be installed (any store) or only a name: configuring a
/// model ahead of its pull is allowed.
pub fn run(
    model: &str,
    action: Option<&ModelConfigAction>,
    loaded: &Loaded,
    json: bool,
) -> Result<ExitCode> {
    let catalog = open_catalog(loaded)?;
    let overlays = catalog
        .overlays()
        .ok_or_else(|| anyhow!("no overlay store"))?;
    let name = target_name(&catalog, model)?;
    match action.unwrap_or(&ModelConfigAction::Show) {
        ModelConfigAction::Path => println!("{}", overlays.model_path(&name).display()),
        ModelConfigAction::Set { assignments } => {
            let updated = overlays.update(&name, |s| {
                for a in assignments {
                    let (k, v) = a
                        .split_once('=')
                        .ok_or_else(|| format!("{a:?} is not KEY=VALUE"))?;
                    s.set_key(k, v)?;
                }
                Ok(())
            })?;
            print_overlay(&name, &updated, json)?;
        }
        ModelConfigAction::Unset { keys } => {
            let updated =
                overlays.update(&name, |s| keys.iter().try_for_each(|k| s.unset_key(k)))?;
            print_overlay(&name, &updated, json)?;
        }
        ModelConfigAction::Show => show(&catalog, overlays, &name, model, loaded, json)?,
    }
    Ok(ExitCode::SUCCESS)
}

/// The store name to configure: the installed model's, else the parsed reference.
fn target_name(catalog: &Catalog, input: &str) -> Result<ModelName> {
    match catalog.resolve(input) {
        Ok(e) => Ok(e.entry.name),
        Err(StoreError::ModelNotFound { .. }) => {
            let scoped = ScopedRef::from_str(input)?;
            match scoped.reference {
                ModelRef::Digest(_) => bail!("model {input:?} not found"),
                r => r
                    .store_name()?
                    .ok_or_else(|| anyhow!("model {input:?} not found")),
            }
        }
        Err(e) => Err(e.into()),
    }
}

fn print_overlay(name: &ModelName, s: &ModelSettings, json: bool) -> Result<()> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({"model": name.display_short(), "settings": s}))?
        );
    } else if s.is_empty() {
        println!("{}: no overlay settings", name.display_short());
    } else {
        print!("{}", toml_of(s)?);
    }
    Ok(())
}

fn toml_of(s: &ModelSettings) -> Result<String> {
    // The same rendering the overlay file uses.
    toml::to_string_pretty(s).context("rendering settings")
}

fn show(
    catalog: &Catalog,
    overlays: &ModelOverlays,
    name: &ModelName,
    input: &str,
    loaded: &Loaded,
    json: bool,
) -> Result<()> {
    let overlay = overlays.get(name)?;
    let recommended = overlays.recommended(name)?;
    let global = ModelSettings::global_defaults(&loaded.config);
    let eff = match catalog.resolve(input) {
        Ok(entry) => {
            catalog.effective_settings(&entry, &ModelSettings::default(), &global, None)?
        }
        Err(_) => overlays.resolve(
            name,
            &ModelSettings::default(),
            &global,
            None,
            &ModelSettings::default(),
        )?,
    };
    if json {
        let origins: serde_json::Map<_, _> = eff
            .origins
            .iter()
            .map(|(k, v)| (k.clone(), json!(v.as_str())))
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "model": name.display_short(),
                "path": overlays.model_path(name),
                "overlay": overlay,
                "recommended": recommended,
                "effective": eff.settings,
                "origins": origins,
            }))?
        );
        return Ok(());
    }
    println!("# overlay: {}", overlays.model_path(name).display());
    if overlay.is_empty() {
        println!("(none)");
    } else {
        print!("{}", toml_of(&overlay)?);
    }
    if let Some(r) = &recommended {
        println!("\n# recommended defaults (written at pull time)");
        println!("{}", serde_json::to_string_pretty(r)?);
    }
    println!("\n# effective settings (value  [layer])");
    let effective = serde_json::to_value(&eff.settings)?;
    if let Some(map) = effective.as_object() {
        for (k, v) in map {
            if let Some(kw) = v.as_object().filter(|_| k == "chat_template_kwargs") {
                for (kk, vv) in kw {
                    let key = format!("{k}.{kk}");
                    println!("{key} = {vv}  [{}]", layer_of(&eff, &key));
                }
            } else {
                println!("{k} = {v}  [{}]", layer_of(&eff, k));
            }
        }
    }
    Ok(())
}

fn layer_of(e: &alkahest_store::Effective, key: &str) -> &'static str {
    e.origin(key).map_or("-", |l| l.as_str())
}
