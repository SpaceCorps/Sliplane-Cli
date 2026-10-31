//! The Postgres commands that have logic of their own: the IP allow list, `logs --since`, and
//! `restore` choosing its own point in time. The one-request commands stay in `mod.rs`.
//!
//! **What an allow list means** (spec 0.11.0, create and update, and the official CLI agrees): a
//! database created without one accepts every address (`0.0.0.0/0` and `::/0`), and an *empty* list
//! blocks every public connection. So emptying the list locks clients out - it never opens anything -
//! and the way to open a database to the world is to put a `/0` entry on it, which `allowlist add`
//! therefore refuses without `--allow-open`. The API replaces the list as a whole, so every change is
//! read, edited and written back here, and an answer without the list is never edited blind.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use serde_json::{Value, json};

use crate::cli::{Allowlist, MyIp, PgId};
use crate::client::{Client, Family, query, seg};
use crate::commands::{client, done, print};
use crate::error::{Error, ErrorCode, Result};
use crate::{obj, timeparse};

fn at(p: &PgId, tail: &str) -> String {
    format!("postgres/{}{tail}", seg(&p.postgres_id))
}

// ---------------------------------------------------------------------------------------------
// logs and restore

/// `--since` takes a duration or a timestamp; this API wants a timestamp.
pub fn logs(p: &PgId, since: Option<String>, until: Option<String>, limit: Option<u32>) -> Result<()> {
    let since = since.as_deref().map(timeparse::since).transpose()?.map(timeparse::to_rfc3339);
    let q = query(&[("since", since), ("until", until), ("limit", limit.map(|l| l.to_string()))]);
    print(client(&p.a)?.get(&at(p, &format!("/logs{q}")))?)
}

/// Without `--timestamp`, the latest point the restore window allows.
pub fn restore(p: &PgId, timestamp: Option<String>) -> Result<()> {
    let client = client(&p.a)?;
    let timestamp = match timestamp {
        Some(t) => t,
        None => {
            let window = client.get(&at(p, "/restore"))?;
            let latest = window.get("max").and_then(Value::as_str).filter(|s| !s.is_empty()).ok_or_else(|| {
                Error::other("The restore window did not say its latest point.").fix(format!(
                    "Name one yourself: sliplane postgres restore --postgres-id {} --timestamp <rfc3339> -a <account> \
                     (see sliplane postgres restore-window)",
                    p.postgres_id
                ))
            })?;
            eprintln!("note: no --timestamp given; restoring to the latest restorable point, {latest}");
            latest.to_string()
        }
    };
    let r = client.post(&at(p, "/restore"), &json!({ "timestamp": timestamp }))?;
    done(r, "accepted", obj! { "postgresId" => p.postgres_id, "action" => "restore", "timestamp" => timestamp })
}

// ---------------------------------------------------------------------------------------------
// addresses

/// An address range as the API wants it: host bits cleared, `/32` or `/128` for a single address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Cidr {
    addr: IpAddr,
    bits: u8,
}

impl Cidr {
    /// `203.0.113.7`, `203.0.113.0/24`, `2001:db8::/32`. A bare address is a single-host range, and an
    /// IPv4-mapped IPv6 address is the IPv4 address it wraps.
    fn parse(s: &str) -> Option<Cidr> {
        let s = s.trim();
        let (host, bits) = match s.split_once('/') {
            Some((host, bits)) => (host, Some(bits)),
            None => (s, None),
        };
        let mut addr: IpAddr = host.parse().ok()?;
        let bits = match bits {
            None => {
                addr = unmap(addr);
                max_bits(addr)
            }
            Some(b) => {
                if b.is_empty() || !b.bytes().all(|c| c.is_ascii_digit()) {
                    return None;
                }
                let n: u8 = b.parse().ok()?;
                if n > max_bits(addr) {
                    return None;
                }
                n
            }
        };
        Some(Cidr { addr: mask(addr, bits), bits })
    }

    fn host(addr: IpAddr) -> Cidr {
        let addr = unmap(addr);
        Cidr { addr, bits: max_bits(addr) }
    }

    fn contains(&self, ip: IpAddr) -> bool {
        ip.is_ipv4() == self.addr.is_ipv4() && mask(ip, self.bits) == self.addr
    }

