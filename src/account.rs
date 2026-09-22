//! The account is always explicit - no stored default, no environment variable, no implicit
//! fallback when only one account is configured.
//!
//! The failure this prevents is an agent working from a summarized transcript deleting a
//! service in the wrong organization: the call succeeds, and nothing in the output says it went
//! to the account you did not mean. [`resolve`] is the only way a key enters the process.

use serde_json::Value;

use crate::client::Client;
use crate::config::{self, AccountConfig, Config};
use crate::error::{Error, ErrorCode, Result};
use crate::secrets;

pub struct Resolved {
    pub name: String,
    pub config: AccountConfig,
    pub api_key: String,
}

impl Resolved {
    pub fn org_id(&self) -> Option<&str> {
        Some(self.config.org_id.as_str()).filter(|s| !s.trim().is_empty())
    }

    pub fn client(&self) -> Client {
        Client::new(&self.api_key, self.org_id())
    }
}

pub fn resolve(requested: Option<&str>) -> Result<Resolved> {
    let config = config::load()?;

    let Some(requested) = requested.map(str::trim).filter(|s| !s.is_empty()) else {
        return Err(Error::new(ErrorCode::NoAccount, "No account specified. Pass --account <name>.")
            .detail(describe(&config))
            .fix("sliplane accounts list"));
    };

    let Some((name, account)) = config.find(requested) else {
        return Err(Error::new(ErrorCode::NoAccount, format!("No account named '{requested}'."))
            .detail(describe(&config))
            .fix("sliplane accounts list"));
    };

    let key = secrets::store()?.get(&secrets::account_key(name))?;

    // A config entry with no key is an account that was half-removed, or one whose keystore
    // entry was cleared behind our back. Either way it cannot be used and re-adding is the fix.
    let Some(api_key) = key.filter(|k| !k.trim().is_empty()) else {
        return Err(Error::new(ErrorCode::AuthRequired, format!("Account '{name}' has no stored API key."))
            .detail("The config entry exists but the keystore has nothing under it.")
            .fix(format!("sliplane accounts add {name} --api-key <key>")));
    };

    Ok(Resolved { name: name.clone(), config: account.clone(), api_key })
}

fn describe(config: &Config) -> String {
    if config.accounts.is_empty() {
        return "No accounts are configured yet. Run 'sliplane accounts add <name> --api-key <key>'.".into();
    }
    let listed: Vec<String> = config
        .sorted()
        .into_iter()
        .map(|(k, v)| if v.identity.trim().is_empty() { k.clone() } else { format!("{k} ({})", v.identity) })
        .collect();
    format!("Configured accounts: {}", listed.join(", "))
}

/// Reads the two things worth remembering from `me`: who the key belongs to, and which
/// organization it is scoped to.
///
/// The response differs between a scoped key and a legacy token, so this probes the fields that
/// have been seen rather than binding to one shape. An unrecognised response yields an empty
/// label, which is cosmetic: the account still works.
pub mod identity {
    use super::Value;

    const CANDIDATES: &[&str] = &["email", "userEmail", "user_email", "name", "userName", "user_name", "login", "id"];

    /// A short human label - today the account email, as `me.user.email`.
    pub fn describe(me: &Value) -> String {
        if !me.is_object() {
            return String::new();
        }
        first_string(me, CANDIDATES).or_else(|| nested(me, "user", CANDIDATES)).unwrap_or_default()
    }

    /// The organization the key reports. Observed, unlike the `--org-id` a legacy token sends.
    pub fn organization(me: &Value) -> String {
        if !me.is_object() {
            return String::new();
        }
        first_string(me, &["organizationId", "organization_id", "orgId"])
            .or_else(|| nested(me, "organization", &["id", "name"]))
            .unwrap_or_default()
    }

    fn nested(root: &Value, property: &str, names: &[&str]) -> Option<String> {
        root.get(property).filter(|c| c.is_object()).and_then(|c| first_string(c, names))
    }

    fn first_string(v: &Value, names: &[&str]) -> Option<String> {
        names
            .iter()
            .filter_map(|n| v.get(*n).and_then(Value::as_str))
            .find(|s| !s.trim().is_empty())
            .map(str::to_string)
    }

    #[cfg(test)]
    mod tests {
        use serde_json::json;

        #[test]
        fn reads_scoped_key_shape() {
            let me = json!({"authType": "api_key", "organizationId": "org_1", "user": {"email": "a@b.c", "name": "A"}});
            assert_eq!(super::describe(&me), "a@b.c");
            assert_eq!(super::organization(&me), "org_1");
        }

        #[test]
        fn degrades_to_empty() {
            assert_eq!(super::describe(&json!([1])), "");
            assert_eq!(super::organization(&json!({"x": 1})), "");
            assert_eq!(super::organization(&json!({"organization": {"id": "o"}})), "o");
        }
    }
}
