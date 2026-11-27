//! `servers autoscaling` and `postgres autoscaling`: read, change and turn off the policy that
//! grows a server or a managed database by itself.
//!
//! The API replaces the policy whole on every write (`cpu`, `memory` and `disk` are required), so
//! `set` reads the current policy, changes what was named and sends all of it back, the way
//! `postgres allowlist` treats its list. A policy the API did not show is never edited blind.
//!
//! The writes go to the endpoints that also rescale (`POST /servers/{id}`, `PATCH /postgres/{id}`).
//! The API wants `autoscaling` sent apart from `instanceType`, `billingCycle` and `diskSizeGb`, so
//! these bodies carry nothing else.
//!
//! `GET /postgres/{id}` does not report the database's compute tier (spec 0.13.0), so for Postgres
//! the "larger than the current tier" check on `maxInstanceType` is left to the API. The disk check
//! is ours for both, from `diskSizeGb`.

use serde_json::{Value, json};

use super::{client, print};
use crate::catalog::{self, POSTGRES_DISK_GB, SERVER_DISK_GB};
use crate::cli::{Account, PgAutoscaling, Policy, ServerAutoscaling};
use crate::client::{Client, seg};
use crate::error::{Error, Result};
use crate::obj;

/// The three rules, as the API names them and as a sentence does.
const RULES: [(&str, &str); 3] = [("cpu", "CPU"), ("memory", "memory"), ("disk", "disk")];

#[derive(Clone, Copy, Debug, PartialEq)]
enum Target {
    Server,
    Postgres,
}

impl Target {
    fn noun(self) -> &'static str {
        match self {
            Target::Server => "server",
            Target::Postgres => "database",
        }
    }

    fn id_key(self) -> &'static str {
        match self {
            Target::Server => "serverId",
            Target::Postgres => "postgresId",
        }
    }

    fn disk_sizes(self) -> &'static [u64] {
        match self {
            Target::Server => SERVER_DISK_GB,
            Target::Postgres => POSTGRES_DISK_GB,
        }
    }

    fn path(self, id: &str) -> String {
        match self {
            Target::Server => format!("servers/{}", seg(id)),
            Target::Postgres => format!("postgres/{}", seg(id)),
        }
    }

    /// The command that names this resource, for `undo` lines.
    fn command(self, id: &str, account: &str) -> String {
        match self {
            Target::Server => format!("sliplane servers autoscaling set --server-id {id} -a {account}"),
            Target::Postgres => format!("sliplane postgres autoscaling set --postgres-id {id} -a {account}"),
        }
    }

    fn write(self, client: &Client, path: &str, body: &Value) -> Result<Value> {
        match self {
            Target::Server => client.post(path, body),
            Target::Postgres => client.patch(path, body),
        }
    }

    fn method(self) -> &'static str {
        match self {
            Target::Server => "POST",
            Target::Postgres => "PATCH",
        }
    }
}

pub fn server(c: ServerAutoscaling) -> Result<()> {
    match c {
        ServerAutoscaling::Get(s) => get(Target::Server, &s.server_id, &s.a),
        ServerAutoscaling::Set { p, s } => set(Target::Server, &s.server_id, &s.a, &p),
        ServerAutoscaling::Clear { dry_run, s } => clear(Target::Server, &s.server_id, &s.a, dry_run),
    }
}

pub fn postgres(c: PgAutoscaling) -> Result<()> {
    match c {
        PgAutoscaling::Get(d) => get(Target::Postgres, &d.postgres_id, &d.a),
        PgAutoscaling::Set { p, d } => set(Target::Postgres, &d.postgres_id, &d.a, &p),
        PgAutoscaling::Clear { dry_run, d } => clear(Target::Postgres, &d.postgres_id, &d.a, dry_run),
    }
}

/// Refuses a `set` that names nothing, before any account or network use.
pub fn named(p: &Policy) -> Result<()> {
    let any = p.cpu_threshold.is_some()
        || p.cpu_minutes.is_some()
        || p.no_cpu
        || p.memory_threshold.is_some()
        || p.memory_minutes.is_some()
        || p.no_memory
        || p.disk_threshold.is_some()
        || p.disk_minutes.is_some()
        || p.no_disk
        || p.max_instance_type.is_some()
        || p.max_disk_gb.is_some();
    if any {
        return Ok(());
    }
    Err(Error::invalid(
        "Nothing to change. Name a rule (--cpu-threshold/--cpu-minutes, --memory-..., --disk-..., or --no-cpu, \
         --no-memory, --no-disk) or a limit (--max-instance-type, --max-disk-gb).",
    ))
}

