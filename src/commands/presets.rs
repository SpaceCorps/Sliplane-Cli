//! `presets *`. Listing and showing need no account - the catalog is public and no key is sent
//! to it. Deploying is a normal `POST .../services` with the preset turned into a request.

use std::collections::HashSet;
use std::time::Duration;

use serde_json::{Value, json};

use crate::cli::Presets;
use crate::client::seg;
use crate::commands::services::parse_env;
use crate::commands::{client, print, wait};
use crate::error::Result;
use crate::{obj, presets};

pub fn run(c: Presets) -> Result<()> {
    match c {
        Presets::List { search, tag } => {
            let catalog = presets::fetch()?;
            print(Value::Array(catalog.list(search.as_deref(), tag.as_deref())))
        }
        Presets::Show { id } => print(presets::fetch()?.describe(&id)?),
        Presets::Deploy { id, project_id, server_id, name, env, secret_env, private, dry_run, w, a } => {
            // The account is chosen before anything else happens, a dry run included.
            let client = client(&a)?;
            let overrides = parse_env(&env, &secret_env)?;
            let instance = presets::fetch()?.instantiate(&id)?;
            let (body, warnings) =
                presets::service_body(&instance.preset, &server_id, name.as_deref(), &overrides, private)?;

            if dry_run {
                return print(obj! {
                    "dryRun" => true,
                    "preset" => id,
                    "projectId" => project_id,
                    "request" => presets::redact(&body, &instance.generated),
                    "warnings" => warnings,
                });
            }

            let service = client.post(&format!("projects/{}/services", seg(&project_id)), &body)?;
            let generated: Vec<Value> = instance
                .generated
                .iter()
                .map(|g| json!({ "key": g.key, "value": g.value, "secret": g.secret }))
                .collect();
            let mut out = obj! { "preset" => id, "service" => service.clone() };
            if !generated.is_empty() {
                out["generated"] = Value::Array(generated);
                out["note"] = json!(
                    "These values were generated for this deploy and are shown only here. Secret ones cannot be read back from the API."
                );
            }
            if !warnings.is_empty() {
                out["warnings"] = json!(warnings);
            }

            if w.wait {
                let service_id = service.get("id").and_then(Value::as_str).unwrap_or_default();
                let timeout = Duration::from_secs(w.timeout.unwrap_or(wait::DEFAULT_TIMEOUT_SECS));
                // A brand-new service has no earlier events to ignore.
                match wait::until_deployed(&client, &project_id, service_id, &HashSet::new(), timeout) {
                    Ok(outcome) => out["deploy"] = outcome,
                    Err(e) => {
                        // The service exists and its generated secrets are gone for good unless
                        // they are printed now, so print first and fail after.
                        out["deploy"] = json!({ "status": "not_confirmed", "error": e.message });
                        print(out)?;
                        return Err(e);
                    }
                }
            }
            print(out)
        }
    }
}
