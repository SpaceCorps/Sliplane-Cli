//! The manual an agent reads before its first call. Markdown by default so it can be pasted
//! into a system prompt or a CLAUDE.md; `--json` gives the same rules as data.

use crate::{obj, output};

pub fn print() {
    if output::json() {
        output::write(&obj! {
            "tool" => "sliplane",
            "apiVersion" => API_VERSION,
            "rules" => RULES,
            "exitCodes" => obj! {
                "0" => "ok",
                "1" => "error - unclassified, report and stop",
                "2" => "network - retry once, then stop",
                "3" => "auth_required - stop, surface the remediation to a human",
                "4" => "not_found - do not retry",
                "5" => "rate_limited - back off before retrying",
                "6" => "invalid_input - fix the call",
                "7" => "no_account - run sliplane accounts list",
            },
        });
        return;
    }
    println!("{README}");
}

/// The Sliplane API spec (`https://ctrl.sliplane.io/spec.json`) this build was checked against.
pub const API_VERSION: &str = "0.5.0";

const RULES: &[&str] = &[
    "Always pass --account. There is no default account and no environment variable.",
    "Run 'sliplane accounts list' first if you do not know which accounts exist; ask the human which to use.",
    "On code auth_required, stop and surface the remediation string. Do not retry.",
    "--env replaces the whole environment. Use 'services set-env' to change one variable, or --merge-env to keep the rest.",
    "Secret env values read back empty, so they cannot be copied between services.",
    "A service cannot move between a registry image and a repository build; it must be recreated.",
    "Deletes are irreversible and take no confirmation. Read the resource back before deleting it.",
    "Use --json when you are going to parse the output.",
];

const README: &str = r#"# sliplane - agent operating manual

A CLI over the Sliplane API (v0, spec 0.5.0): servers, services, Postgres databases, buckets,
registry credentials and OAuth clients. Results are YAML on stdout, errors are YAML on stderr,
and `--json` switches both to JSON. Prompts and warnings go to stderr, so stdout is always safe
to parse - including for deletes and other actions that return no body, which print a small
`status:` object.

## Always name the account

`--account` (short `-a`) is required on every command that touches the API. There is no
default, no "current" account and no environment variable - this is deliberate. One machine
holds keys for several Sliplane accounts, and a convenience default is exactly how an agent
working from a summarized transcript deletes a service in the wrong one.

    sliplane accounts list                     # what is configured here
    sliplane projects list -a work

If you do not know which account to use, run `sliplane accounts list` and ask the human. Never
guess when more than one is configured.

### Managing accounts

    sliplane accounts add <name> --api-key <key> [--force]
    printf %s "$KEY" | sliplane accounts add <name> --api-key-stdin
    sliplane accounts list [--check]
    sliplane accounts test <name>
    sliplane accounts remove <name> --yes

`add` calls `me` with the key before storing it, so a typo fails at setup rather than on some
later command. The key goes into the OS keystore (DPAPI on Windows, Keychain on macOS, libsecret
on Linux); only the name, organization id and a label ever reach `config.yaml`. Current API keys
embed their organization (`api_rw_org_...`); `--org-id` exists only for legacy tokens that still
need `X-Organization-ID`.

`list` reports `stored` without touching the network. `--check` calls `me` once per account and
reports `valid`, `rejected` or `unreachable` instead.

`remove` deletes the key from this machine. It does **not** revoke it at Sliplane - say so when
you report the result, because the key keeps working anywhere else it was copied.

Read-only keys can only read: anything that changes state fails with `auth_required` and
`read_only_token` in the detail.

## Reading

    sliplane me -a <account>
    sliplane projects list -a <account>
    sliplane servers list -a <account>
    sliplane servers get --server-id <id> -a <account>
    sliplane services list -a <account> [--project-id <id>]
    sliplane services get --project-id <id> --service-id <id> -a <account>
    sliplane services logs --project-id <id> --service-id <id> -a <account> [--from <unix>] [--to <unix>]
    sliplane services events --project-id <id> --service-id <id> -a <account>
    sliplane postgres list -a <account>
    sliplane buckets list -a <account>