fn get(t: Target, id: &str, a: &Account) -> Result<()> {
    let v = client(a)?.get(&t.path(id))?;
    print(summary(t, id, &v))
}

fn set(t: Target, id: &str, a: &Account, p: &Policy) -> Result<()> {
    let client = client(a)?;
    let path = t.path(id);
    let v = client.get(&path)?;
    let before = current_policy(t, &v)?;
    let policy = merge(before, p)?;
    check(t, &policy, p, &Current::of(&v))?;
    let body = json!({ "autoscaling": policy });
    if p.dry_run {
        return print(dry_run(t, id, before, &path, body));
    }
    let r = t.write(&client, &path, &body)?;
    print(written(t, id, "set", before, &body["autoscaling"], &r))
}

fn clear(t: Target, id: &str, a: &Account, dry_run_only: bool) -> Result<()> {
    let client = client(a)?;
    let path = t.path(id);
    let v = client.get(&path)?;
    let before = current_policy(t, &v)?;
    if !RULES.iter().any(|(k, _)| before[*k].is_object()) {
        return print(obj! {
            "status" => "unchanged",
            t.id_key() => id,
            "note" => "Autoscaling is already off: every rule is null.",
            "policy" => before,
        });
    }
    let body = json!({ "autoscaling": { "cpu": null, "memory": null, "disk": null } });
    if dry_run_only {
        return print(dry_run(t, id, before, &path, body));
    }
    let r = t.write(&client, &path, &body)?;
    let mut out = written(t, id, "clear", before, &body["autoscaling"], &r);
    out["undo"] = json!(undo(t, id, a.account.as_deref().unwrap_or_default(), before));
    print(out)
}

/// What a resource is now: the sizes a maximum has to exceed. The tier is `None` for Postgres,
/// whose GET does not report one (read anyway, in case a later API version adds it).
#[derive(Debug, Default)]
struct Current {
    instance_type: Option<String>,
    disk_gb: Option<u64>,
}

impl Current {
    fn of(v: &Value) -> Current {
        Current {
            instance_type: v.get("instanceType").and_then(Value::as_str).map(str::to_string),
            disk_gb: v.get("diskSizeGb").and_then(Value::as_u64),
        }
    }
}

fn current_policy(t: Target, v: &Value) -> Result<&Value> {
    v.get("autoscaling").and_then(|a| a.get("policy")).filter(|p| p.is_object()).ok_or_else(|| {
        Error::other(format!(
            "The API returned no autoscaling policy for this {}, so it cannot be changed without guessing the rest of it.",
            t.noun()
        ))
        .detail("GET answered without autoscaling.policy; this CLI was checked against API spec 0.13.0")
    })
}

/// The current policy with the named flags applied. Rules and limits not named are sent back as
/// they were read; an empty `maxInstanceType` or a zero `maxDiskGb` (how the API reports "unset")
/// is left out.
fn merge(current: &Value, p: &Policy) -> Result<Value> {
    let flags = [
        ("cpu", "CPU", p.cpu_threshold, p.cpu_minutes, p.no_cpu),
        ("memory", "memory", p.memory_threshold, p.memory_minutes, p.no_memory),
        ("disk", "disk", p.disk_threshold, p.disk_minutes, p.no_disk),
    ];
    let mut out = obj! {};
    for (key, label, threshold, minutes, off) in flags {
        let now = current.get(key).filter(|r| r.is_object());
        let rule = if off {
            Value::Null
        } else if threshold.is_some() || minutes.is_some() {
            let field = |name: &str| now.and_then(|r| r.get(name)).and_then(Value::as_u64);
            let threshold = threshold.map(u64::from).or_else(|| field("thresholdPercent"));
            let minutes = minutes.map(u64::from).or_else(|| field("durationMinutes"));
            let (Some(threshold), Some(minutes)) = (threshold, minutes) else {
                return Err(Error::invalid(format!(
                    "The {label} rule is off, so turning it on takes both --{key}-threshold and --{key}-minutes."
                )));
            };
            // Keep any field the API returned that this CLI does not know about.
            let mut rule = now.cloned().unwrap_or_else(|| obj! {});
            rule["thresholdPercent"] = json!(threshold);
            rule["durationMinutes"] = json!(minutes);
            rule
        } else {
            now.cloned().unwrap_or(Value::Null)
        };
        out[key] = rule;
    }
    let max_type = match &p.max_instance_type {
        Some(t) => t.clone(),
        None => current.get("maxInstanceType").and_then(Value::as_str).unwrap_or_default().to_string(),
    };
    if !max_type.is_empty() {
        out["maxInstanceType"] = json!(max_type);
    }
    let max_disk = p.max_disk_gb.or_else(|| current.get("maxDiskGb").and_then(Value::as_u64)).unwrap_or(0);
    if max_disk > 0 {
        out["maxDiskGb"] = json!(max_disk);
    }
    Ok(out)
}

