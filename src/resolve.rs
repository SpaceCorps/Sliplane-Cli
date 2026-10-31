//! Names for ids: `--service web` as well as `--service-id service_abc`.
//!
//! The API only speaks ids, so a name costs a listing. That is paid only when a name is used (or an
//! id is missing and has to be found); a command given ids makes exactly the requests it always did.
//!
//! A wanted value is matched against ids first, then against exact, case-sensitive names. A name that
//! two things share is an error that lists both ids, never a guess: an agent that deletes the wrong
//! one of two `web`s has no way to notice.

use serde_json::Value;

use crate::cli::{BucketId, PgId, ServerId, ServiceRef};
use crate::client::{Client, seg};
use crate::error::{Error, ErrorCode, Result};

#[derive(Clone, Copy)]
pub enum Kind {
    Project,
    Server,
    Postgres,
    Bucket,
}

impl Kind {
    fn list(self) -> &'static str {
        match self {
            Kind::Project => "projects",
            Kind::Server => "servers",
            Kind::Postgres => "postgres",
            Kind::Bucket => "buckets",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Kind::Project => "project",
            Kind::Server => "server",
            Kind::Postgres => "Postgres database",
            Kind::Bucket => "bucket",
        }
    }

    /// The name of the flags: `--project` and `--project-id`.
    fn flag(self) -> &'static str {
        match self {
            Kind::Project => "project",
            Kind::Server => "server",
            Kind::Postgres => "postgres",
            Kind::Bucket => "bucket",
        }
    }
}

fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

/// How many known things to list when nothing matched; enough to spot a typo, not a wall of text.
const SHOWN: usize = 20;

/// The item whose id is `wanted`, or else the only one named `wanted`.
fn pick<'a>(label: &str, flag: &str, wanted: &str, items: &'a [Value], listing: &str) -> Result<&'a Value> {
    if let Some(found) = items.iter().find(|i| text(i, "id") == wanted) {
        return Ok(found);
    }
    let named: Vec<&Value> = items.iter().filter(|i| text(i, "name") == wanted).collect();
    match named.as_slice() {
        [one] => Ok(one),
        [] => {
            let mut known: Vec<String> =
                items.iter().take(SHOWN).map(|i| format!("{} ({})", text(i, "name"), text(i, "id"))).collect();
            if items.len() > SHOWN {
                known.push(format!("and {} more", items.len() - SHOWN));
            }
            let detail = if known.is_empty() {
                format!("There are no {label}s.")
            } else {
                format!("Known: {}", known.join(", "))
            };
            Err(Error::new(ErrorCode::NotFound, format!("No {label} has the name or id '{wanted}'."))
                .detail(detail)
                .fix(listing.to_string()))
        }
        many => {
            let ids: Vec<String> = many.iter().map(|i| text(i, "id").to_string()).collect();
            Err(Error::invalid(format!("{} {label}s are named '{wanted}'.", many.len()))
                .detail(format!("Their ids: {}", ids.join(", ")))
                .fix(format!("Pass the one you mean by id with --{flag}-id <id>.")))
        }
    }
}

fn note(flag: &str, wanted: &str, id: &str) {
    // On stderr, so stdout stays the result. It is the only receipt that a name found what was meant.
    eprintln!("note: --{flag} '{wanted}' is {id}");
}

fn unnamed(kind: Kind) -> Error {
    Error::invalid(format!(
        "Name the {label}: --{flag}-id <id> or --{flag} <name>.",
        label = kind.label(),
        flag = kind.flag()
    ))
}

/// Fails, before anything else happens, when a command was given neither an id nor a name.
fn require(kind: Kind, id: &str, name: &Option<String>) -> Result<()> {
    if id.is_empty() && name.is_none() { Err(unnamed(kind)) } else { Ok(()) }
}

/// Fills `id` from `name` (an id or an exact name) with one listing. With no name, `id` must be there already.
pub fn one(client: &Client, kind: Kind, id: &mut String, name: &Option<String>) -> Result<()> {
    let Some(wanted) = name else {
        require(kind, id, name)?;
        return Ok(());
    };
    let items = client.get(kind.list())?;
    let items = items.as_array().map(Vec::as_slice).unwrap_or(&[]);
    let listing = format!("sliplane {} list --account <account>", kind.list());
    *id = text(pick(kind.label(), kind.flag(), wanted, items, &listing)?, "id").to_string();
    note(kind.flag(), wanted, id);
    Ok(())
}

/// Every service in every project, one request per project, side by side.
fn all_services(client: &Client) -> Result<Vec<Value>> {
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
    for (project_id, r) in ids.iter().zip(results) {
        if let Value::Array(items) = r? {
            for mut s in items {
                // The listing is per project; make sure each service says which.
                if text(&s, "projectId").is_empty() {
                    s["projectId"] = Value::String(project_id.clone());
                }
                all.push(s);
            }
        }
    }
    Ok(all)
}

impl ServerId {
    pub fn check(&self) -> Result<()> {
        require(Kind::Server, &self.server_id, &self.server)
    }

    pub fn needs_lookup(&self) -> bool {
        self.server.is_some() || self.server_id.is_empty()
    }

    pub fn resolve(&mut self, client: &Client) -> Result<()> {
        one(client, Kind::Server, &mut self.server_id, &self.server)
    }
}

impl PgId {
    pub fn check(&self) -> Result<()> {
        require(Kind::Postgres, &self.postgres_id, &self.postgres)
    }

    pub fn needs_lookup(&self) -> bool {
        self.postgres.is_some() || self.postgres_id.is_empty()
    }

