//! The preset catalog behind `presets` - the same list the Sliplane dashboard deploys from.
//!
//! It is not part of the versioned API: a public, unauthenticated JSON document at
//! sliplane.io with no version field. Nothing here sends an API key to it, and anything
//! unexpected in it is an error rather than a guess, because the catalog decides which image
//! runs with which secrets.
//!
//! Placeholders like `__SLIPLANE_GENERATED_3__` stand for random values chosen per deploy, so
//! two deploys of one preset never share a name, a volume or a password.

use std::collections::HashMap;
use std::time::Duration;

use serde_json::{Map, Value, json};

use crate::error::{Error, ErrorCode, Result};

const URL: &str = "https://sliplane.io/api/presets";
const MAX_BYTES: u64 = 4 * 1024 * 1024;
const MARK: &str = "__SLIPLANE_GENERATED_";
const ALPHABET: &[u8; 62] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    /// Random letters and digits, `length` characters.
    String,
    /// `length` random bytes, hex encoded.
    Hex,
    /// A version 4 UUID.
    Uuid,
}

struct Rule {
    placeholder: String,
    kind: Kind,
    length: usize,
}

pub struct Catalog {
    presets: Vec<Value>,
    rules: Vec<Rule>,
}

/// An environment variable whose value was generated for this deploy.
pub struct Generated {
    pub key: String,
    pub value: String,
    pub secret: bool,
}

/// One preset with every placeholder replaced.
pub struct Instance {
    pub preset: Value,
    pub generated: Vec<Generated>,
}

fn broken(what: impl std::fmt::Display) -> Error {
    Error::other(format!("The preset catalog {what}.")).fix(
        "The catalog is not part of the versioned API and may have changed. Update sliplane, or report it at \
         https://github.com/SpaceCorps/Sliplane-Cli/issues",
    )
}

/// Downloads the catalog. `SLIPLANE_PRESETS_URL` points it elsewhere, for tests.
pub fn fetch() -> Result<Catalog> {
    let url = std::env::var("SLIPLANE_PRESETS_URL").ok().filter(|s| !s.trim().is_empty()).unwrap_or_else(|| URL.into());
    // A bare agent: the catalog is public, and the API key has no business going anywhere near it.
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .http_status_as_error(false)
        .user_agent(concat!("sliplane-cli/", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
    let network = |e: ureq::Error| {
        Error::new(ErrorCode::Network, "Could not download the preset catalog.")
            .detail(format!("{url}: {e}"))
            .fix("Retry once, then stop.")
    };
    let mut response = agent.get(&url).header("Accept", "application/json").call().map_err(network)?;
    let status = response.status().as_u16();
    if status != 200 {
        return Err(Error::other(format!("The preset catalog answered HTTP {status}.")).detail(url));
    }
    let bytes = response.body_mut().with_config().limit(MAX_BYTES).read_to_vec().map_err(network)?;
    parse(&String::from_utf8_lossy(&bytes))
}

pub fn parse(text: &str) -> Result<Catalog> {
    let root: Value = serde_json::from_str(text).map_err(|e| broken(format!("is not JSON ({e})")))?;
    let presets = root
        .get("presets")
        .and_then(Value::as_array)
        .filter(|p| !p.is_empty())
        .ok_or_else(|| broken("lists no presets"))?
        .clone();

    let mut rules = Vec::new();
    for (i, r) in root.get("generatedValues").and_then(Value::as_array).into_iter().flatten().enumerate() {
        let placeholder = format!("{MARK}{i}__");
        if r.get("placeholder").and_then(Value::as_str) != Some(placeholder.as_str()) {
            return Err(broken(format!("has generated value {i} out of order")));
        }
        let length = r.get("length").and_then(Value::as_u64).filter(|l| (1..=128).contains(l)).map(|l| l as usize);
        let (kind, length) = match (r.get("kind").and_then(Value::as_str), length) {
            (Some("string"), Some(l)) => (Kind::String, l),
            (Some("hex"), Some(l)) => (Kind::Hex, l),
            (Some("uuid"), Some(16)) => (Kind::Uuid, 16),
            _ => {
                return Err(broken(format!(
                    "has a generated value ({placeholder}) of a kind or length this version does not know"
                )));
            }
        };
        rules.push(Rule { placeholder, kind, length });
    }

    let mut seen = std::collections::HashSet::new();
    for p in &presets {
        let id = p.pointer("/settings/presetId").and_then(Value::as_str).filter(|s| !s.is_empty());
        match id {
            Some(id) if seen.insert(id.to_string()) => {}
            Some(id) => return Err(broken(format!("lists '{id}' twice"))),
            None => return Err(broken("has a preset without an id")),
        }
    }
    Ok(Catalog { presets, rules })
}

fn text<'a>(v: &'a Value, pointer: &str) -> &'a str {
    v.pointer(pointer).and_then(Value::as_str).unwrap_or("")
}