/// Refuses a policy the API would refuse, or one that could never act: a limit outside the
/// catalog, a limit not above the current size, or an enabled rule without its limit. A limit
/// given on the command line is checked even when no rule uses it.
fn check(t: Target, policy: &Value, p: &Policy, now: &Current) -> Result<()> {
    let on = |k: &str| policy.get(k).is_some_and(Value::is_object);
    if !RULES.iter().any(|(k, _)| on(k)) {
        return Err(Error::invalid(format!(
            "This would leave every rule off. 'autoscaling clear' is how to turn autoscaling off for a {}.",
            t.noun()
        )));
    }
    let noun = t.noun();

    let compute = on("cpu") || on("memory");
    let max_type = policy.get("maxInstanceType").and_then(Value::as_str).unwrap_or_default();
    if compute && max_type.is_empty() {
        return Err(Error::invalid(format!(
            "A CPU or memory rule needs --max-instance-type: the largest tier autoscaling may grow the {noun} to."
        )));
    }
    if compute || p.max_instance_type.is_some() {
        let Some(max) = catalog::tier(max_type) else {
            return Err(Error::invalid(format!(
                "--max-instance-type '{max_type}' is not an instance type. Use one of: {}.",
                catalog::instance_types()
            )));
        };
        if let Some(cur_name) = now.instance_type.as_deref()
            && let Some(cur) = catalog::tier(cur_name)
        {
            if catalog::family(max_type) != catalog::family(cur_name) {
                return Err(Error::invalid(format!(
                    "--max-instance-type {max_type} is a {} tier and the {noun} is {cur_name}, a {} one. Autoscaling \
                     stays in the current CPU family.",
                    catalog::family(max_type),
                    catalog::family(cur_name)
                )));
            }
            if max < cur || (compute && max == cur) {
                let larger: Vec<_> = catalog::INSTANCE_TYPES
                    .iter()
                    .enumerate()
                    .filter(|(i, n)| *i > cur && catalog::family(n) == catalog::family(cur_name))
                    .map(|(_, n)| *n)
                    .collect();
                let room = if larger.is_empty() {
                    format!(
                        "{cur_name} is the largest {} tier, so there is no room to grow.",
                        catalog::family(cur_name)
                    )
                } else {
                    format!("Larger tiers: {}.", larger.join(", "))
                };
                return Err(Error::invalid(format!(
                    "--max-instance-type {max_type} is not larger than the {noun}'s current tier ({cur_name}), so a CPU \
                     or memory rule could never grow it. {room}"
                )));
            }
        }
    }

    let disk = on("disk");
    let max_disk = policy.get("maxDiskGb").and_then(Value::as_u64).unwrap_or(0);
    if disk && max_disk == 0 {
        return Err(Error::invalid(format!(
            "The disk rule needs --max-disk-gb: the largest disk in GB autoscaling may grow the {noun} to."
        )));
    }
    if disk || p.max_disk_gb.is_some() {
        if !t.disk_sizes().contains(&max_disk) {
            return Err(Error::invalid(format!(
                "--max-disk-gb {max_disk} is not a {noun} disk size. Use one of: {}.",
                catalog::sizes(t.disk_sizes())
            )));
        }
        if let Some(cur) = now.disk_gb
            && (max_disk < cur || (disk && max_disk == cur))
        {
            let larger: Vec<_> = t.disk_sizes().iter().filter(|s| **s > cur).collect();
            let room = if larger.is_empty() {
                "There is no larger size to grow to.".to_string()
            } else {
                format!("Larger sizes: {}.", larger.iter().map(|s| s.to_string()).collect::<Vec<_>>().join(", "))
            };
            return Err(Error::invalid(format!(
                "--max-disk-gb {max_disk} is not larger than the {noun}'s current disk ({cur} GB), so the disk rule \
                 could never grow it. {room}"
            )));
        }
    }
    Ok(())
}

