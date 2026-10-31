//! `--wait`: the API accepts a deploy with a 202 and finishes it later, so "accepted" does not
//! mean "running". This polls the service's events until one reports the outcome.
//!
//! The outcome is the first deploy event that was not already there before the deploy was
//! triggered; older events from earlier deploys are in `known` and ignored. Events are used rather
//! than the service's `status`, which still reads `live` (or `failed`) from the previous deploy
//! for a moment after a redeploy is accepted.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::client::{Client, seg};
use crate::error::{Error, Result};

const POLL: Duration = Duration::from_secs(3);
pub const DEFAULT_TIMEOUT_SECS: u64 = 600;

const SUCCESS: &str = "service_deploy_success";
const FAILURE: &[&str] = &["service_deploy_failed", "service_build_failed", "service_deploy_cancelled"];

fn events_path(project_id: &str, service_id: &str) -> String {
    format!("projects/{}/services/{}/events", seg(project_id), seg(service_id))
}

fn id(event: &Value) -> Option<&str> {
    event.get("id").and_then(Value::as_str)
}

/// The ids of the events a service has right now. Take this before triggering the deploy.
pub fn known_events(client: &Client, project_id: &str, service_id: &str) -> Result<HashSet<String>> {
    let events = client.get(&events_path(project_id, service_id))?;
    Ok(events.as_array().into_iter().flatten().filter_map(id).map(str::to_string).collect())
}

/// Blocks until the deploy succeeds (returning the fields to print), fails, or `timeout` passes.
pub fn until_deployed(
    client: &Client,
    project_id: &str,
    service_id: &str,
    known: &HashSet<String>,
    timeout: Duration,
) -> Result<Value> {
    let started = Instant::now();
    let inspect =
        format!("sliplane services events --project-id {project_id} --service-id {service_id} --account <account>");
    loop {
        let events = client.get(&events_path(project_id, service_id))?;
        let fresh = events.as_array().into_iter().flatten().filter(|e| id(e).is_none_or(|i| !known.contains(i)));
        let mut outcome: Option<&Value> = None;
        for e in fresh {
            let kind = e.get("type").and_then(Value::as_str).unwrap_or("");
            if FAILURE.contains(&kind) {
                let message = e.get("message").and_then(Value::as_str).unwrap_or("no message");
                return Err(Error::other(format!("The deploy did not succeed: {message} ({kind}).")).detail(e.to_string()).fix(
                    format!(
                        "Read the logs with: sliplane services logs --project-id {project_id} --service-id {service_id} --account <account>"
                    ),
                ));
            }
            if kind == SUCCESS {
                outcome = Some(e);
            }
        }
        if let Some(event) = outcome {
            return Ok(json!({ "status": "live", "waitedSeconds": started.elapsed().as_secs(), "event": event }));
        }

        let remaining = timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(Error::other(format!(
                "No deploy outcome after {} seconds. The deploy may still finish.",
                timeout.as_secs()
            ))
            .fix(inspect));
        }
        std::thread::sleep(POLL.min(remaining));
    }
}
