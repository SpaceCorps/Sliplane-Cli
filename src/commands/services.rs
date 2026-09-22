//! `services *`. The two commands with real logic are `create` (volume names resolve to
//! existing volumes) and `update` (the API replaces whole objects, so this carries the current
//! state over and refuses updates that would silently drop something).

use std::collections::{HashMap, HashSet};

use serde_json::{Map, Value, json};

use crate::cli::{Deploy, ServiceRef, Services};
use crate::client::{Client, query, seg};
use crate::commands::{client, done, print};
use crate::error::{Error, Result};
use crate::{obj, patharg};

pub fn run(c: Services) -> Result<()> {
    match c {
        Services::List { project_id, a } => list(&client(&a)?, project_id.as_deref()),
        Services::Get(s) => print(client(&s.a)?.get(&path(&s, ""))?),
        Services::Create { project_id, name, server_id, d, public, protocol, env, secret_env, volumes, a } => {
            let client = client(&a)?;
            let body = create_body(&client, name, &server_id, &d, public, protocol, &env, &secret_env, &volumes)?;
            print(client.post(&format!("projects/{}/services", seg(&project_id)), &body)?)
        }
        Services::Update { s, name, d, env, secret_env, replace_env, merge_env } => {
            update(&s, name, &d, &env, &secret_env, replace_env, merge_env)
        }
        Services::Delete(s) => {
            let r = client(&s.a)?.delete(&path(&s, ""))?;
            done(r, "deleted", ids(&s))
        }
        Services::Pause(s) => {
            let r = client(&s.a)?.post_empty(&path(&s, "/pause"))?;
            done(r, "accepted", with(ids(&s), "action", "pause"))
        }
        Services::Unpause(s) => {
            let r = client(&s.a)?.post_empty(&path(&s, "/unpause"))?;
            done(r, "accepted", with(ids(&s), "action", "unpause"))
        }
        Services::Deploy { s, tag } => {
            // An absent body means no Content-Type either, and the deploy endpoint rejects that
            // with "Invalid request body". An empty object is accepted.
            let body = match tag.as_deref().filter(|t| !t.is_empty()) {
                Some(t) => json!({ "tag": t }),
                None => json!({}),
            };
            let r = client(&s.a)?.post(&path(&s, "/deploy"), &body)?;
            let mut fields = with(ids(&s), "action", "deploy");
            if let Some(t) = tag {
                fields["tag"] = json!(t);
            }
            done(r, "accepted", fields)
        }
        Services::Logs { s, from, to } => {
            let q = query(&[("from", from.map(|v| v.to_string())), ("to", to.map(|v| v.to_string()))]);
            print(client(&s.a)?.get(&path(&s, &format!("/logs{q}")))?)
        }
        Services::Metrics { s, r } => {
            let q = query(&[
                ("range", Some(r.range).filter(|v| !v.is_empty())),
                ("from", r.from.map(|v| v.to_string())),
                ("to", r.to.map(|v| v.to_string())),
            ]);
            print(client(&s.a)?.get(&path(&s, &format!("/metrics{q}")))?)
        }
        Services::Events(s) => print(client(&s.a)?.get(&path(&s, "/events"))?),
        Services::ListEnv(s) => print(client(&s.a)?.get(&path(&s, "/env"))?),
        Services::SetEnv { s, key, value, secret } => {
            let body = json!({ "value": value, "secret": secret });
            print(client(&s.a)?.put(&path(&s, &format!("/env/{}", seg(&key))), &body)?)
        }
        Services::DeleteEnv { s, key } => {
            let r = client(&s.a)?.delete(&path(&s, &format!("/env/{}", seg(&key))))?;
            done(r, "deleted", with(ids(&s), "key", &key))
        }
        Services::AddDomain { s, domain } => {
            print(client(&s.a)?.post(&path(&s, "/domains"), &json!({ "domain": domain }))?)
        }
        Services::RemoveDomain { s, domain_id } => {
            let r = client(&s.a)?.delete(&path(&s, &format!("/domains/{}", seg(&domain_id))))?;
            done(r, "deleted", with(ids(&s), "domainId", &domain_id))
        }
    }
}

fn path(s: &ServiceRef, tail: &str) -> String {
    format!("projects/{}/services/{}{tail}", seg(&s.project_id), seg(&s.service_id))
}