/// What `get` prints: the resource's id, name and size, and `autoscaling` as the API returned it.
/// Fields are picked, never copied whole, so a database's connection (and password) stays out.
fn summary(t: Target, id: &str, v: &Value) -> Value {
    let mut out = obj! { t.id_key() => id };
    for key in ["name", "instanceType", "diskSizeGb"] {
        if let Some(x) = v.get(key) {
            out[key] = x.clone();
        }
    }
    out["autoscaling"] = v.get("autoscaling").cloned().unwrap_or(Value::Null);
    out
}

fn dry_run(t: Target, id: &str, before: &Value, path: &str, body: Value) -> Value {
    obj! {
        "status" => "dry_run",
        t.id_key() => id,
        "previous" => before,
        "request" => obj! { "method" => t.method(), "path" => path, "body" => body },
    }
}

/// The receipt for a write. A server answers 202 with no body; a database answers with itself,
/// of which only `autoscaling` is kept (the rest holds the connection password).
fn written(t: Target, id: &str, action: &str, before: &Value, policy: &Value, response: &Value) -> Value {
    let status = match t {
        Target::Server => "accepted",
        Target::Postgres => "updated",
    };
    let mut out = obj! {
        "status" => status,
        t.id_key() => id,
        "action" => format!("autoscaling-{action}"),
        "previous" => before,
        "policy" => policy,
    };
    if let Some(a) = response.get("autoscaling") {
        out["autoscaling"] = a.clone();
    }
    out
}