fn id_of(preset: &Value) -> &str {
    text(preset, "/settings/presetId")
}

impl Catalog {
    /// Presets matching `search` (id, name, description or tag, case-insensitive) and `tag`.
    pub fn list(&self, search: Option<&str>, tag: Option<&str>) -> Vec<Value> {
        let search = search.map(str::to_lowercase);
        let tag = tag.map(str::to_lowercase);
        self.presets
            .iter()
            .filter(|p| {
                let tags: Vec<String> = tags_of(p).iter().map(|t| t.to_lowercase()).collect();
                tag.as_ref().is_none_or(|t| tags.contains(t))
                    && search.as_ref().is_none_or(|s| {
                        [id_of(p), text(p, "/name"), text(p, "/description")]
                            .iter()
                            .any(|f| f.to_lowercase().contains(s.as_str()))
                            || tags.iter().any(|t| t.contains(s.as_str()))
                    })
            })
            .map(summary)
            .collect()
    }

    fn find(&self, id: &str) -> Result<&Value> {
        self.presets.iter().find(|p| id_of(p) == id).ok_or_else(|| {
            Error::new(ErrorCode::NotFound, format!("There is no preset '{id}'.")).fix("sliplane presets list")
        })
    }

    /// What a preset would deploy, with the random parts shown as `<random>` and the secrets hidden.
    pub fn describe(&self, id: &str) -> Result<Value> {
        let mut preset = self.find(id)?.clone();
        let generated_env: Vec<bool> = envs(&preset).iter().map(|e| text(e, "/value").contains(MARK)).collect();
        let rules = &self.rules;
        map_strings(&mut preset, &|s| {
            s.contains(MARK).then(|| rules.iter().fold(s.to_string(), |s, r| s.replace(&r.placeholder, "<random>")))
        });

        let mut out = summary(&preset).as_object().cloned().unwrap_or_default();
        let env: Vec<Value> = envs(&preset)
            .iter()
            .zip(generated_env)
            .map(|(e, generated)| {
                let secret = e.get("secret").and_then(Value::as_bool).unwrap_or(false);
                let value = text(e, "/value");
                let mut v = Map::new();
                v.insert("key".into(), json!(text(e, "/key")));
                if generated {
                    v.insert("value".into(), json!("<generated>"));
                } else if secret && !value.is_empty() {
                    v.insert("value".into(), json!("<hidden>"));
                } else {
                    v.insert("value".into(), json!(value));
                }
                v.insert("secret".into(), json!(secret));
                if value.is_empty() {
                    v.insert("required".into(), json!(true));
                }
                Value::Object(v)
            })
            .collect();
        out.insert("env".into(), Value::Array(env));
        let volumes: Vec<Value> = volume_mounts(&preset)
            .into_iter()
            .map(|(name, mount)| json!({ "name": name, "mountPath": mount }))
            .collect();
        out.insert("volumes".into(), Value::Array(volumes));
        if let Some(r) = preset.get("requirements") {
            out.insert("requirements".into(), r.clone());
        }
        Ok(Value::Object(out))
    }