fn ids(s: &ServiceRef) -> Value {
    obj! { "projectId" => s.project_id, "serviceId" => s.service_id }
}

fn with(mut v: Value, k: &str, val: &str) -> Value {
    v[k] = json!(val);
    v
}

/// Without a project, walk every project - one request per project, run side by side.
fn list(client: &Client, project_id: Option<&str>) -> Result<()> {
    if let Some(p) = project_id {
        return print(client.get(&format!("projects/{}/services", seg(p)))?);
    }

    let projects = client.get("projects")?;
    let ids: Vec<String> = projects
        .as_array()
        .map(|a| a.iter().filter_map(|p| p.get("id").and_then(Value::as_str).map(str::to_string)).collect())
        .unwrap_or_default();

    let results: Vec<Result<Value>> = std::thread::scope(|scope| {
        let handles: Vec<_> =
            ids.iter().map(|id| scope.spawn(move || client.get(&format!("projects/{}/services", seg(id))))).collect();
        handles.into_iter().map(|h| h.join().expect("request thread panicked")).collect()
    });

    let mut all = Vec::new();
    for r in results {
        if let Value::Array(items) = r? {
            all.extend(items);
        }
    }
    print(Value::Array(all))
}

pub(crate) fn parse_env(env: &[String], secret_env: &[String]) -> Result<Vec<Value>> {
    let parse = |e: &String, secret: bool| -> Result<Value> {
        let (k, v) = e.split_once('=').unwrap_or((e.as_str(), ""));
        if k.is_empty() {
            return Err(Error::invalid(format!("'{e}' has no key. Environment variables are KEY=VALUE.")));
        }
        Ok(obj! { "key" => k, "value" => v, "secret" => secret })
    };
    env.iter().map(|e| parse(e, false)).chain(secret_env.iter().map(|e| parse(e, true))).collect()
}

fn nonempty(s: &Option<String>) -> Option<&str> {
    s.as_deref().filter(|v| !v.is_empty())
}

fn build_flags(d: &Deploy) -> bool {
    nonempty(&d.branch).is_some() || nonempty(&d.dockerfile_path).is_some() || nonempty(&d.docker_context).is_some()
}

fn repo_only_flags(d: &Deploy) -> bool {
    build_flags(d) || d.auto_deploy.is_some() || d.include_paths.is_some() || d.ignore_paths.is_some()
}

/// Applies the repository-build flags the caller passed.
fn overlay_repo(dep: &mut Map<String, Value>, d: &Deploy) {
    if let Some(v) = nonempty(&d.branch) {
        dep.insert("branch".into(), json!(v));
    }
    if let Some(v) = nonempty(&d.dockerfile_path) {
        dep.insert("dockerfilePath".into(), json!(v));
    }
    if let Some(v) = nonempty(&d.docker_context) {
        dep.insert("dockerContext".into(), json!(v));
    }
    if let Some(v) = d.auto_deploy {
        dep.insert("autoDeploy".into(), json!(v));
    }
    if let Some(v) = &d.include_paths {
        dep.insert("includePaths".into(), json!(v));
    }
    if let Some(v) = &d.ignore_paths {
        dep.insert("ignorePaths".into(), json!(v));
    }
}

const IMAGE_FLAGS_ERROR: &str = "--branch, --dockerfile, --docker-context, --auto-deploy and the deploy path filters \
                                 only apply to repository builds, not to --image.";