    fn family(&self) -> Family {
        if self.addr.is_ipv4() { Family::V4 } else { Family::V6 }
    }
}

impl std::fmt::Display for Cidr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.addr, self.bits)
    }
}

fn max_bits(addr: IpAddr) -> u8 {
    if addr.is_ipv4() { 32 } else { 128 }
}

fn unmap(addr: IpAddr) -> IpAddr {
    match addr {
        IpAddr::V6(a) => a.to_ipv4_mapped().map_or(addr, IpAddr::V4),
        v4 => v4,
    }
}

fn mask(addr: IpAddr, bits: u8) -> IpAddr {
    match addr {
        IpAddr::V4(a) => {
            let m = if bits == 0 { 0 } else { u32::MAX << (32 - u32::from(bits)) };
            IpAddr::V4(Ipv4Addr::from(u32::from(a) & m))
        }
        IpAddr::V6(a) => {
            let m = if bits == 0 { 0 } else { u128::MAX << (128 - u32::from(bits)) };
            IpAddr::V6(Ipv6Addr::from(u128::from(a) & m))
        }
    }
}

fn label(family: Family) -> &'static str {
    match family {
        Family::V4 => "IPv4",
        Family::V6 => "IPv6",
    }
}

// ---------------------------------------------------------------------------------------------
// the list, and edits to it

/// One entry of the allow list. Only `source` and `description` can be written; the id is how
/// `remove` can name one.
#[derive(Clone, Debug, PartialEq)]
struct Rule {
    id: String,
    source: String,
    description: String,
}

impl Rule {
    fn cidr(&self) -> Option<Cidr> {
        Cidr::parse(&self.source)
    }

    /// What goes back to the API. A rule's source is sent as it was read, not as we would spell it.
    fn body(&self) -> Value {
        let mut b = obj! { "source" => self.source };
        if !self.description.is_empty() {
            b["description"] = json!(self.description);
        }
        b
    }

    fn entry(&self) -> Value {
        let mut b = obj! { "source" => self.source };
        if !self.description.is_empty() {
            b["description"] = json!(self.description);
        }
        if !self.id.is_empty() {
            b["id"] = json!(self.id);
        }
        b
    }
}

