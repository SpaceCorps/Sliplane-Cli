//! Dispatch. Most commands are one request and a print; the ones with real logic live in their
//! own modules (`accounts`, `services`).

mod accounts;
mod services;

use serde_json::{Value, json};

use crate::account::{self, Resolved};
use crate::cli::*;
use crate::client::{Client, query, seg};
use crate::error::{Error, Result};
use crate::{obj, output};

pub fn run(command: Command) -> Result<()> {
    match command {
        Command::Me(a) => print(client(&a)?.get("me")?),
        Command::AgentReadme => {
            crate::readme::print();
            Ok(())
        }
        Command::Accounts(c) => accounts::run(c),
        Command::Projects(c) => projects(c),
        Command::Servers(c) => servers(c),
        Command::Services(c) => services::run(c),
        Command::Postgres(c) => postgres(c),
        Command::Buckets(c) => buckets(c),
        Command::Credentials(c) => credentials(c),
        Command::Oauth(c) => oauth(c),
    }
}

pub(crate) fn resolve(a: &Account) -> Result<Resolved> {
    account::resolve(a.account.as_deref())
}

pub(crate) fn client(a: &Account) -> Result<Client> {
    Ok(resolve(a)?.client())
}

pub(crate) fn print(v: Value) -> Result<()> {
    output::write(&v);
    Ok(())
}

/// For endpoints that answer 202/204 with no body: say what happened instead of printing `{}`,
/// and keep stdout parseable under --json.
pub(crate) fn done(v: Value, status: &str, fields: Value) -> Result<()> {
    let empty = match &v {
        Value::Object(m) => m.is_empty(),
        Value::Null => true,
        Value::String(s) => s.trim().is_empty(),
        _ => false,
    };
    if !empty {
        return print(v);
    }
    let mut out = obj! { "status" => status };
    if let (Value::Object(o), Value::Object(f)) = (&mut out, fields) {
        o.extend(f);
    }
    print(out)
}

fn projects(c: Projects) -> Result<()> {
    match c {
        Projects::List(a) => print(client(&a)?.get("projects")?),
        Projects::Create { name, a } => print(client(&a)?.post("projects", &json!({ "name": name }))?),
        Projects::Update { project_id, name, a } => {
            print(client(&a)?.patch(&format!("projects/{}", seg(&project_id)), &json!({ "name": name }))?)
        }
        Projects::Delete { project_id, a } => {
            let r = client(&a)?.delete(&format!("projects/{}", seg(&project_id)))?;
            done(r, "deleted", obj! { "projectId" => project_id })
        }
    }
}

fn metrics_query(r: &Range) -> String {
    query(&[
        ("range", Some(r.range.clone()).filter(|s| !s.is_empty())),
        ("from", r.from.map(|v| v.to_string())),
        ("to", r.to.map(|v| v.to_string())),
    ])
}

fn servers(c: Servers) -> Result<()> {
    match c {
        Servers::List(a) => print(client(&a)?.get("servers")?),
        Servers::Get(s) => print(client(&s.a)?.get(&format!("servers/{}", seg(&s.server_id)))?),
        Servers::Create { name, instance_type, location, disk_size_gb, billing_cycle, a } => {
            let mut body = obj! { "name" => name, "instanceType" => instance_type, "location" => location };
            if let Some(d) = disk_size_gb {
                body["diskSizeGb"] = json!(d);
            }
            if let Some(b) = billing_cycle.filter(|s| !s.is_empty()) {
                body["billingCycle"] = json!(b);
            }
            print(client(&a)?.post("servers", &body)?)
        }
        Servers::Delete(s) => {
            let r = client(&s.a)?.delete(&format!("servers/{}", seg(&s.server_id)))?;
            done(r, "deleted", obj! { "serverId" => s.server_id })
        }
        Servers::Rescale { instance_type, billing_cycle, s } => {
            let mut body = obj! { "instanceType" => instance_type };
            if let Some(b) = billing_cycle.filter(|s| !s.is_empty()) {
                body["billingCycle"] = json!(b);
            }
            let r = client(&s.a)?.post(&format!("servers/{}", seg(&s.server_id)), &body)?;
            done(r, "accepted", obj! { "serverId" => s.server_id, "action" => "rescale" })
        }
        Servers::RescaleDisk { disk_size_gb, s } => {
            let r = client(&s.a)?
                .post(&format!("servers/{}/disk", seg(&s.server_id)), &json!({ "diskSizeGb": disk_size_gb }))?;
            done(r, "accepted", obj! { "serverId" => s.server_id, "action" => "rescale-disk" })
        }
        Servers::Metrics { s, r } => {
            print(client(&s.a)?.get(&format!("servers/{}/metrics{}", seg(&s.server_id), metrics_query(&r)))?)
        }
        Servers::Volumes(s) => print(client(&s.a)?.get(&format!("servers/{}/volumes", seg(&s.server_id)))?),
        Servers::CreateVolume { name, s } => {
            print(client(&s.a)?.post(&format!("servers/{}/volumes", seg(&s.server_id)), &json!({ "name": name }))?)
        }
    }
}