#[allow(clippy::too_many_arguments)]
fn create_body(
    client: &Client,
    name: String,
    server_id: &str,
    d: &Deploy,
    public: bool,
    protocol: Option<String>,
    env: &[String],
    secret_env: &[String],
    volumes: &[String],
) -> Result<Value> {
    let mut body = obj! { "name" => name, "serverId" => server_id };

    let mut dep = Map::new();
    match (nonempty(&d.image), nonempty(&d.repo)) {
        (Some(_), Some(_)) => return Err(Error::invalid("Pass --image or --repo, not both.")),
        (None, None) => {
            return Err(Error::invalid(
                "A service needs a deployment: pass --image <url> for a registry image, or --repo <url> for a repository build.",
            ));
        }
        (Some(image), None) => {
            if repo_only_flags(d) {
                return Err(Error::invalid(IMAGE_FLAGS_ERROR));
            }
            dep.insert("url".into(), json!(image));
            if let Some(c) = nonempty(&d.registry_credential_id) {
                dep.insert("registryAuthenticationId".into(), json!(c));
            }
        }
        (None, Some(repo)) => {
            if nonempty(&d.registry_credential_id).is_some() {
                return Err(Error::invalid("--registry-credential-id only applies to --image."));
            }
            dep.insert("url".into(), json!(repo));
            overlay_repo(&mut dep, d);
        }
    }
    body["deployment"] = Value::Object(dep);

    // The API applies protocol only to a public service; saying so beats silently dropping it.
    if protocol.is_some() && !public {
        return Err(Error::invalid("--protocol only applies to a public service. Add --public."));
    }
    let mut network = obj! { "public" => public };
    if let Some(p) = protocol.filter(|p| !p.is_empty()) {
        network["protocol"] = json!(p);
    }
    body["network"] = network;

    if let Some(h) = nonempty(&d.healthcheck) {
        body["healthcheck"] = json!(patharg::check(h, "--healthcheck")?);
    }
    if let Some(c) = nonempty(&d.cmd) {
        body["cmd"] = json!(c);
    }

    let env = parse_env(env, secret_env)?;
    if !env.is_empty() {
        body["env"] = Value::Array(env);
    }

    if !volumes.is_empty() {
        body["volumes"] = Value::Array(resolve_volumes(client, server_id, volumes)?);
    }
    Ok(body)
}

/// Sending a name creates a volume - every time, even when one of that name already exists.
/// Two services asked for "app-data" end up on two different volumes with the same name, which
/// is invisible until one of them is missing its data. So resolve the name to an existing id
/// first and only fall back to creating when there genuinely is none.
fn resolve_volumes(client: &Client, server_id: &str, volumes: &[String]) -> Result<Vec<Value>> {
    let mut parsed = Vec::with_capacity(volumes.len());
    for v in volumes {
        let Some((vol, mount)) = v.split_once(':').filter(|(a, b)| !a.is_empty() && !b.is_empty()) else {
            return Err(Error::invalid(format!("--volume '{v}' should be id:/path or name:/path.")));
        };
        if !mount.starts_with('/') {
            return Err(Error::invalid(format!(
                "--volume '{v}' has mount path '{mount}', which is not an absolute path. \
                 On Windows, run from PowerShell or cmd rather than Git Bash."
            )));
        }
        parsed.push((vol, mount));
    }

    let mut by_name: HashMap<String, String> = HashMap::new();
    if parsed.iter().any(|(v, _)| !v.starts_with("volume_"))
        && let Value::Array(existing) = client.get(&format!("servers/{}/volumes", seg(server_id)))?
    {
        for e in existing {
            if let (Some(n), Some(i)) = (e.get("name").and_then(Value::as_str), e.get("id").and_then(Value::as_str)) {
                // First wins, so a re-run keeps using the same volume.
                by_name.entry(n.to_string()).or_insert_with(|| i.to_string());
            }
        }
    }

    Ok(parsed
        .into_iter()
        .map(|(vol, mount)| {
            if vol.starts_with("volume_") {
                obj! { "id" => vol, "mountPath" => mount }
            } else if let Some(id) = by_name.get(vol) {
                obj! { "id" => id, "mountPath" => mount }
            } else {
                obj! { "name" => vol, "mountPath" => mount }
            }
        })
        .collect())
}

/// Fetches the service once, and only if something needs it.
struct Current<'a> {
    client: &'a Client,
    path: String,
    value: Option<Value>,
}

impl Current<'_> {
    fn get(&mut self) -> Result<&Value> {
        if self.value.is_none() {
            self.value = Some(self.client.get(&self.path)?);
        }
        Ok(self.value.as_ref().expect("just set"))
    }
}

const REPO_FIELDS: &[&str] =
    &["url", "branch", "dockerfilePath", "dockerContext", "autoDeploy", "includePaths", "ignorePaths"];
const IMAGE_FIELDS: &[&str] = &["url", "registryAuthenticationId"];

fn is_repository_build(dep: &Value) -> bool {
    ["branch", "dockerfilePath", "dockerContext", "autoDeploy"]
        .iter()
        .any(|k| dep.get(*k).is_some_and(|v| !v.is_null()))
}

fn pick(dep: &Value, fields: &[&str]) -> Map<String, Value> {
    fields.iter().filter_map(|f| dep.get(*f).filter(|v| !v.is_null()).map(|v| (f.to_string(), v.clone()))).collect()
}