    /// A copy of the preset with fresh random values. The catalog itself is never changed, so a
    /// second call gives different names and secrets.
    pub fn instantiate(&self, id: &str) -> Result<Instance> {
        let template = self.find(id)?;
        let serialized = template.to_string();
        let mut values: HashMap<&str, String> = HashMap::new();
        for rule in self.rules.iter().filter(|r| serialized.contains(&r.placeholder)) {
            values.insert(rule.placeholder.as_str(), random(rule.kind, rule.length)?);
        }

        let mut preset = template.clone();
        map_strings(&mut preset, &|s| {
            s.contains(MARK)
                .then(|| values.iter().fold(s.to_string(), |s, (placeholder, value)| s.replace(placeholder, value)))
        });
        if preset.to_string().contains(MARK) {
            return Err(broken(format!("uses a generated value for '{id}' that it never defines")));
        }

        let generated = envs(template)
            .iter()
            .zip(envs(&preset))
            .filter(|(before, _)| text(before, "/value").contains(MARK))
            .map(|(_, after)| Generated {
                key: text(after, "/key").to_string(),
                value: text(after, "/value").to_string(),
                secret: after.get("secret").and_then(Value::as_bool).unwrap_or(false),
            })
            .collect();
        Ok(Instance { preset, generated })
    }
}

fn tags_of(preset: &Value) -> Vec<&str> {
    preset.get("tags").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).collect()
}

fn envs(preset: &Value) -> &[Value] {
    preset.pointer("/settings/envs").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[])
}

/// (volume name, container mount path) for each volume the preset mounts.
fn volume_mounts(preset: &Value) -> Vec<(String, String)> {
    let names: HashMap<&str, &str> = preset
        .get("volumes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|v| (text(v, "/id"), text(v, "/name")))
        .collect();
    preset
        .pointer("/settings/volumes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|m| {
            (names.get(text(m, "/id")).copied().unwrap_or("").to_string(), text(m, "/containerMountPath").to_string())
        })
        .collect()
}

fn summary(preset: &Value) -> Value {
    let mut out = Map::new();
    out.insert("id".into(), json!(id_of(preset)));
    out.insert("name".into(), json!(text(preset, "/name")));
    out.insert("description".into(), json!(text(preset, "/description")));
    out.insert("tags".into(), json!(tags_of(preset)));
    out.insert("image".into(), json!(text(preset, "/settings/externalDockerImage")));
    out.insert("public".into(), json!(preset.pointer("/settings/public").and_then(Value::as_bool).unwrap_or(false)));
    if let Some(p) = preset.pointer("/settings/protocol").and_then(Value::as_str).filter(|p| !p.is_empty()) {
        out.insert("protocol".into(), json!(p));
    }
    if let Some(m) = preset.pointer("/requirements/min/memory").and_then(Value::as_u64) {
        out.insert("minMemoryMb".into(), json!(m));
    }
    Value::Object(out)
}

/// The body for `POST /projects/{id}/services`, and anything worth warning about.
///
/// `overrides` are `{key, value, secret}` objects: they replace a preset variable of the same
/// name (which stays secret if the preset says so) or add a new one.
pub fn service_body(
    preset: &Value,
    server_id: &str,
    name: Option<&str>,
    overrides: &[Value],
    private: bool,
) -> Result<(Value, Vec<String>)> {
    let id = id_of(preset);
    let image = text(preset, "/settings/externalDockerImage");
    if text(preset, "/settings/deploySource") != "registry" || image.is_empty() {
        return Err(broken(format!(
            "describes '{id}' as something other than a registry image, which this version cannot deploy"
        )));
    }
    let name = name.filter(|n| !n.is_empty()).unwrap_or_else(|| text(preset, "/settings/name"));
    if name.is_empty() {
        return Err(broken(format!("gives '{id}' no name")));
    }

    let mut warnings = Vec::new();
    let public = preset.pointer("/settings/public").and_then(Value::as_bool).unwrap_or(false) && !private;
    let protocol = text(preset, "/settings/protocol");
    let mut network = json!({ "public": public });
    if public {
        if !["http", "tcp", "udp"].contains(&protocol) {
            return Err(broken(format!("makes '{id}' public with protocol '{protocol}'")));
        }
        network["protocol"] = json!(protocol);
        if protocol != "http" {
            warnings.push(format!(
                "{id} is exposed to the internet on a public {protocol} port, protected only by its credentials. \
                 Pass --private to keep it off the public internet."
            ));
        }
    }

    let mut env: Vec<Value> = envs(preset)
        .iter()
        .map(|e| json!({ "key": text(e, "/key"), "value": text(e, "/value"), "secret": e.get("secret").and_then(Value::as_bool).unwrap_or(false) }))
        .collect();
    for o in overrides {
        let key = text(o, "/key");
        let secret = o.get("secret").and_then(Value::as_bool).unwrap_or(false);
        match env.iter_mut().find(|e| text(e, "/key") == key) {
            Some(existing) => {
                existing["value"] = json!(text(o, "/value"));
                existing["secret"] = json!(existing["secret"].as_bool().unwrap_or(false) || secret);
            }
            None => env.push(json!({ "key": key, "value": text(o, "/value"), "secret": secret })),
        }
    }
    let missing: Vec<&str> = env.iter().filter(|e| text(e, "/value").is_empty()).map(|e| text(e, "/key")).collect();
    if !missing.is_empty() {
        let flags: Vec<String> = missing.iter().map(|k| format!("--secret-env {k}=<value>")).collect();
        return Err(Error::invalid(format!("The '{id}' preset needs a value for: {}.", missing.join(", ")))
            .fix(format!("Add {} to the command.", flags.join(" "))));
    }

    let mut volumes = Vec::new();
    for (volume, mount) in volume_mounts(preset) {
        if volume.is_empty() || !mount.starts_with('/') {
            return Err(broken(format!(
                "mounts a volume of '{id}' without a name or at '{mount}', which is not an absolute path"
            )));
        }
        volumes.push(json!({ "name": volume, "mountPath": mount }));
    }

    let mut body = json!({
        "name": name,
        "serverId": server_id,
        "deployment": { "url": image },
        "network": network,
        "env": env,
    });
    if !volumes.is_empty() {
        body["volumes"] = Value::Array(volumes);
    }
    if let Some(cmd) = Some(text(preset, "/settings/cmd")).filter(|c| !c.is_empty()) {
        body["cmd"] = json!(cmd);
    }
    let healthcheck = text(preset, "/settings/healthcheckPath");
    if public && protocol == "http" && !healthcheck.is_empty() {
        body["healthcheck"] = json!(healthcheck);
    }
    Ok((body, warnings))
}