/// `CIDR` or `CIDR=description` into the API's ipAllowList shape. `--block-public-ips` is the
/// empty list; neither flag leaves the field out.
fn ip_allow_list(ip: &IpAllow) -> Option<Value> {
    if ip.block_public_ips {
        return Some(json!([]));
    }
    ip.ip_allow.as_ref().map(|entries| {
        Value::Array(
            entries
                .iter()
                .map(|e| match e.split_once('=') {
                    Some((src, desc)) => obj! { "source" => src.trim(), "description" => desc },
                    None => obj! { "source" => e.trim() },
                })
                .collect(),
        )
    })
}

fn postgres(c: Postgres) -> Result<()> {
    let at = |p: &PgId, tail: &str| format!("postgres/{}{tail}", seg(&p.postgres_id));
    match c {
        Postgres::List(a) => print(client(&a)?.get("postgres")?),
        Postgres::Get(p) => print(client(&p.a)?.get(&at(&p, ""))?),
        Postgres::Create {
            name,
            instance_type,
            region,
            pg_version,
            database_name,
            database_user,
            disk_size_gb,
            billing_cycle,
            ip,
            a,
        } => {
            let mut body = obj! { "name" => name, "instanceType" => instance_type, "region" => region };
            if let Some(v) = pg_version {
                body["version"] = json!(v);
            }
            if let Some(v) = database_name.filter(|s| !s.is_empty()) {
                body["databaseName"] = json!(v);
            }
            if let Some(v) = database_user.filter(|s| !s.is_empty()) {
                body["databaseUser"] = json!(v);
            }
            if let Some(v) = disk_size_gb {
                body["diskSizeGb"] = json!(v);
            }
            if let Some(v) = billing_cycle.filter(|s| !s.is_empty()) {
                body["billingCycle"] = json!(v);
            }
            if let Some(v) = ip_allow_list(&ip) {
                body["ipAllowList"] = v;
            }
            print(client(&a)?.post("postgres", &body)?)
        }
        Postgres::Update { p, name, instance_type, disk_size_gb, ip } => {
            let mut body = obj! {};
            if let Some(v) = name.filter(|s| !s.is_empty()) {
                body["name"] = json!(v);
            }
            if let Some(v) = instance_type.filter(|s| !s.is_empty()) {
                body["instanceType"] = json!(v);
            }
            if let Some(v) = disk_size_gb {
                body["diskSizeGb"] = json!(v);
            }
            if let Some(v) = ip_allow_list(&ip) {
                body["ipAllowList"] = v;
            }
            if body.as_object().is_some_and(|m| m.is_empty()) {
                return Err(Error::invalid(
                    "Nothing to update. Pass --name, --instance-type, --disk-size-gb, --ip-allow or --block-public-ips.",
                ));
            }
            print(client(&p.a)?.patch(&at(&p, ""), &body)?)
        }
        Postgres::Delete(p) => {
            let r = client(&p.a)?.delete(&at(&p, ""))?;
            done(r, "deleted", obj! { "postgresId" => p.postgres_id })
        }
        Postgres::Pause(p) => {
            let r = client(&p.a)?.post_empty(&at(&p, "/pause"))?;
            done(r, "accepted", obj! { "postgresId" => p.postgres_id, "action" => "pause" })
        }
        Postgres::Unpause(p) => {
            let r = client(&p.a)?.post_empty(&at(&p, "/unpause"))?;
            done(r, "accepted", obj! { "postgresId" => p.postgres_id, "action" => "unpause" })
        }
        Postgres::Restart(p) => {
            let r = client(&p.a)?.post_empty(&at(&p, "/restart"))?;
            done(r, "accepted", obj! { "postgresId" => p.postgres_id, "action" => "restart" })
        }
        Postgres::RotateCredentials(p) => print(client(&p.a)?.post_empty(&at(&p, "/credentials/rotate"))?),
        Postgres::Logs { p, since, until, limit } => {
            let q = query(&[("since", since), ("until", until), ("limit", limit.map(|l| l.to_string()))]);
            print(client(&p.a)?.get(&at(&p, &format!("/logs{q}")))?)
        }
        Postgres::Connections(p) => print(client(&p.a)?.get(&at(&p, "/active-connections"))?),
        Postgres::RelationSizes(p) => print(client(&p.a)?.get(&at(&p, "/relation-sizes"))?),
        Postgres::SlowQueries(p) => print(client(&p.a)?.get(&at(&p, "/slow-queries"))?),
        Postgres::TopQueries(p) => print(client(&p.a)?.get(&at(&p, "/top-queries"))?),
        Postgres::RestoreWindow(p) => print(client(&p.a)?.get(&at(&p, "/restore"))?),
        Postgres::Restore { p, timestamp } => {
            let r = client(&p.a)?.post(&at(&p, "/restore"), &json!({ "timestamp": timestamp }))?;
            done(r, "accepted", obj! { "postgresId" => p.postgres_id, "action" => "restore", "timestamp" => timestamp })
        }
    }
}