/// The registry host of an image reference; `nginx` and `library/nginx` are Docker Hub.
fn registry_host(image: &str) -> &str {
    match image.split_once('/') {
        Some((first, _)) if first.contains('.') || first.contains(':') || first == "localhost" => first,
        _ => "docker.io",
    }
}

fn update(
    s: &ServiceRef,
    name: Option<String>,
    d: &Deploy,
    env: &[String],
    secret_env: &[String],
    replace_env: bool,
    merge_env: bool,
) -> Result<()> {
    let client = client(&s.a)?;
    let mut current = Current { client: &client, path: path(s, ""), value: None };
    let mut body = obj! {};

    if let Some(n) = nonempty(&name) {
        body["name"] = json!(n);
    }
    if let Some(h) = nonempty(&d.healthcheck) {
        body["healthcheck"] = json!(patharg::check(h, "--healthcheck")?);
    }
    if let Some(c) = nonempty(&d.cmd) {
        body["cmd"] = json!(c);
    }

    // The API wants a deployment object on every PATCH and takes it whole, so any update has to
    // carry the current one over - not just the deploy-related flags. The spec defaults every
    // field it leaves out (`branch: main`, `dockerfilePath: Dockerfile`), so sending only the url
    // risks moving a service on `develop` to `main` and drops a private image's credentials.
    let mut dep = Map::new();
    if let Some(image) = nonempty(&d.image) {
        if nonempty(&d.repo).is_some() {
            return Err(Error::invalid("Pass --image or --repo, not both."));
        }
        if repo_only_flags(d) {
            return Err(Error::invalid(IMAGE_FLAGS_ERROR));
        }
        dep.insert("url".into(), json!(image));
        match nonempty(&d.registry_credential_id) {
            Some(c) => {
                dep.insert("registryAuthenticationId".into(), json!(c));
            }
            None => {
                // Keep the credentials when the new image lives in the same registry.
                let old = current.get()?.get("deployment").cloned().unwrap_or(Value::Null);
                let old_url = old.get("url").and_then(Value::as_str).unwrap_or("");
                if let Some(c) = old.get("registryAuthenticationId").filter(|v| v.is_string())
                    && registry_host(old_url) == registry_host(image)
                {
                    dep.insert("registryAuthenticationId".into(), c.clone());
                }
            }
        }
    } else if let Some(repo) = nonempty(&d.repo) {
        if nonempty(&d.registry_credential_id).is_some() {
            return Err(Error::invalid("--registry-credential-id only applies to registry images."));
        }
        dep.insert("url".into(), json!(repo));
        overlay_repo(&mut dep, d);
    } else {
        let old = current.get()?.get("deployment").cloned().filter(Value::is_object).ok_or_else(|| {
            Error::other("The service has no deployment to carry over.").detail("Pass --image or --repo explicitly.")
        })?;
        if is_repository_build(&old) {
            if nonempty(&d.registry_credential_id).is_some() {
                return Err(Error::invalid(
                    "This service is a repository build, so --registry-credential-id does not apply.",
                ));
            }
            dep = pick(&old, REPO_FIELDS);
            overlay_repo(&mut dep, d);
        } else {
            // These flags used to be dropped on an image service, and `services update --branch
            // main` returned the unchanged service with exit 0.
            if repo_only_flags(d) {
                return Err(Error::invalid(
                    "This service runs a registry image, so --branch, --dockerfile, --docker-context, --auto-deploy \
                     and the deploy path filters do not apply. A service cannot move between an image and a \
                     repository build; delete and recreate it instead.",
                ));
            }
            dep = pick(&old, IMAGE_FIELDS);
            if let Some(c) = nonempty(&d.registry_credential_id) {
                dep.insert("registryAuthenticationId".into(), json!(c));
            }
        }
    }
    body["deployment"] = Value::Object(dep);

    let supplied = parse_env(env, secret_env)?;
    if supplied.is_empty() && (replace_env || merge_env) {
        return Err(Error::invalid("--replace-env and --merge-env need at least one --env or --secret-env."));
    }
    if !supplied.is_empty() {
        body["env"] = Value::Array(env_for_update(&mut current, supplied, replace_env, merge_env)?);
    }

    let result = client.patch(&path(s, ""), &body)?;
    ensure_applied(&result, d)?;
    print(result)
}

