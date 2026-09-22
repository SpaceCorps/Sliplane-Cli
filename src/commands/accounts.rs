//! `accounts add|list|test|remove`. Nothing here takes `--account`: these are the commands that
//! manage what `--account` refers to.

use std::io::{BufRead, IsTerminal, Write};

use serde_json::Value;

use crate::account::{self, identity};
use crate::cli::Accounts;
use crate::client::Client;
use crate::commands::print;
use crate::config::{self, AccountConfig};
use crate::error::{Error, ErrorCode, Result};
use crate::obj;
use crate::secrets::{self, Store};

pub fn run(c: Accounts) -> Result<()> {
    match c {
        Accounts::Add { name, api_key, api_key_stdin, org_id, force, no_verify } => {
            // Keeps the key out of argv, shell history and anything that echoes the command.
            let api_key = if api_key_stdin { Some(read_stdin_key()?) } else { api_key };
            add(name, api_key, org_id, force, no_verify)
        }
        Accounts::List { check } => list(check),
        Accounts::Test { name } => test(&name),
        Accounts::Remove { name, yes } => remove(&name, yes),
    }
}

fn add(name: String, api_key: Option<String>, org_id: Option<String>, force: bool, no_verify: bool) -> Result<()> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err(Error::invalid("An account name is required."));
    }

    let store = secrets::store()?;
    let config = config::load()?;

    let existing = config.find(&name).map(|(k, _)| k.clone());
    if let Some(existing) = &existing
        && !force
    {
        return Err(Error::invalid(format!("An account named '{existing}' already exists.")).fix(format!(
            "Pick a different name, or replace its key: sliplane accounts add {existing} --api-key <key> --force"
        )));
    }
    let name = existing.clone().unwrap_or(name);

    let key = match api_key.map(|k| k.trim().to_string()).filter(|k| !k.is_empty()) {
        Some(k) => k,
        None => prompt_key(&name)?,
    };
    let org_id = org_id.map(|o| o.trim().to_string()).unwrap_or_default();

    let (mut ident, mut org) = (String::new(), String::new());
    if !no_verify {
        // Verify before storing: a key that does not work is worse than no account at all,
        // because every later failure looks like a problem with the command being run.
        let me = Client::new(&key, Some(&org_id)).get("me")?;
        ident = identity::describe(&me);
        org = identity::organization(&me);
    }

    {
        let _lock = config::lock()?;
        // The key goes in first: a config entry with no key is a broken account, while an
        // orphaned secret is invisible and harmless.
        store.set(&secrets::account_key(&name), &key)?;

        let mut config = config::load()?;
        config.accounts.insert(
            name.clone(),
            AccountConfig {
                org_id: org_id.clone(),
                identity: ident.clone(),
                organization: org.clone(),
                added_at: config::now_utc(),
            },
        );
        config::save(&config)?;
    }

    print(obj! {
        "status" => if existing.is_none() { "added" } else { "replaced" },
        "name" => name,
        "identity" => ident,
        "organization" => org,
        "orgId" => org_id,
        "verified" => !no_verify,
        "secretStore" => store.name(),
        "configDir" => config::config_dir().display().to_string(),
        "nextStep" => format!("sliplane projects list --account {name}"),
    })
}

fn read_stdin_key() -> Result<String> {
    let mut key = String::new();
    std::io::stdin().lock().read_line(&mut key).map_err(|e| Error::invalid(format!("Could not read stdin: {e}")))?;
    let key = key.trim().to_string();
    if key.is_empty() {
        return Err(Error::invalid("--api-key-stdin was given but stdin was empty."));
    }
    Ok(key)
}

/// Prompts on the terminal, never stdout, so stdout stays a clean payload.
fn prompt_key(name: &str) -> Result<String> {
    if !std::io::stdin().is_terminal() {
        return Err(Error::invalid("No API key given and no terminal to prompt on.")
            .fix(format!("pbpaste | sliplane accounts add {name} --api-key-stdin")));
    }
    loop {
        let key = rpassword::prompt_password(format!("API key for {name}: "))
            .map_err(|e| Error::other("Could not read the API key.").detail(e.to_string()))?;
        let key = key.trim().to_string();
        if !key.is_empty() {
            return Ok(key);
        }
        eprintln!("Cannot be empty");
    }
}