`services list` without `--project-id` walks every project, one API call per project (run in
parallel). Pass the project when you know it.

`services logs` returns at most 500 lines per call, counting back from `--to`. To page back,
repeat with `--to` set to the earliest timestamp you received. Service log and metrics times
are Unix seconds; `postgres logs --since/--until` take RFC 3339.

Ids are the currency of this API: almost every command takes `--project-id`, `--service-id`,
`--server-id`, `--postgres-id` or `--bucket-id`. Get them from the corresponding `list` rather
than constructing them. There is no API to get a single project or bucket by id - use `list`.

## Changing things

    sliplane services create --project-id <id> --name <name> --server-id <id> (--image <url> | --repo <url>) ...
    sliplane services update --project-id <id> --service-id <id> ...
    sliplane services deploy --project-id <id> --service-id <id> [--tag <tag>]
    sliplane services set-env --project-id <id> --service-id <id> --key K --value V [--secret]
    sliplane services delete-env --project-id <id> --service-id <id> --key K
    sliplane services pause|unpause --project-id <id> --service-id <id>

Five behaviours that will otherwise cost you data:

1. **`--env` replaces the entire environment.** It is not a merge - the API takes the array it
   is given, so passing two variables to a service that holds three deletes the third. The CLI
   refuses such an update and names what would be lost. `--merge-env` keeps every variable you
   did not list; `--replace-env` says the deletion is intended. To change one variable use
   `services set-env`, which leaves the rest alone.
2. **Secret values are write-only.** They read back empty, so they cannot be copied from one
   service to another. Sending a secret back with an empty value keeps its stored value, which
   is how `--merge-env` preserves secrets it cannot read.
3. **A service cannot move between a registry image and a repository build.** The API answers
   `409`. The service has to be deleted and recreated; volumes are server-level resources and
   survive, so data on them is not lost.
4. **`services update` carries the current deployment over.** The API replaces the deployment
   object whole, so flags like `--healthcheck` or `--name` on their own resend the current
   branch, Dockerfile, context and registry credentials rather than resetting them to defaults.
   `--branch`, `--dockerfile` and `--docker-context` work on their own; after the update the CLI
   checks the returned service and fails if the change did not take effect.
5. **Network settings and volumes are fixed at creation.** `services update` cannot change
   `--public`, `--protocol` or volumes. `--protocol` applies only to a `--public` service.

Deletes (`services delete`, `projects delete`, `servers delete`, `postgres delete`,
`buckets delete`) are irreversible and ask nothing. Read the resource back first and confirm
with the human that it is the one they meant - including which account it is in. A project must
be empty of services before it can be deleted.

Rate limits: server creation 50/hour, server rescale 10/hour, service creation and deploys
10/minute each, everything else 400/minute. Exceeding one exits with `rate_limited`.

## Errors

Failures print YAML (or JSON) on stderr with a stable `code`, and exit with a matching status:

    0  ok
    1  error          unclassified - report it and stop
    2  network        retry once, then stop
    3  auth_required  stop; give the human the `remediation` string verbatim
    4  not_found      the id does not exist; do not retry
    5  rate_limited   back off before trying again
    6  invalid_input  fix the call
    7  no_account     run `sliplane accounts list`

`detail` carries the HTTP status and the API's own `{code, message}` body. An envelope carries
`remediation` when there is a specific command that fixes the problem. Surface it rather than
guessing, and never retry an `auth_required`.

## Platform notes

**Windows: run from PowerShell or cmd, not Git Bash.** Git Bash rewrites arguments that look
like Unix paths before this program sees them, so `--healthcheck /` arrives as
`C:/Program Files/Git/`. `MSYS_NO_PATHCONV=1` does not prevent it. The CLI detects the result
and refuses, rather than deploying a service whose healthchecks can never pass.

**`--version` belongs to the CLI.** The PostgreSQL major version on `postgres create` is
`--pg-version`."#;