/// The request with every secret and generated value hidden, for `--dry-run`.
pub fn redact(body: &Value, generated: &[Generated]) -> Value {
    let mut body = body.clone();
    if let Some(env) = body.get_mut("env").and_then(Value::as_array_mut) {
        for e in env {
            let hide = e.get("secret").and_then(Value::as_bool).unwrap_or(false)
                || generated.iter().any(|g| g.key == text(e, "/key"));
            if hide {
                e["value"] = json!("<hidden>");
            }
        }
    }
    body
}

fn map_strings(v: &mut Value, f: &dyn Fn(&str) -> Option<String>) {
    match v {
        Value::String(s) => {
            if let Some(replacement) = f(s) {
                *s = replacement;
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|i| map_strings(i, f)),
        Value::Object(map) => map.values_mut().for_each(|i| map_strings(i, f)),
        _ => {}
    }
}

fn random_bytes(n: usize) -> Result<Vec<u8>> {
    let mut buf = vec![0u8; n];
    getrandom::fill(&mut buf)
        .map_err(|e| Error::other("The system could not provide random bytes.").detail(e.to_string()))?;
    Ok(buf)
}

fn random(kind: Kind, length: usize) -> Result<String> {
    Ok(match kind {
        Kind::String => {
            let mut out = String::with_capacity(length);
            while out.len() < length {
                for b in random_bytes(64)? {
                    // 248 is the largest multiple of 62 below 256; anything above would favour
                    // the first characters of the alphabet.
                    if b < 248 && out.len() < length {
                        out.push(ALPHABET[(b % 62) as usize] as char);
                    }
                }
            }
            out
        }
        Kind::Hex => random_bytes(length)?.iter().map(|b| format!("{b:02x}")).collect(),
        Kind::Uuid => {
            let mut b = random_bytes(16)?;
            b[6] = (b[6] & 0x0f) | 0x40;
            b[8] = (b[8] & 0x3f) | 0x80;
            let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
            format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CATALOG: &str = r#"{
      "generatedValues": [
        {"placeholder": "__SLIPLANE_GENERATED_0__", "kind": "string", "length": 4},
        {"placeholder": "__SLIPLANE_GENERATED_1__", "kind": "string", "length": 16},
        {"placeholder": "__SLIPLANE_GENERATED_2__", "kind": "uuid", "length": 16},
        {"placeholder": "__SLIPLANE_GENERATED_3__", "kind": "hex", "length": 8}
      ],
      "presets": [
        {"name": "PostgreSQL", "tags": ["database"], "description": "A relational database.",
         "volumes": [{"id": "pg-__SLIPLANE_GENERATED_0__", "name": "pg-data-__SLIPLANE_GENERATED_0__"}],
         "settings": {"presetId": "postgres", "name": "Postgres-__SLIPLANE_GENERATED_0__",
           "envs": [{"key": "POSTGRES_PASSWORD", "value": "__SLIPLANE_GENERATED_1__", "secret": false},
                    {"key": "POSTGRES_DB", "value": "mydb", "secret": false}],
           "public": true, "protocol": "tcp", "deploySource": "registry",
           "externalDockerImage": "docker.io/library/postgres:18.6", "cmd": "",
           "volumes": [{"id": "pg-__SLIPLANE_GENERATED_0__", "containerMountPath": "/var/lib/postgresql"}]}},
        {"name": "n8n", "tags": ["automation"], "description": "Workflow automation.",
         "requirements": {"min": {"memory": 2048}},
         "volumes": [{"id": "n8n-__SLIPLANE_GENERATED_0__", "name": "n8n-data-__SLIPLANE_GENERATED_0__"}],
         "settings": {"presetId": "n8n", "name": "n8n-__SLIPLANE_GENERATED_0__", "healthcheckPath": "/healthz",
           "envs": [{"key": "N8N_ENCRYPTION_KEY", "value": "__SLIPLANE_GENERATED_1__", "secret": true},
                    {"key": "TRACE", "value": "__SLIPLANE_GENERATED_2__ __SLIPLANE_GENERATED_3__", "secret": false},
                    {"key": "OPENAI_API_KEY", "value": "", "secret": true}],
           "public": true, "protocol": "http", "deploySource": "registry",
           "externalDockerImage": "docker.io/n8nio/n8n:2.35.7", "cmd": "start",
           "volumes": [{"id": "n8n-__SLIPLANE_GENERATED_0__", "containerMountPath": "/home/node/.n8n"}]}}
      ]
    }"#;

    fn catalog() -> Catalog {
        parse(CATALOG).unwrap()
    }

    #[test]
    fn instantiating_replaces_every_placeholder_consistently() {
        let i = catalog().instantiate("postgres").unwrap();
        assert!(!i.preset.to_string().contains(MARK));
        let suffix = i.preset["settings"]["name"].as_str().unwrap().strip_prefix("Postgres-").unwrap().to_string();
        assert_eq!(suffix.len(), 4);
        // The volume's id, name and mount all use the same suffix, or the mount would not resolve.
        assert_eq!(i.preset["volumes"][0]["id"], json!(format!("pg-{suffix}")));
        assert_eq!(i.preset["volumes"][0]["name"], json!(format!("pg-data-{suffix}")));
        assert_eq!(i.generated.len(), 1);
        assert_eq!(i.generated[0].key, "POSTGRES_PASSWORD");
        assert_eq!(i.generated[0].value.len(), 16);
        assert!(i.generated[0].value.bytes().all(|b| b.is_ascii_alphanumeric()));
    }

    #[test]
    fn every_deploy_gets_fresh_values() {
        let c = catalog();
        let (a, b) = (c.instantiate("postgres").unwrap(), c.instantiate("postgres").unwrap());
        assert_ne!(a.generated[0].value, b.generated[0].value);
        // The catalog itself still holds the placeholders.
        assert!(c.find("postgres").unwrap().to_string().contains(MARK));
    }

    #[test]
    fn uuid_and_hex_values_have_the_right_shape() {
        let i = catalog().instantiate("n8n").unwrap();
        let trace = i.generated.iter().find(|g| g.key == "TRACE").unwrap();
        let (uuid, hex) = trace.value.split_once(' ').unwrap();
        let parts: Vec<&str> = uuid.split('-').collect();
        assert_eq!(parts.iter().map(|p| p.len()).collect::<Vec<_>>(), [8, 4, 4, 4, 12]);
        assert!(parts[2].starts_with('4'), "version nibble: {uuid}");
        assert!("89ab".contains(parts[3].chars().next().unwrap()), "variant nibble: {uuid}");
        assert_eq!(hex.len(), 16, "8 bytes, hex encoded");
        assert!(hex.bytes().all(|b| b.is_ascii_hexdigit()));
    }

    #[test]
    fn string_values_use_the_whole_alphabet_without_bias() {
        let mut counts = HashMap::new();
        for _ in 0..200 {
            for c in random(Kind::String, 128).unwrap().chars() {
                *counts.entry(c).or_insert(0usize) += 1;
            }
        }
        assert_eq!(counts.len(), 62, "all 62 characters should appear in 25,600 draws");
        let (min, max) = (counts.values().min().unwrap(), counts.values().max().unwrap());
        // Expected ~413 each; modulo bias would put the first 8 characters well above the rest.
        assert!(*max < *min * 2, "min {min}, max {max}");
    }

    #[test]
    fn service_body_for_a_public_http_preset() {
        let i = catalog().instantiate("n8n").unwrap();
        let overrides = [json!({"key": "OPENAI_API_KEY", "value": "sk-1", "secret": true})];
        let (body, warnings) = service_body(&i.preset, "server_1", None, &overrides, false).unwrap();
        assert!(warnings.is_empty());
        assert_eq!(body["serverId"], "server_1");
        assert!(body["name"].as_str().unwrap().starts_with("n8n-"));
        assert_eq!(body["deployment"], json!({"url": "docker.io/n8nio/n8n:2.35.7"}));
        assert_eq!(body["network"], json!({"public": true, "protocol": "http"}));
        assert_eq!(body["healthcheck"], "/healthz");
        assert_eq!(body["cmd"], "start");
        let key = body["env"].as_array().unwrap().iter().find(|e| e["key"] == "OPENAI_API_KEY").unwrap();
        assert_eq!(key["value"], "sk-1");
        assert_eq!(key["secret"], true);
        let mount = &body["volumes"][0];
        assert!(mount["name"].as_str().unwrap().starts_with("n8n-data-"));
        assert_eq!(mount["mountPath"], "/home/node/.n8n");
        // The volume carries a name, not an id: the API creates it.
        assert!(mount.get("id").is_none());
    }

    #[test]
    fn a_name_overrides_the_generated_one() {
        let i = catalog().instantiate("postgres").unwrap();
        let (body, _) = service_body(&i.preset, "s", Some("billing-db"), &[], false).unwrap();
        assert_eq!(body["name"], "billing-db");
    }

    #[test]
    fn a_public_database_warns_and_private_removes_the_exposure() {
        let i = catalog().instantiate("postgres").unwrap();
        let (body, warnings) = service_body(&i.preset, "s", None, &[], false).unwrap();
        assert_eq!(body["network"], json!({"public": true, "protocol": "tcp"}));
        assert!(warnings[0].contains("exposed to the internet") && warnings[0].contains("--private"), "{warnings:?}");
        assert!(body.get("healthcheck").is_none(), "only http presets get a health check");

        let (body, warnings) = service_body(&i.preset, "s", None, &[], true).unwrap();
        assert_eq!(body["network"], json!({"public": false}));
        assert!(warnings.is_empty());
    }

    #[test]
    fn an_empty_required_value_is_refused_with_the_flag_to_supply_it() {
        let i = catalog().instantiate("n8n").unwrap();
        let e = service_body(&i.preset, "s", None, &[], false).unwrap_err();
        assert_eq!(e.code, ErrorCode::InvalidInput);
        assert!(e.message.contains("OPENAI_API_KEY"), "{}", e.message);
        assert!(e.remediation.unwrap().contains("--secret-env OPENAI_API_KEY=<value>"));
    }

    #[test]
    fn an_override_cannot_unhide_a_secret_the_preset_marked() {
        let i = catalog().instantiate("n8n").unwrap();
        let overrides = [
            json!({"key": "OPENAI_API_KEY", "value": "x", "secret": false}),
            json!({"key": "N8N_ENCRYPTION_KEY", "value": "mine", "secret": false}),
        ];
        let (body, _) = service_body(&i.preset, "s", None, &overrides, false).unwrap();
        for k in ["OPENAI_API_KEY", "N8N_ENCRYPTION_KEY"] {
            let e = body["env"].as_array().unwrap().iter().find(|e| e["key"] == k).unwrap();
            assert_eq!(e["secret"], true, "{k}");
        }
    }

    #[test]
    fn dry_run_output_hides_generated_and_secret_values() {
        let i = catalog().instantiate("postgres").unwrap();
        let (body, _) = service_body(&i.preset, "s", None, &[], false).unwrap();
        let shown = redact(&body, &i.generated).to_string();
        assert!(!shown.contains(&i.generated[0].value), "{shown}");
        assert!(shown.contains("<hidden>") && shown.contains("mydb"));
    }

    #[test]
    fn describing_a_preset_shows_no_secrets_and_flags_what_is_required() {
        let d = catalog().describe("n8n").unwrap();
        assert_eq!(d["image"], "docker.io/n8nio/n8n:2.35.7");
        assert_eq!(d["minMemoryMb"], 2048);
        let env = d["env"].as_array().unwrap();
        assert_eq!(env[0]["value"], "<generated>");
        assert_eq!(env.iter().find(|e| e["key"] == "OPENAI_API_KEY").unwrap()["required"], true);
        assert!(!d.to_string().contains(MARK), "placeholders must not leak into the output");
        assert_eq!(d["volumes"][0]["name"], "n8n-data-<random>");
    }

    #[test]
    fn listing_filters_by_text_and_tag() {
        let c = catalog();
        assert_eq!(c.list(None, None).len(), 2);
        assert_eq!(c.list(Some("RELATIONAL"), None)[0]["id"], "postgres");
        assert_eq!(c.list(Some("autom"), None)[0]["id"], "n8n");
        assert_eq!(c.list(None, Some("Database"))[0]["id"], "postgres");
        assert!(c.list(Some("nope"), None).is_empty());
        assert!(c.list(None, Some("data")).is_empty(), "a tag filter is an exact match");
    }

    #[test]
    fn an_unknown_preset_points_at_the_list() {
        let e = catalog().instantiate("nope").map(|_| ()).unwrap_err();
        assert_eq!(e.code, ErrorCode::NotFound);
        assert_eq!(e.remediation.unwrap(), "sliplane presets list");
    }

    #[test]
    fn a_catalog_that_drifts_is_refused_not_guessed_at() {
        let bad = |edit: &dyn Fn(&mut Value)| {
            let mut v: Value = serde_json::from_str(CATALOG).unwrap();
            edit(&mut v);
            parse(&v.to_string()).map(|_| ()).unwrap_err().message
        };
        assert!(bad(&|v| v["presets"] = json!([])).contains("lists no presets"));
        assert!(
            bad(&|v| v["generatedValues"][1]["placeholder"] = json!("__SLIPLANE_GENERATED_7__"))
                .contains("out of order")
        );
        assert!(bad(&|v| v["generatedValues"][0]["kind"] = json!("base64")).contains("does not know"));
        assert!(bad(&|v| v["generatedValues"][0]["length"] = json!(0)).contains("does not know"));
        assert!(bad(&|v| v["generatedValues"][2]["length"] = json!(12)).contains("does not know"));
        assert!(bad(&|v| v["presets"][1]["settings"]["presetId"] = json!("postgres")).contains("twice"));
        assert!(bad(&|v| v["presets"][0]["settings"]["presetId"] = json!("")).contains("without an id"));
        assert!(parse("<html>").map(|_| ()).unwrap_err().message.contains("is not JSON"));
    }

    #[test]
    fn a_placeholder_nobody_defined_is_refused() {
        let mut v: Value = serde_json::from_str(CATALOG).unwrap();
        v["presets"][0]["settings"]["envs"][1]["value"] = json!("__SLIPLANE_GENERATED_9__");
        let e = parse(&v.to_string()).unwrap().instantiate("postgres").map(|_| ()).unwrap_err();
        assert!(e.message.contains("never defines"), "{}", e.message);
    }

    #[test]
    fn only_registry_images_with_a_known_protocol_are_deployable() {
        let mut v: Value = serde_json::from_str(CATALOG).unwrap();
        v["presets"][0]["settings"]["deploySource"] = json!("repository");
        v["presets"][1]["settings"]["protocol"] = json!("sctp");
        let c = parse(&v.to_string()).unwrap();
        for id in ["postgres", "n8n"] {
            let i = c.instantiate(id).unwrap();
            let overrides = [json!({"key": "OPENAI_API_KEY", "value": "x", "secret": true})];
            assert!(service_body(&i.preset, "s", None, &overrides, false).is_err(), "{id}");
        }
    }
}