fn list(check: bool) -> Result<()> {
    let store = secrets::store()?;
    let config = config::load()?;
    let sorted = config.sorted();

    // One keystore lookup (and with --check, one API call) per account, run side by side.
    let statuses: Vec<String> = std::thread::scope(|scope| {
        let handles: Vec<_> =
            sorted.iter().map(|(name, acct)| scope.spawn(move || status_of(name, acct, store, check))).collect();
        handles.into_iter().map(|h| h.join().unwrap_or_else(|_| "unreachable".into())).collect()
    });

    let accounts: Vec<Value> = sorted
        .iter()
        .zip(statuses)
        .map(|((name, a), status)| {
            obj! {
                "name" => name,
                "identity" => a.identity,
                "organization" => a.organization,
                "orgId" => a.org_id,
                "addedAt" => a.added_at,
                "keyStatus" => status,
            }
        })
        .collect();

    print(obj! {
        "count" => accounts.len(),
        "accounts" => accounts,
        "secretStore" => store.name(),
        "configDir" => config::config_dir().display().to_string(),
    })
}

fn status_of(name: &str, acct: &AccountConfig, store: Store, check: bool) -> String {
    let key = match store.get(&secrets::account_key(name)) {
        Ok(Some(k)) if !k.trim().is_empty() => k,
        Ok(_) => return "missing_key".into(),
        Err(_) => return "unreadable".into(),
    };
    // Without --check this is what is actually known locally: a key is stored, and whether it
    // still works is a question only the API can answer.
    if !check {
        return "stored".into();
    }
    match Client::new(&key, Some(&acct.org_id)).get("me") {
        Ok(_) => "valid".into(),
        Err(e) if e.code == ErrorCode::AuthRequired => "rejected".into(),
        Err(_) => "unreachable".into(),
    }
}

fn test(name: &str) -> Result<()> {
    let account = account::resolve(Some(name))?;
    let me = account.client().get("me")?;
    let ident = identity::describe(&me);
    let org = identity::organization(&me);

    let mut result = obj! {
        "name" => account.name,
        "identity" => ident,
        "organization" => org,
        "orgId" => account.org_id().unwrap_or(""),
        "keyStatus" => "valid",
        "me" => me,
    };

    // Drift means the key was replaced with one belonging to somewhere else - worth saying out
    // loud, because every command run against this account now goes somewhere new.
    if let Some(w) = drift(&account.config.identity, &ident).or_else(|| drift(&account.config.organization, &org)) {
        result["warning"] = Value::String(w);
    }
    print(result)
}

fn drift(recorded: &str, current: &str) -> Option<String> {
    (!recorded.trim().is_empty() && !current.trim().is_empty() && !current.eq_ignore_ascii_case(recorded))
        .then(|| format!("This account was added as {recorded}, but the stored key now reports {current}."))
}

fn remove(requested: &str, yes: bool) -> Result<()> {
    let store = secrets::store()?;
    let config = config::load()?;

    let Some((name, acct)) = config.find(requested) else {
        return Err(
            Error::new(ErrorCode::NoAccount, format!("No account named '{requested}'.")).fix("sliplane accounts list")
        );
    };
    let (name, acct) = (name.clone(), acct.clone());

    if !yes {
        if !std::io::stdin().is_terminal() {
            return Err(Error::invalid(format!(
                "Removing '{name}' needs confirmation and there is no terminal to ask on."
            ))
            .fix(format!("sliplane accounts remove {name} --yes")));
        }
        let label = if acct.identity.trim().is_empty() { name.clone() } else { format!("{name} ({})", acct.identity) };
        eprint!("Remove account {label}? [y/N] ");
        let _ = std::io::stderr().flush();
        let mut answer = String::new();
        let _ = std::io::stdin().lock().read_line(&mut answer);
        if !matches!(answer.trim().to_lowercase().as_str(), "y" | "yes") {
            return Err(Error::invalid("Cancelled."));
        }
    }

    {
        let _lock = config::lock()?;
        store.delete(&secrets::account_key(&name))?;
        let mut config = config::load()?;
        config.accounts.shift_remove(&name);
        config::save(&config)?;
    }

    print(obj! {
        "status" => "removed",
        "name" => name,
        "identity" => acct.identity,
        // The key is gone from this machine, not from Sliplane. Anything else holding a copy
        // still works until the key itself is revoked in the dashboard.
        "note" => "The API key was deleted locally. Revoke it at https://sliplane.io/app/team/api if it should stop working everywhere.",
    })
}