/// The `set` command that restores `policy`, or `None` when it has no rule on.
fn undo(t: Target, id: &str, account: &str, policy: &Value) -> Option<String> {
    let mut cmd = t.command(id, account);
    let mut any = false;
    for (key, _) in RULES {
        let Some(rule) = policy.get(key).filter(|r| r.is_object()) else { continue };
        let (Some(threshold), Some(minutes)) =
            (rule.get("thresholdPercent").and_then(Value::as_u64), rule.get("durationMinutes").and_then(Value::as_u64))
        else {
            continue;
        };
        cmd.push_str(&format!(" --{key}-threshold {threshold} --{key}-minutes {minutes}"));
        any = true;
    }
    if !any {
        return None;
    }
    let compute = ["cpu", "memory"].iter().any(|k| policy.get(*k).is_some_and(Value::is_object));
    if let Some(max) = policy.get("maxInstanceType").and_then(Value::as_str).filter(|s| compute && !s.is_empty()) {
        cmd.push_str(&format!(" --max-instance-type {max}"));
    }
    if let Some(max) = policy
        .get("maxDiskGb")
        .and_then(Value::as_u64)
        .filter(|n| *n > 0 && policy.get("disk").is_some_and(Value::is_object))
    {
        cmd.push_str(&format!(" --max-disk-gb {max}"));
    }
    Some(cmd)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The policy a resource with autoscaling off reports (seen live on spec 0.13.0).
    fn off() -> Value {
        json!({"cpu": null, "memory": null, "disk": null, "maxDiskGb": 0, "maxInstanceType": ""})
    }

    fn server(instance_type: &str, disk: u64) -> Current {
        Current { instance_type: Some(instance_type.into()), disk_gb: Some(disk) }
    }

    fn postgres(disk: u64) -> Current {
        Current { instance_type: None, disk_gb: Some(disk) }
    }

    fn msg(r: Result<()>) -> String {
        let e = r.expect_err("should be refused");
        assert_eq!(e.code, crate::error::ErrorCode::InvalidInput);
        e.message
    }

    #[test]
    fn turning_rules_on_sends_the_whole_policy_and_drops_unset_limits() {
        let p = Policy {
            cpu_threshold: Some(80),
            cpu_minutes: Some(5),
            max_instance_type: Some("large".into()),
            ..Policy::default()
        };
        let policy = merge(&off(), &p).unwrap();
        assert_eq!(
            policy,
            json!({"cpu": {"thresholdPercent": 80, "durationMinutes": 5}, "memory": null, "disk": null,
                   "maxInstanceType": "large"})
        );
        let keys: Vec<_> = policy.as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, ["cpu", "memory", "disk", "maxInstanceType"], "stable order, nothing else");
        check(Target::Server, &policy, &p, &server("base", 20)).unwrap();
    }

    #[test]
    fn what_is_not_named_is_kept() {
        let current = json!({
            "cpu": {"thresholdPercent": 80, "durationMinutes": 5},
            "memory": {"thresholdPercent": 75, "durationMinutes": 10},
            "disk": {"thresholdPercent": 90, "durationMinutes": 30},
            "maxInstanceType": "x-large", "maxDiskGb": 250,
        });
        let p = Policy { memory_minutes: Some(3), no_disk: true, ..Policy::default() };
        let policy = merge(&current, &p).unwrap();
        assert_eq!(policy["cpu"], current["cpu"]);
        assert_eq!(policy["memory"], json!({"thresholdPercent": 75, "durationMinutes": 3}), "only minutes changed");
        assert_eq!(policy["disk"], Value::Null);
        assert_eq!((policy["maxInstanceType"].as_str(), policy["maxDiskGb"].as_u64()), (Some("x-large"), Some(250)));
    }

    #[test]
    fn a_rule_that_is_off_needs_both_numbers() {
        let p = Policy { disk_threshold: Some(85), ..Policy::default() };
        let e = merge(&off(), &p).unwrap_err();
        assert!(e.message.contains("--disk-threshold and --disk-minutes"), "{}", e.message);
    }

    #[test]
    fn unknown_fields_on_a_rule_survive_a_change() {
        let current = json!({"cpu": {"thresholdPercent": 80, "durationMinutes": 5, "cooldown": 7}, "memory": null,
                             "disk": null, "maxInstanceType": "large"});
        let p = Policy { cpu_threshold: Some(60), ..Policy::default() };
        assert_eq!(
            merge(&current, &p).unwrap()["cpu"],
            json!({"thresholdPercent": 60, "durationMinutes": 5, "cooldown": 7})
        );
    }

    #[test]
    fn limits_come_from_the_catalog() {
        let p = Policy {
            cpu_threshold: Some(80),
            cpu_minutes: Some(5),
            max_instance_type: Some("huge".into()),
            ..Policy::default()
        };
        let policy = merge(&off(), &p).unwrap();
        assert!(msg(check(Target::Server, &policy, &p, &server("base", 20))).contains("not an instance type"));

        let p = Policy { disk_threshold: Some(85), disk_minutes: Some(10), max_disk_gb: Some(60), ..Policy::default() };
        let policy = merge(&off(), &p).unwrap();
        assert!(msg(check(Target::Server, &policy, &p, &server("base", 20))).contains("50, 100, 250, 500, 1000"));
        // 10 is a Postgres size, not a server one.
        let p = Policy { max_disk_gb: Some(10), ..p };
        let policy = merge(&off(), &p).unwrap();
        assert!(msg(check(Target::Server, &policy, &p, &server("base", 20))).contains("not a server disk size"));
    }

    #[test]
    fn a_limit_must_be_above_the_current_size() {
        let p = Policy {
            memory_threshold: Some(80),
            memory_minutes: Some(5),
            max_instance_type: Some("medium".into()),
            ..Policy::default()
        };
        let policy = merge(&off(), &p).unwrap();
        let m = msg(check(Target::Server, &policy, &p, &server("large", 20)));
        assert!(
            m.contains("not larger than the server's current tier (large)") && m.contains("x-large, xx-large"),
            "{m}"
        );
        // Equal is no room to grow either.
        assert!(msg(check(Target::Server, &policy, &p, &server("medium", 20))).contains("not larger"));
        assert!(msg(check(Target::Server, &policy, &p, &server("dedicated-base", 20))).contains("CPU family"));
        check(Target::Server, &policy, &p, &server("base", 20)).unwrap();

        let p =
            Policy { disk_threshold: Some(85), disk_minutes: Some(10), max_disk_gb: Some(100), ..Policy::default() };
        let policy = merge(&off(), &p).unwrap();
        let m = msg(check(Target::Postgres, &policy, &p, &postgres(100)));
        assert!(m.contains("current disk (100 GB)") && m.contains("250, 500, 1000"), "{m}");
        assert!(msg(check(Target::Postgres, &policy, &p, &postgres(250))).contains("not larger"));
        check(Target::Postgres, &policy, &p, &postgres(10)).unwrap();
    }

    #[test]
    fn the_largest_tier_has_no_room_to_grow() {
        let p = Policy {
            cpu_threshold: Some(80),
            cpu_minutes: Some(5),
            max_instance_type: Some("dedicated-xxx-large".into()),
            ..Policy::default()
        };
        let policy = merge(&off(), &p).unwrap();
        let m = msg(check(Target::Server, &policy, &p, &server("dedicated-xxx-large", 20)));
        assert!(m.contains("largest dedicated tier"), "{m}");
    }

    #[test]
    fn postgres_compute_is_checked_against_the_catalog_only() {
        // The API does not say which tier a database is on, so any catalog tier passes here.
        let p = Policy {
            cpu_threshold: Some(80),
            cpu_minutes: Some(5),
            max_instance_type: Some("starter".into()),
            ..Policy::default()
        };
        let policy = merge(&off(), &p).unwrap();
        check(Target::Postgres, &policy, &p, &postgres(10)).unwrap();
        // ...unless a later API version reports one.
        let now = Current { instance_type: Some("base".into()), disk_gb: Some(10) };
        assert!(msg(check(Target::Postgres, &policy, &p, &now)).contains("database's current tier (base)"));
    }

    #[test]
    fn enabled_rules_need_their_limit() {
        let p = Policy { cpu_threshold: Some(80), cpu_minutes: Some(5), ..Policy::default() };
        let policy = merge(&off(), &p).unwrap();
        assert!(msg(check(Target::Server, &policy, &p, &server("base", 20))).contains("needs --max-instance-type"));
        let p = Policy { disk_threshold: Some(80), disk_minutes: Some(5), ..Policy::default() };
        let policy = merge(&off(), &p).unwrap();
        assert!(msg(check(Target::Postgres, &policy, &p, &postgres(10))).contains("needs --max-disk-gb"));
    }

    #[test]
    fn a_limit_no_rule_uses_is_still_checked_when_named() {
        let current = json!({"cpu": {"thresholdPercent": 80, "durationMinutes": 5}, "memory": null, "disk": null,
                             "maxInstanceType": "large", "maxDiskGb": 0});
        let p = Policy { max_disk_gb: Some(5), ..Policy::default() };
        let policy = merge(&current, &p).unwrap();
        assert!(msg(check(Target::Postgres, &policy, &p, &postgres(10))).contains("not a database disk size"));
        // A stored limit nothing uses is not ours to judge.
        let current = json!({"cpu": null, "memory": null, "disk": {"thresholdPercent": 80, "durationMinutes": 5},
                             "maxInstanceType": "gone-tier", "maxDiskGb": 50});
        let p = Policy { disk_minutes: Some(9), ..Policy::default() };
        let policy = merge(&current, &p).unwrap();
        check(Target::Postgres, &policy, &p, &postgres(10)).unwrap();
    }

    #[test]
    fn set_refuses_to_leave_every_rule_off() {
        let current = json!({"cpu": {"thresholdPercent": 80, "durationMinutes": 5}, "memory": null, "disk": null,
                             "maxInstanceType": "large"});
        let p = Policy { no_cpu: true, ..Policy::default() };
        let policy = merge(&current, &p).unwrap();
        assert!(msg(check(Target::Server, &policy, &p, &server("base", 20))).contains("autoscaling clear"));
    }

    #[test]
    fn nothing_named_is_refused() {
        assert!(named(&Policy::default()).is_err());
        assert!(named(&Policy { dry_run: true, ..Policy::default() }).is_err(), "--dry-run alone changes nothing");
        assert!(named(&Policy { no_memory: true, ..Policy::default() }).is_ok());
    }

    #[test]
    fn undo_restores_the_rules_and_the_limits_they_use() {
        let policy = json!({"cpu": {"thresholdPercent": 80, "durationMinutes": 5}, "memory": null,
                            "disk": {"thresholdPercent": 90, "durationMinutes": 15}, "maxInstanceType": "large",
                            "maxDiskGb": 100});
        assert_eq!(
            undo(Target::Postgres, "pg_1", "work", &policy).unwrap(),
            "sliplane postgres autoscaling set --postgres-id pg_1 -a work --cpu-threshold 80 --cpu-minutes 5 \
             --disk-threshold 90 --disk-minutes 15 --max-instance-type large --max-disk-gb 100"
        );
        assert_eq!(undo(Target::Server, "s", "work", &off()), None);
    }

    #[test]
    fn summary_never_copies_the_connection() {
        let db = json!({"id": "pg_1", "name": "main", "diskSizeGb": 10, "connection": {"password": "pw"},
                        "autoscaling": {"status": "idle", "policy": off()}});
        let out = summary(Target::Postgres, "pg_1", &db);
        assert_eq!(out["postgresId"], "pg_1");
        assert_eq!(out["autoscaling"]["status"], "idle");
        assert!(out.get("connection").is_none() && !out.to_string().contains("pw"));
    }
}