fn buckets(c: Buckets) -> Result<()> {
    let at = |b: &BucketId, tail: &str| format!("buckets/{}{tail}", seg(&b.bucket_id));
    match c {
        Buckets::List(a) => print(client(&a)?.get("buckets")?),
        Buckets::Create { name, region, versioning, object_locking, a } => {
            let body = obj! {
                "name" => name, "region" => region, "versioning" => versioning, "objectLocking" => object_locking
            };
            print(client(&a)?.post("buckets", &body)?)
        }
        Buckets::Update { b, versioning } => {
            print(client(&b.a)?.patch(&at(&b, ""), &json!({ "versioning": versioning }))?)
        }
        Buckets::Delete(b) => {
            let r = client(&b.a)?.delete(&at(&b, ""))?;
            done(r, "scheduled_for_deletion", obj! { "bucketId" => b.bucket_id })
        }
        Buckets::Cors(b) => print(client(&b.a)?.get(&at(&b, "/cors"))?),
        Buckets::SetCors {
            b,
            file,
            allowed_origins,
            allowed_methods,
            allowed_headers,
            expose_headers,
            max_age_seconds,
        } => {
            let body = if let Some(path) = file {
                let text = std::fs::read_to_string(&path)
                    .map_err(|e| Error::invalid(format!("Could not read {path}: {e}")))?;
                let v: Value = serde_json::from_str(&text)
                    .map_err(|e| Error::invalid(format!("{path} is not valid JSON: {e}")))?;
                // Accept either the full config object or a bare array of rules.
                if v.is_array() { json!({ "rules": v }) } else { v }
            } else {
                if allowed_origins.is_empty() || allowed_methods.is_empty() {
                    return Err(Error::invalid(
                        "Provide --file, or at least one --allowed-origin and one --allowed-method.",
                    ));
                }
                let mut rule = obj! {
                    "allowedOrigins" => allowed_origins,
                    "allowedMethods" => allowed_methods.iter().map(|m| m.to_uppercase()).collect::<Vec<_>>(),
                };
                if !allowed_headers.is_empty() {
                    rule["allowedHeaders"] = json!(allowed_headers);
                }
                if !expose_headers.is_empty() {
                    rule["exposeHeaders"] = json!(expose_headers);
                }
                if let Some(m) = max_age_seconds {
                    rule["maxAgeSeconds"] = json!(m);
                }
                json!({ "rules": [rule] })
            };
            print(client(&b.a)?.put(&at(&b, "/cors"), &body)?)
        }
        Buckets::DeleteCors(b) => {
            let r = client(&b.a)?.delete(&at(&b, "/cors"))?;
            done(r, "deleted", obj! { "bucketId" => b.bucket_id, "deleted" => "cors" })
        }
        Buckets::Keys(b) => print(client(&b.a)?.get(&at(&b, "/keys"))?),
        Buckets::CreateKey { b, name } => print(client(&b.a)?.post(&at(&b, "/keys"), &json!({ "name": name }))?),
        Buckets::DeleteKey { b, key_id } => {
            let r = client(&b.a)?.delete(&at(&b, &format!("/keys/{}", seg(&key_id))))?;
            done(r, "deleted", obj! { "bucketId" => b.bucket_id, "keyId" => key_id })
        }
    }
}

fn credentials(c: Credentials) -> Result<()> {
    let at = |c: &CredId| format!("registry-credentials/{}", seg(&c.credential_id));
    match c {
        Credentials::List(a) => print(client(&a)?.get("registry-credentials")?),
        Credentials::Get(c) => print(client(&c.a)?.get(&at(&c))?),
        Credentials::Create { name, kind, username, token, a } => {
            let body = obj! { "name" => name, "type" => kind, "username" => username, "token" => token };
            print(client(&a)?.post("registry-credentials", &body)?)
        }
        Credentials::Update { c, name } => print(client(&c.a)?.patch(&at(&c), &json!({ "name": name }))?),
        Credentials::Delete(c) => {
            let r = client(&c.a)?.delete(&at(&c))?;
            done(r, "deleted", obj! { "credentialId" => c.credential_id })
        }
    }
}

fn oauth(c: OAuth) -> Result<()> {
    let at = |c: &ClientId, tail: &str| format!("oauth-clients/{}{tail}", seg(&c.client_id));
    match c {
        OAuth::List(a) => print(client(&a)?.get("oauth-clients")?),
        OAuth::Get(c) => print(client(&c.a)?.get(&at(&c, ""))?),
        OAuth::Update { c, name, image_url, redirect_uris } => {
            let mut body = obj! {};
            if let Some(v) = name.filter(|s| !s.is_empty()) {
                body["name"] = json!(v);
            }
            if let Some(v) = image_url {
                body["imageUrl"] = json!(v);
            }
            if let Some(v) = redirect_uris {
                body["redirectUris"] = json!(v);
            }
            if body.as_object().is_some_and(|m| m.is_empty()) {
                return Err(Error::invalid("Nothing to update. Pass --name, --image-url or --redirect-uri."));
            }
            print(client(&c.a)?.patch(&at(&c, ""), &body)?)
        }
        OAuth::Users(c) => print(client(&c.a)?.get(&at(&c, "/users"))?),
    }
}