/// The API replaces the whole env array rather than merging, so an update that lists two
/// variables on a service holding three deletes the third - silently, and the service only fails
/// later when it cannot find it.
///
/// Default: refuse, and name what would be lost. `--replace-env`: send exactly what was given.
/// `--merge-env`: carry every unlisted variable over. Secrets read back masked, so they are sent
/// with an empty value, which the API documents as "keep the stored value".
fn env_for_update(current: &mut Current, supplied: Vec<Value>, replace: bool, merge: bool) -> Result<Vec<Value>> {
    if replace {
        return Ok(supplied);
    }

    let existing: Vec<Value> = current.get()?.get("env").and_then(Value::as_array).cloned().unwrap_or_default();
    let keys: HashSet<&str> = supplied.iter().filter_map(|v| v["key"].as_str()).collect();
    let dropped: Vec<&Value> =
        existing.iter().filter(|e| e.get("key").and_then(Value::as_str).is_some_and(|k| !keys.contains(k))).collect();

    if merge {
        let mut out: Vec<Value> = dropped
            .into_iter()
            .map(|e| {
                let secret = e.get("secret").and_then(Value::as_bool).unwrap_or(false);
                let value = if secret { "" } else { e.get("value").and_then(Value::as_str).unwrap_or("") };
                obj! { "key" => e["key"], "value" => value, "secret" => secret }
            })
            .collect();
        out.extend(supplied);
        return Ok(out);
    }

    if !dropped.is_empty() {
        let names: Vec<&str> = dropped.iter().filter_map(|e| e["key"].as_str()).collect();
        return Err(Error::invalid(format!(
            "This would delete {} environment variable(s) not listed: {}. --env replaces the whole environment.",
            names.len(),
            names.join(", ")
        ))
        .fix("Keep the others with --merge-env, change one variable with `services set-env`, or pass --replace-env if deleting them is intended."));
    }
    Ok(supplied)
}

/// A 200 has come back before with the deployment unchanged, so compare what was asked for
/// with the service the API returns instead of trusting the status.
fn ensure_applied(result: &Value, d: &Deploy) -> Result<()> {
    let Some(actual) = result.get("deployment") else { return Ok(()) };
    for (field, wanted) in [
        ("branch", nonempty(&d.branch)),
        ("dockerfilePath", nonempty(&d.dockerfile_path)),
        ("dockerContext", nonempty(&d.docker_context)),
    ] {
        let Some(wanted) = wanted else { continue };
        let got = actual.get(field).and_then(Value::as_str);
        if got != Some(wanted) {
            // invalid_input, as in the .NET version: exit codes are a contract with callers.
            return Err(Error::invalid(format!(
                "Sliplane accepted the update but the service still has {field} '{}' instead of '{wanted}'.",
                got.unwrap_or("")
            ))
            .fix("Check it with `sliplane services get`."));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_parsing() {
        let v = parse_env(&["A=1".into(), "B=x=y".into(), "C".into()], &["S=s".into()]).unwrap();
        assert_eq!(v[0], json!({"key": "A", "value": "1", "secret": false}));
        assert_eq!(v[1]["value"], "x=y");
        assert_eq!(v[2]["value"], "");
        assert_eq!(v[3]["secret"], true);
        assert!(parse_env(&["=v".into()], &[]).is_err());
    }

    #[test]
    fn registry_hosts() {
        assert_eq!(registry_host("nginx:latest"), "docker.io");
        assert_eq!(registry_host("library/nginx"), "docker.io");
        assert_eq!(registry_host("docker.io/library/nginx"), "docker.io");
        assert_eq!(registry_host("ghcr.io/org/app:1"), "ghcr.io");
        assert_eq!(registry_host("localhost:5000/app"), "localhost:5000");
    }

    #[test]
    fn repository_detection() {
        assert!(is_repository_build(&json!({"url": "https://github.com/a/b", "branch": "main"})));
        assert!(!is_repository_build(&json!({"url": "nginx", "registryAuthenticationId": "c"})));
    }

    #[test]
    fn carry_over_keeps_branch() {
        let old = json!({"url": "https://github.com/a/b", "branch": "develop", "autoDeploy": false, "extra": 1});
        let picked = pick(&old, REPO_FIELDS);
        assert_eq!(picked.get("branch"), Some(&json!("develop")));
        assert_eq!(picked.get("autoDeploy"), Some(&json!(false)));
        assert!(!picked.contains_key("extra"));
    }
}