    pub fn resolve(&mut self, client: &Client) -> Result<()> {
        one(client, Kind::Postgres, &mut self.postgres_id, &self.postgres)
    }
}

impl BucketId {
    pub fn check(&self) -> Result<()> {
        require(Kind::Bucket, &self.bucket_id, &self.bucket)
    }

    pub fn needs_lookup(&self) -> bool {
        self.bucket.is_some() || self.bucket_id.is_empty()
    }

    pub fn resolve(&mut self, client: &Client) -> Result<()> {
        one(client, Kind::Bucket, &mut self.bucket_id, &self.bucket)
    }
}

impl ServiceRef {
    pub fn check(&self) -> Result<()> {
        if self.service_id.is_empty() && self.service.is_none() {
            return Err(Error::invalid("Name the service: --service-id <id> or --service <name>."));
        }
        Ok(())
    }

    /// True when something has to be looked up: a name was given, or the project has to be found.
    pub fn needs_lookup(&self) -> bool {
        self.project.is_some() || self.service.is_some() || self.project_id.is_empty() || self.service_id.is_empty()
    }

    pub fn resolve(&mut self, client: &Client) -> Result<()> {
        self.check()?;
        if self.project.is_some() {
            one(client, Kind::Project, &mut self.project_id, &self.project)?;
        }

        match (&self.service, self.service_id.is_empty()) {
            // An id and a project: nothing to find.
            (None, false) if !self.project_id.is_empty() => Ok(()),
            // An id alone: ask every project which one holds it.
            (None, false) => {
                let wanted = self.service_id.clone();
                let services = all_services(client)?;
                match services.iter().find(|s| text(s, "id") == wanted) {
                    Some(s) => {
                        self.project_id = text(s, "projectId").to_string();
                        note("service-id", &wanted, &format!("in project {}", self.project_id));
                        Ok(())
                    }
                    None => Err(Error::new(
                        ErrorCode::NotFound,
                        format!("No service has the id '{wanted}' in any project."),
                    )
                    .fix("sliplane services list --account <account>")),
                }
            }
            // A name (or an id given as --service): look in the project if there is one, else everywhere.
            (Some(wanted), _) => {
                let services = if self.project_id.is_empty() {
                    all_services(client)?
                } else {
                    match client.get(&format!("projects/{}/services", seg(&self.project_id)))? {
                        Value::Array(a) => a,
                        _ => Vec::new(),
                    }
                };
                let in_project = !self.project_id.is_empty();
                let found = pick("service", "service", wanted, &services, "sliplane services list --account <account>")
                    .map_err(|e| match (&e.code, in_project) {
                        (ErrorCode::InvalidInput, false) => {
                            // Same name in several projects: say which project each is in.
                            let named: Vec<String> = services
                                .iter()
                                .filter(|s| text(s, "name") == wanted.as_str())
                                .map(|s| format!("{} (project {})", text(s, "id"), text(s, "projectId")))
                                .collect();
                            e.detail(format!("They are: {}", named.join(", ")))
                                .fix("Name the project too (--project), or pass --service-id <id>.")
                        }
                        _ => e,
                    })?;
                let id = text(found, "id").to_string();
                if !in_project {
                    self.project_id = text(found, "projectId").to_string();
                }
                self.service_id = id.clone();
                note("service", wanted, &format!("{id} in project {}", self.project_id));
                Ok(())
            }
            (None, true) => unreachable!("rejected above"),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn items() -> Vec<Value> {
        vec![
            json!({"id": "service_1", "name": "web", "projectId": "p1"}),
            json!({"id": "service_2", "name": "worker", "projectId": "p1"}),
            json!({"id": "service_3", "name": "web", "projectId": "p2"}),
        ]
    }

    #[test]
    fn an_id_wins_over_a_name() {
        let mut items = items();
        // A service whose *name* is another's id is a trap; the id must win.
        items.push(json!({"id": "service_9", "name": "service_2", "projectId": "p1"}));
        assert_eq!(text(pick("service", "service", "service_2", &items, "x").unwrap(), "id"), "service_2");
    }

    #[test]
    fn a_unique_name_resolves() {
        assert_eq!(text(pick("service", "service", "worker", &items(), "x").unwrap(), "id"), "service_2");
    }

    #[test]
    fn a_shared_name_is_ambiguous_and_lists_the_ids() {
        let e = pick("service", "service", "web", &items(), "x").unwrap_err();
        assert_eq!(e.code, ErrorCode::InvalidInput);
        assert!(e.message.contains("2 services are named 'web'"), "{}", e.message);
        assert_eq!(e.detail.unwrap(), "Their ids: service_1, service_3");
        assert_eq!(e.remediation.unwrap(), "Pass the one you mean by id with --service-id <id>.");
    }

    #[test]
    fn nothing_matching_is_not_found_and_shows_what_exists() {
        let e = pick("service", "service", "WEB", &items(), "sliplane services list").unwrap_err();
        assert_eq!(e.code, ErrorCode::NotFound, "names are case-sensitive");
        assert!(e.detail.unwrap().contains("worker (service_2)"));
        assert_eq!(e.remediation.unwrap(), "sliplane services list");
        let e = pick("bucket", "bucket", "x", &[], "l").unwrap_err();
        assert_eq!(e.detail.unwrap(), "There are no buckets.");
    }

    #[test]
    fn a_long_list_is_cut_short() {
        let many: Vec<Value> = (0..30).map(|i| json!({"id": format!("id_{i}"), "name": format!("n{i}")})).collect();
        let detail = pick("server", "server", "nope", &many, "l").unwrap_err().detail.unwrap();
        assert!(detail.ends_with("and 10 more"), "{detail}");
    }
}
