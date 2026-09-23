//! `sliplane login`. Authenticates with Sliplane via API token, opening the dashboard
//! in the browser if interactive, verifying against `/me`, and storing the key in the OS keystore.

use std::io::{BufRead, IsTerminal, Write};

use crate::account::identity;
use crate::cli::Login;
use crate::client::Client;
use crate::commands::print;
use crate::config::{self, AccountConfig};
use crate::error::{Error, Result};
use crate::obj;
use crate::secrets;

const API_KEYS_URL: &str = "https://sliplane.io/app/team/api";

pub fn run(args: Login) -> Result<()> {
    let Login { name, api_key, api_key_stdin, org_id, no_browser, force, no_verify } = args;

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
        return Err(Error::invalid(format!("An account named '{existing}' already exists."))
            .fix(format!("Use --force to replace its key: sliplane login {existing} --force")));
    }
    let name = existing.clone().unwrap_or(name);

    let key = if api_key_stdin {
        read_stdin_key()?
    } else if let Some(k) = api_key.map(|k| k.trim().to_string()).filter(|k| !k.is_empty()) {
        k
    } else {
        prompt_login_key(&name, no_browser)?
    };

    let org_id = org_id.map(|o| o.trim().to_string()).unwrap_or_default();

    let (mut ident, mut org) = (String::new(), String::new());
    if !no_verify {
        // Verify before storing: a key that does not work is worse than no account at all.
        let me = Client::new(&key, Some(&org_id)).get("me")?;
        ident = identity::describe(&me);
        org = identity::organization(&me);
    }

    {
        let _lock = config::lock()?;
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

    if std::io::stderr().is_terminal() {
        if !ident.is_empty() && !org.is_empty() {
            eprintln!("Successfully logged in as {ident} ({org}) to account '{name}'.");
        } else if !ident.is_empty() {
            eprintln!("Successfully logged in as {ident} to account '{name}'.");
        } else {
            eprintln!("Successfully logged in to account '{name}'.");
        }
    }

    print(obj! {
        "status" => "logged_in",
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

fn prompt_login_key(name: &str, no_browser: bool) -> Result<String> {
    if !std::io::stdin().is_terminal() {
        return Err(Error::invalid("No API key given and no terminal to prompt on.")
            .fix(format!("pbpaste | sliplane login {name} --api-key-stdin")));
    }

    eprintln!("To log in, copy or create an API token from Sliplane:");
    eprintln!("  {API_KEYS_URL}\n");

    if !no_browser {
        eprintln!("Opening {API_KEYS_URL} in your browser...");
        open_browser(API_KEYS_URL);
    }

    let _ = std::io::stderr().flush();

    loop {
        let key = rpassword::prompt_password(format!("Paste your Sliplane API token for '{name}': "))
            .map_err(|e| Error::other("Could not read the API key.").detail(e.to_string()))?;
        let key = key.trim().to_string();
        if !key.is_empty() {
            return Ok(key);
        }
        eprintln!("Token cannot be empty. Paste your API token from {API_KEYS_URL}");
    }
}

fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(url).spawn();
    }
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("cmd").args(["/C", "start", "", url]).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    }
}