fn text(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

/// The allow list of a `GET /postgres/{id}` answer. Without one there is nothing to edit: writing
/// back a list built from the new entries alone would replace, and so delete, whatever is there.
fn rules_of(db: &Value) -> Result<Vec<Rule>> {
    let list = db.get("ipAllowList").and_then(Value::as_array).ok_or_else(|| {
        Error::other("The API's answer has no ipAllowList, so the list cannot be edited safely.").detail(
            "The API replaces the allow list as a whole; an edit made without seeing it would delete its entries.",
        )
    })?;
    Ok(list
        .iter()
        .map(|e| Rule { id: text(e, "id"), source: text(e, "source"), description: text(e, "description") })
        .collect())
}

struct AddPlan {
    rules: Vec<Rule>,
    added: Vec<String>,
    relabeled: Vec<String>,
}

/// New ranges go on the end, in the order given; one that is already there (by range, however it
/// is spelled) is not added twice. `description` labels what this call adds, and relabels a range
/// that is already there.
fn plan_add(current: &[Rule], wanted: &[Cidr], description: Option<&str>) -> AddPlan {
    let mut plan = AddPlan { rules: current.to_vec(), added: Vec::new(), relabeled: Vec::new() };
    for c in wanted {
        match plan.rules.iter_mut().find(|r| r.cidr() == Some(*c)) {
            Some(rule) => {
                if let Some(d) = description
                    && rule.description != d
                {
                    rule.description = d.to_string();
                    plan.relabeled.push(rule.source.clone());
                }
            }
            None => {
                let source = c.to_string();
                plan.rules.push(Rule {
                    id: String::new(),
                    source: source.clone(),
                    description: description.unwrap_or("").to_string(),
                });
                plan.added.push(source);
            }
        }
    }
    plan
}

#[derive(Debug)]
enum Target {
    Range(Cidr),
    Id(String),
}

impl Target {
    fn parse(arg: &str) -> Target {
        Cidr::parse(arg).map_or_else(|| Target::Id(arg.trim().to_string()), Target::Range)
    }

    fn matches(&self, rule: &Rule) -> bool {
        match self {
            Target::Range(c) => rule.cidr() == Some(*c),
            Target::Id(id) => !id.is_empty() && rule.id == *id,
        }
    }
}

/// What is left, and what went. `Err` names the targets that matched no entry: a removal that
/// quietly does nothing for a typo is how a rule stays when the agent thinks it is gone.
fn plan_remove(
    current: &[Rule],
    targets: &[(String, Target)],
) -> std::result::Result<(Vec<Rule>, Vec<Rule>), Vec<String>> {
    let unmatched: Vec<String> =
        targets.iter().filter(|(_, t)| !current.iter().any(|r| t.matches(r))).map(|(shown, _)| shown.clone()).collect();
    if !unmatched.is_empty() {
        return Err(unmatched);
    }
    let (gone, kept) = current.iter().cloned().partition(|r| targets.iter().any(|(_, t)| t.matches(r)));
    Ok((kept, gone))
}

/// The family the list's entries share, if there is one.
fn family_of(rules: &[Rule]) -> Option<Family> {
    let mut families = rules.iter().filter_map(Rule::cidr).map(|c| c.family());
    let first = families.next()?;
    families.all(|f| f == first).then_some(first)
}

/// `blocked`, `open` or `restricted`, with a warning for each reason to look twice.
pub(super) fn assess(list: &Value) -> (&'static str, Vec<String>) {
    let ranges: Vec<Cidr> = list
        .as_array()
        .map(|a| a.iter().filter_map(|e| e.get("source").and_then(Value::as_str)).filter_map(Cidr::parse).collect())
        .unwrap_or_default();
    if list.as_array().is_some_and(Vec::is_empty) {
        return (
            "blocked",
            vec!["The list is empty: the database refuses every public connection, from every address.".into()],
        );
    }
    let warnings: Vec<String> = ranges
        .iter()
        .filter(|c| c.bits == 0)
        .map(|c| {
            format!(
                "{c} lets every {} address connect: the database is open to the whole internet over {}, protected only by its credentials.",
                label(c.family()),
                label(c.family())
            )
        })
        .collect();
    (if warnings.is_empty() { "restricted" } else { "open" }, warnings)
}

// ---------------------------------------------------------------------------------------------
// this machine's address

/// The public address the API sees this machine at, over `family` and without any proxy: the address
/// a database would see. The family has to match the one the client connects to the database with,
/// or the entry that gets added lets in an address that never connects.
fn probe(client: &Client, family: Family) -> Result<IpAddr> {
    let name = label(family);
    let (_, headers) = client.direct(family).get_with_headers("me").map_err(|e| {
        if e.code == ErrorCode::Network && e.message.starts_with("Could not reach") {
            Error::new(ErrorCode::Network, format!("Could not reach the Sliplane API over {name}."))
                .detail(e.detail.unwrap_or_default())
                .fix(format!(
                    "This machine may have no {name} route to the API. Try the other family (--ipv4 or --ipv6), \
                     or pass the address yourself as an IP or CIDR."
                ))
        } else {
            e
        }
    })?;
    let raw = headers.get("sliplane-client-ip").and_then(|v| v.to_str().ok()).map_or("", str::trim);
    let ip = raw.parse::<IpAddr>().map(unmap).map_err(|_| {
        Error::other("The API did not report this machine's public address.")
            .detail(format!("Its Sliplane-Client-IP header was {raw:?}."))
            .fix("Pass the address yourself as an IP or CIDR.")
    })?;
    if ip.is_ipv4() != (family == Family::V4) {
        return Err(Error::other(format!("Asked for an {name} address, the API answered {ip}."))
            .fix("Pass the address yourself as an IP or CIDR."));
    }
    Ok(ip)
}

/// `--my-ip`: the family from `--ipv4`/`--ipv6`, else the one the list already uses.
fn my_ip(client: &Client, me: &MyIp, current: &[Rule], p: &PgId) -> Result<IpAddr> {
    let family = if me.ipv4 {
        Family::V4
    } else if me.ipv6 {
        Family::V6
    } else {
        let Some(family) = family_of(current) else {
            return Err(Error::invalid(format!(
                "--my-ip needs an IP family, and database '{}' gives none: its allow list is empty or mixes IPv4 and IPv6.",
                p.postgres_id
            ))
            .detail(
                "It has to be the family this machine connects to the database with. IPv4 is the usual one; \
                 pick IPv6 only if you know the connection goes out over IPv6.",
            )
            .fix("Repeat the command with --ipv4 (or --ipv6)."));
        };
        let other = if family == Family::V4 { "--ipv6" } else { "--ipv4" };
        eprintln!(
            "note: --my-ip is the {} address, because every entry on the allow list is {}. Pass {other} for the other.",
            label(family),
            label(family)
        );
        family
    };
    probe(client, family)
}

// ---------------------------------------------------------------------------------------------
// the commands

pub fn allowlist(c: Allowlist) -> Result<()> {
    match c {
        Allowlist::Show { p, me } => show(&p, &me),
        Allowlist::Add { p, sources, me, description, allow_open } => add(&p, &sources, &me, description, allow_open),
        Allowlist::Remove { p, sources, me } => remove(&p, &sources, &me),
        Allowlist::Clear(p) => clear(&p),
    }
}

fn read(client: &Client, p: &PgId) -> Result<(Value, Vec<Rule>)> {
    let db = client.get(&at(p, ""))?;
    let rules = rules_of(&db)?;
    Ok((db, rules))
}

/// Writes `rules` as the whole list; the list the API reports back (or, if it reports none, what was sent).
fn save(client: &Client, p: &PgId, db: &Value, rules: &[Rule]) -> Result<(Value, Value)> {
    let body = json!({ "ipAllowList": rules.iter().map(Rule::body).collect::<Vec<_>>() });
    let saved = client.patch(&at(p, ""), &body)?;
    let list = match saved.get("ipAllowList") {
        Some(l) if l.is_array() => l.clone(),
        _ => Value::Array(rules.iter().map(Rule::entry).collect()),
    };
    let name = saved.get("name").or_else(|| db.get("name")).cloned().unwrap_or(Value::Null);
    Ok((name, list))
}

/// `extra` goes between the verdict and the list.
fn report(p: &PgId, name: &Value, status: Option<&str>, extra: Vec<(&str, Value)>, list: Value) -> Value {
    let (access, warnings) = assess(&list);
    let mut out = obj! {};
    if let Some(s) = status {
        out["status"] = json!(s);
    }
    out["postgresId"] = json!(p.postgres_id);
    out["name"] = name.clone();
    out["access"] = json!(access);
    for (k, v) in extra {
        out[k] = v;
    }
    out["ipAllowList"] = list;
    if !warnings.is_empty() {
        out["warnings"] = json!(warnings);
    }
    out
}

fn show(p: &PgId, me: &MyIp) -> Result<()> {
    let client = client(&p.a)?;
    let (db, rules) = read(&client, p)?;
    let mut extra = Vec::new();
    if me.my_ip {
        let ip = my_ip(&client, me, &rules, p)?;
        let allowed = rules.iter().any(|r| r.cidr().is_some_and(|c| c.contains(ip)));
        extra.push(("myIp", json!(ip.to_string())));
        extra.push(("myIpAllowed", json!(allowed)));
    }
    let list = db["ipAllowList"].clone();
    print(report(p, &db["name"], None, extra, list))
}

fn add(p: &PgId, sources: &[String], me: &MyIp, description: Option<String>, allow_open: bool) -> Result<()> {
    if sources.is_empty() && !me.my_ip {
        return Err(Error::invalid("Name what to allow: one or more IP addresses or CIDR ranges, or --my-ip."));
    }
    let mut wanted = Vec::new();
    for s in sources {
        wanted.push(Cidr::parse(s).ok_or_else(|| {
            Error::invalid(format!("'{s}' is not an IP address or CIDR range."))
                .fix("Use an address like 203.0.113.7, or a range like 203.0.113.0/24 or 2001:db8::/32.")
        })?);
    }
    if let Some(world) = wanted.iter().find(|c| c.bits == 0)
        && !allow_open
    {
        return Err(Error::invalid(format!(
            "{world} lets every {} address connect: it opens the database to the whole internet.",
            label(world.family())
        ))
        .detail("Only the credentials would stand between the internet and the data. Do this only if the human asked for it.")
        .fix("If they did, repeat the command with --allow-open. Otherwise allow just the addresses that need access (--my-ip for this machine)."));
    }
    let description = description.filter(|d| !d.is_empty());

    let client = client(&p.a)?;
    let (db, current) = read(&client, p)?;
    if me.my_ip {
        wanted.push(Cidr::host(my_ip(&client, me, &current, p)?));
    }
    let plan = plan_add(&current, &wanted, description.as_deref());
    if plan.added.is_empty() && plan.relabeled.is_empty() {
        return print(report(p, &db["name"], Some("unchanged"), vec![], db["ipAllowList"].clone()));
    }
    let (name, list) = save(&client, p, &db, &plan.rules)?;
    let mut extra = vec![("added", json!(plan.added))];
    if !plan.relabeled.is_empty() {
        extra.push(("relabeled", json!(plan.relabeled)));
    }
    print(report(p, &name, Some("updated"), extra, list))
}

fn remove(p: &PgId, sources: &[String], me: &MyIp) -> Result<()> {
    if sources.is_empty() && !me.my_ip {
        return Err(Error::invalid(
            "Name what to remove: one or more IP addresses, CIDR ranges or rule ids, or --my-ip.",
        ));
    }
    let mut targets: Vec<(String, Target)> = sources.iter().map(|s| (s.clone(), Target::parse(s))).collect();

    let client = client(&p.a)?;
    let (db, current) = read(&client, p)?;
    if me.my_ip {
        let c = Cidr::host(my_ip(&client, me, &current, p)?);
        targets.push((format!("{c} (this machine)"), Target::Range(c)));
    }
    let (kept, gone) = plan_remove(&current, &targets).map_err(|unmatched| {
        let known: Vec<String> = current
            .iter()
            .map(|r| if r.id.is_empty() { r.source.clone() } else { format!("{} ({})", r.source, r.id) })
            .collect();
        Error::new(ErrorCode::NotFound, format!("The allow list has no entry for: {}.", unmatched.join(", ")))
            .detail(if known.is_empty() {
                "The list is empty.".to_string()
            } else {
                format!("It has: {}.", known.join(", "))
            })
            .fix(format!(
                "sliplane postgres allowlist show --postgres-id {} -a {}",
                p.postgres_id,
                p.a.account.as_deref().unwrap_or("<account>")
            ))
    })?;
    if kept.is_empty() {
        return Err(Error::invalid("That removes the last entry, and an empty allow list refuses every public connection.")
            .detail("Every client that connects over the internet would be locked out at once. If that is the intent, `clear` says so.")
            .fix(format!(
                "sliplane postgres allowlist clear --postgres-id {} -a {}",
                p.postgres_id,
                p.a.account.as_deref().unwrap_or("<account>")
            )));
    }
    let (name, list) = save(&client, p, &db, &kept)?;
    let removed: Vec<Value> = gone.iter().map(Rule::entry).collect();
    print(report(p, &name, Some("updated"), vec![("removed", json!(removed))], list))
}

fn clear(p: &PgId) -> Result<()> {
    let client = client(&p.a)?;
    let (db, current) = read(&client, p)?;
    if current.is_empty() {
        return print(report(p, &db["name"], Some("unchanged"), vec![], db["ipAllowList"].clone()));
    }
    let (name, list) = save(&client, p, &db, &[])?;
    let removed: Vec<Value> = current.iter().map(Rule::entry).collect();
    let sources: Vec<&str> = current.iter().map(|r| r.source.as_str()).collect();
    let undo = format!(
        "sliplane postgres allowlist add --postgres-id {} {} -a {}",
        p.postgres_id,
        sources.join(" "),
        p.a.account.as_deref().unwrap_or("<account>")
    );
    print(report(p, &name, Some("updated"), vec![("removed", json!(removed)), ("undo", json!(undo))], list))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cidr(s: &str) -> Cidr {
        Cidr::parse(s).unwrap_or_else(|| panic!("{s} should parse"))
    }

    fn rule(id: &str, source: &str, description: &str) -> Rule {
        Rule { id: id.into(), source: source.into(), description: description.into() }
    }

    fn sources(rules: &[Rule]) -> Vec<&str> {
        rules.iter().map(|r| r.source.as_str()).collect()
    }

    #[test]
    fn addresses_become_host_ranges_and_ranges_lose_their_host_bits() {
        assert_eq!(cidr("203.0.113.7").to_string(), "203.0.113.7/32");
        assert_eq!(cidr("2001:db8::1").to_string(), "2001:db8::1/128");
        assert_eq!(cidr("10.1.2.3/24").to_string(), "10.1.2.0/24");
        assert_eq!(cidr("2001:db8:ffff::1/32").to_string(), "2001:db8::/32");
        assert_eq!(cidr("0.0.0.0/0").to_string(), "0.0.0.0/0");
        assert_eq!(cidr("1.2.3.4/0").to_string(), "0.0.0.0/0");
        assert_eq!(cidr("::/0").to_string(), "::/0");
        assert_eq!(cidr(" 10.0.0.0/8 ").to_string(), "10.0.0.0/8");
        // An IPv4-mapped address is the IPv4 address; `::1` is not 0.0.0.1.
        assert_eq!(cidr("::ffff:203.0.113.7").to_string(), "203.0.113.7/32");
        assert_eq!(cidr("::1").to_string(), "::1/128");
    }

    #[test]
    fn nonsense_is_not_a_range() {
        for bad in [
            "",
            "/",
            "1.2.3.4/",
            "1.2.3.4/33",
            "1.2.3.4/-1",
            "1.2.3.4/+8",
            "1.2.3.4/8/8",
            "::/129",
            "1.2.3",
            "1.2.3.256",
            "example.com",
            "1.2.3.4 /32",
            "fe80::1%eth0",
            "any",
            "pgfw_abc123",
        ] {
            assert_eq!(Cidr::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn a_range_contains_its_own_family_only() {
        let office = cidr("203.0.113.0/24");
        assert!(office.contains("203.0.113.200".parse().unwrap()));
        assert!(!office.contains("203.0.114.1".parse().unwrap()));
        assert!(!office.contains("::ffff:203.0.113.7".parse().unwrap()), "mapped addresses are a different family");
        assert!(cidr("0.0.0.0/0").contains("8.8.8.8".parse().unwrap()));
        assert!(!cidr("0.0.0.0/0").contains("2001:db8::1".parse().unwrap()));
        assert!(cidr("::/0").contains("2001:db8::1".parse().unwrap()));
        assert!(cidr("2001:db8::/32").contains("2001:db8:1::1".parse().unwrap()));
        assert!(!cidr("2001:db8::/32").contains("2001:db9::1".parse().unwrap()));
    }

    #[test]
    fn add_keeps_everything_and_appends_in_order_without_duplicates() {
        let current = vec![rule("pgfw_1", "10.0.0.0/8", "vpn"), rule("pgfw_2", "2001:db8::/32", "")];
        let wanted = [cidr("203.0.113.7"), cidr("10.9.9.9/8"), cidr("203.0.113.7/32"), cidr("198.51.100.0/24")];
        let plan = plan_add(&current, &wanted, None);
        assert_eq!(sources(&plan.rules), ["10.0.0.0/8", "2001:db8::/32", "203.0.113.7/32", "198.51.100.0/24"]);
        assert_eq!(plan.added, ["203.0.113.7/32", "198.51.100.0/24"]);
        assert!(plan.relabeled.is_empty());
        // The entries that were there are untouched, labels and ids included.
        assert_eq!(plan.rules[0], current[0]);
        assert_eq!(plan.rules[1], current[1]);
    }

    #[test]
    fn add_labels_new_entries_and_relabels_existing_ones_only_when_asked() {
        let current = vec![rule("pgfw_1", "10.0.0.0/8", "vpn")];
        let plan = plan_add(&current, &[cidr("10.0.0.1/8"), cidr("192.0.2.1")], Some("laptop"));
        assert_eq!(plan.rules[0].description, "laptop");
        assert_eq!(plan.rules[1].description, "laptop");
        assert_eq!(plan.relabeled, ["10.0.0.0/8"]);
        assert_eq!(plan.added, ["192.0.2.1/32"]);

        let plan = plan_add(&current, &[cidr("10.0.0.0/8")], None);
        assert!(plan.added.is_empty() && plan.relabeled.is_empty(), "no label given, nothing to change");
        let plan = plan_add(&current, &[cidr("10.0.0.0/8")], Some("vpn"));
        assert!(plan.relabeled.is_empty(), "same label, nothing to change");
    }

    #[test]
    fn remove_matches_by_range_or_id_and_keeps_the_rest_in_order() {
        let current = vec![
            rule("pgfw_1", "10.0.0.0/8", ""),
            rule("pgfw_2", "203.0.113.7/32", "me"),
            rule("pgfw_3", "2001:db8::/32", ""),
            rule("pgfw_4", "192.0.2.0/24", ""),
        ];
        let targets =
            vec![("203.0.113.7".to_string(), Target::parse("203.0.113.7")), ("x".into(), Target::parse("pgfw_3"))];
        let (kept, gone) = plan_remove(&current, &targets).unwrap();
        assert_eq!(sources(&kept), ["10.0.0.0/8", "192.0.2.0/24"]);
        assert_eq!(sources(&gone), ["203.0.113.7/32", "2001:db8::/32"]);
    }

    #[test]
    fn remove_names_every_target_that_matched_nothing() {
        let current = vec![rule("pgfw_1", "10.0.0.0/8", "")];
        let targets = vec![
            ("10.0.0.0/8".to_string(), Target::parse("10.0.0.0/8")),
            ("10.0.0.0/16".to_string(), Target::parse("10.0.0.0/16")),
            ("pgfw_9".to_string(), Target::parse("pgfw_9")),
        ];
        assert_eq!(plan_remove(&current, &targets).unwrap_err(), ["10.0.0.0/16", "pgfw_9"]);
        // A narrower range is not the range that is there; nothing is removed by "close enough".
        assert!(Target::parse("10.0.0.0/16").matches(&rule("", "10.0.0.0/16", "")));
        assert!(!Target::parse("10.0.0.0/16").matches(&current[0]));
        assert!(!Target::parse("").matches(&current[0]), "an empty id matches nothing, not every rule without one");
    }

    #[test]
    fn the_family_follows_the_entries_only_when_they_agree() {
        assert_eq!(family_of(&[]), None);
        assert_eq!(family_of(&[rule("", "10.0.0.0/8", ""), rule("", "0.0.0.0/0", "")]), Some(Family::V4));
        assert_eq!(family_of(&[rule("", "2001:db8::/32", "")]), Some(Family::V6));
        assert_eq!(family_of(&[rule("", "10.0.0.0/8", ""), rule("", "::/0", "")]), None);
    }

    #[test]
    fn assess_calls_out_empty_and_open() {
        let (access, warnings) = assess(&json!([]));
        assert_eq!(access, "blocked");
        assert!(warnings[0].contains("refuses every public connection"));

        let (access, warnings) = assess(&json!([{"source": "10.0.0.0/8"}, {"source": "203.0.113.7/32"}]));
        assert_eq!((access, warnings.len()), ("restricted", 0));

        let (access, warnings) = assess(&json!([{"source": "0.0.0.0/0"}, {"source": "::/0"}]));
        assert_eq!(access, "open");
        assert!(
            warnings[0].contains("every IPv4 address") && warnings[1].contains("every IPv6 address"),
            "{warnings:?}"
        );

        // Written out some other way, a /0 is still the whole internet.
        assert_eq!(assess(&json!([{"source": "1.2.3.4/0"}])).0, "open");
    }

    #[test]
    fn a_list_that_cannot_be_seen_is_never_edited() {
        let e = rules_of(&json!({"id": "pg_1"})).unwrap_err();
        assert!(e.message.contains("no ipAllowList"), "{}", e.message);
        assert!(rules_of(&json!({"ipAllowList": null})).is_err());
        assert_eq!(rules_of(&json!({"ipAllowList": []})).unwrap(), vec![]);
    }

    #[test]
    fn a_rule_is_sent_back_as_it_was_read() {
        assert_eq!(rule("pgfw_1", "10.0.0.0/8", "vpn").body(), json!({"source": "10.0.0.0/8", "description": "vpn"}));
        assert_eq!(rule("pgfw_1", "10.0.0.5/8", "").body(), json!({"source": "10.0.0.5/8"}));
    }
}
