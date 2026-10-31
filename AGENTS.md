# AGENTS.md

Notes for whoever extends this next.

`sliplane` is a Rust CLI over the Sliplane v0 REST API, built to be driven by an LLM agent. It
replaced a .NET global tool of the same name and keeps its interface: the same commands and flags,
YAML-first output, the same error envelope and exit codes, and the same config file and keystore
entries, so existing accounts keep working.

For the manual the *agent* reads, run `sliplane agent-readme` - that text lives in
`src/readme.rs` and is the tool's actual interface for its main audience. This file is for the
human editing the source. `PARITY.md` records how this CLI compares with the official Sliplane CLI:
what was ported, what was declined and why, and how the rest would be built.

## Commands

```bash
cargo build --release              # target/release/sliplane
cargo test                         # unit tests + tests/cli.rs against a mock API
cargo clippy --all-targets
cargo fmt
cargo install --path . --locked    # put it on PATH
```

Use a throwaway config directory when testing so you never touch real credentials:

```bash
export SLIPLANE_CONFIG_DIR=$(mktemp -d) SLIPLANE_SECRET_STORE=plaintext
```

| Variable | Effect |
| --- | --- |
| `SLIPLANE_CONFIG_DIR` | Overrides the config/secrets location |
| `SLIPLANE_SECRET_STORE` | Forces a backend: `dpapi`, `keychain`, `libsecret`, `plaintext` |
| `SLIPLANE_ALLOW_PLAINTEXT_STORE=1` | Permits the plaintext fallback where no keystore exists |
| `SLIPLANE_API_URL` | Overrides the API base URL - how `tests/cli.rs` points at its mock |
| `SLIPLANE_PRESETS_URL` | Overrides the preset catalog URL, the same way |
| `SLIPLANE_S3_PAGE_SIZE` | Entries per `s3 ls` request (1-1000). Only the tests set it, to see a listing continue |

There is deliberately **no** `SLIPLANE_API_KEY`. See the invariants below.

## Layout

```
src/
  main.rs          arg parsing, the --json pre-scan, clap errors -> invalid_input envelopes
  cli.rs           the whole command tree (clap derive); help text lives here
  commands/
    mod.rs         dispatch, and every command that is one request and a print
    accounts.rs    accounts add|list|test|remove
    services.rs    services *, including the create/update logic
    presets.rs     presets list|show|deploy
    s3.rs          s3 connect|disconnect|ls|upload|download|rm: the stored key, and the file commands
    logs.rs        services logs --follow: the poll loop and its dedupe
    wait.rs        --wait: poll events until a deploy has an outcome
  presets.rs       the preset catalog: fetch, validate, generate secrets, turn a preset into a request
  s3.rs            the S3 client: its own HTTP agent, listing/XML parsing, S3 error -> ErrorCode
  sigv4.rs         AWS Signature V4 (header-based) and the vectors it is tested against
  timeparse.rs     durations (30m) and RFC 3339, without a date-time crate
  client.rs        blocking HTTP (ureq + rustls), status -> ErrorCode, API error hints
  error.rs         ErrorCode (= exit code) and Error {message, detail, remediation}
  output.rs        YAML by default, JSON with --json, the error envelope, the obj! macro
  account.rs       --account -> Resolved (name, config, key); identity probing of `me`
  config.rs        config.yaml, paths, atomic writes, 0600, the cross-process lock
  secrets.rs       Keychain (security), libsecret (secret-tool), DPAPI, plaintext
  patharg.rs       refuses Windows paths that Git Bash rewrote out of "/"
  readme.rs        agent-readme text, and the API spec version this build targets
tests/cli.rs       drives the binary against an in-process mock of the API
```

## Why it is built this way

**Blocking HTTP, no async runtime.** A CLI makes one to a few requests. Tokio would cost more in
startup than it saves; `services list` and `accounts list --check` fan out with scoped threads.

**The Keychain goes through `/usr/bin/security`, not the Security framework.** Items the .NET
version created list `security` as a trusted app, so reading them through it never prompts. A
native call would prompt for every existing item, and again after every rebuild of an unsigned
binary, because the Keychain trusts a binary by its code signature.

**Responses stay `serde_json::Value`.** The API adds fields within v0 without notice, and this CLI
prints what it gets. Typed structs would drop new fields or fail on new enum values. Request
bodies are built as `Value` too, with `obj!`, which keeps key order stable.

## How a command is wired

1. Add the variant (with doc comments as help text) to the right enum in `src/cli.rs`. API
   commands flatten `Account` (or a struct that contains it, like `ServiceRef`).
2. Handle it in `src/commands/` - `client(&a)?` resolves the account and returns a `Client`.
   Print an API response with `print(v)`; for 202/204 endpoints use `done(v, "status", fields)`
   so stdout is never empty and stays valid JSON.
3. Fail by returning `Err(Error::invalid(..))` (or another code) with `.fix(..)` when a specific
   command fixes it. Never `process::exit`, never print an error yourself.
4. Add the row to `README.md` **and** to `src/readme.rs`. A command an agent cannot discover in
   `agent-readme` effectively does not exist.
5. Add a case to `tests/cli.rs` that asserts on the request body the mock received.

## Invariants — do not casually revert these

**`--account` is required everywhere, and there is no `SLIPLANE_API_KEY`.** No stored default, no
environment variable, no fallback when only one account is configured. This machine holds keys for
several Sliplane accounts; a convenience default is exactly how an agent working from a summarized
transcript deletes a service in the wrong organization — the call succeeds, and nothing in the
output says it went somewhere you did not mean. `account::resolve` is the only way a key
enters the process. `SLIPLANE_API_URL` changes where requests go, never which key they carry.

**Secrets never touch `config.yaml`.** Config holds the account name, the organization, a label and
a date. The API key goes through `secrets::Store` into the OS keystore. When no keystore is available
the tool refuses to start rather than silently writing a file — `SLIPLANE_ALLOW_PLAINTEXT_STORE=1`
is the explicit opt-out, and that backend warns on every read.

**Exit codes are the `code:` field.** `error::ErrorCode` is both, so an agent can branch on either. They
are documented in `agent-readme` and in the README; changing a number breaks callers silently.

**`--env` replaces the whole environment, and the CLI refuses to do it by accident.** The API takes
the array it is given. `services update` names what would be lost and requires `--merge-env` or `--replace-env`;
`services set-env` is the single-variable path. Secret values read back empty; `--merge-env` sends
them back empty, which the API documents (PATCH service) as "keep the stored value". **Plain values
read back empty too when the key lacks `secrets:read`** (spec 0.10.0+), and are *not* kept when sent
empty, so `env_for_update` asks `me` for the key's scopes and refuses a merge that would carry a plain
variable over with such a key. A key with no `scopes` field is a legacy key with full access.

**`services update` sends the whole current deployment.** The spec gives the deployment fields
defaults (`branch: main`, `dockerfilePath: Dockerfile`, `autoDeploy: true`), so a PATCH carrying only
`{url}` - which the .NET version sent for `--name` or `--healthcheck` - risks resetting them, and
drops a private image's `registryAuthenticationId`. This has not been observed against the live API;
resending the current values is correct either way.

**`patharg::check` stays on every option that takes a URL path.** Git Bash rewrites `/` into the Git
installation directory before this program starts, and the resulting service deploys happily and
fails every healthcheck with nothing in the output to explain why.

**The S3 key pair and the API key never meet.** `s3.rs` is handed an endpoint, a signing region and an S3
key pair, and has never heard of the API key; `commands/s3.rs` is the only place that holds both. The S3
agent follows no redirects (a signed request must not be replayed somewhere it was not signed for), has no
global timeout (a 5 GiB transfer takes as long as it takes), and refuses plain HTTP except to a loopback
host. The pair lives in the keystore as JSON under `secrets::s3_key(account, bucketId)` and nowhere else.
Nothing makes a key as a side effect: only `s3 connect` does, because creating a credential is the human's
decision. There is deliberately no `SLIPLANE_S3_ACCESS_KEY_ID` override, for the reason there is no
`SLIPLANE_API_KEY`.

## Traps in this codebase

**clap reserves the id `version`.** A field named `version` panics at startup ("Argument names must
be unique") - `postgres create` uses `pg_version` with `--pg-version`. `cli::tests::command_tree_is_valid`
runs clap's `debug_assert` over the whole tree; keep it.

**The output mode is global.** `main` sets it from the raw args before clap parses anything, because
a parse error is rendered as an envelope too. If you add another output switch, pre-scan it the same
way or errors will come out in the wrong format.

**The config lock is not reentrant.** `accounts add` and `accounts remove` hold it around the secret
write and the config write together. Keep every network call outside it - `accounts add` verifies
the key against `me` *before* taking it.

**`secrets::store()` caches the backend for the process.** Changing `SLIPLANE_SECRET_STORE`
mid-process has no effect; a test that wants another backend needs another process (which is what
`tests/cli.rs` does anyway).

**Prompts and warnings go to stderr.** `accounts add` prompts through `rpassword` (the terminal),
`accounts remove` asks on stderr, and both refuse to prompt when stdin is not a terminal, so a
scripted call fails with a remediation instead of hanging.

**`accounts remove` does not revoke anything.** It deletes the local copy of the key. The key keeps
working anywhere else it was pasted until it is revoked in the Sliplane dashboard, and the command
says so in its output. Do not reword that into something that sounds like revocation.

**Names are resolved once, before dispatch, and ids cost nothing.** `--service`, `--project`, `--server`,
`--postgres` and `--bucket` sit beside the `--*-id` flags (which are `default_value = ""` rather than
required, so `check()` reports a missing one as `invalid_input` before any account or network use).
`Services::service_mut` and its siblings in `cli.rs` hand `run` the reference a command carries; they
have no wildcard arm on purpose, so a new command must say whether it names a resource. `resolve.rs`
matches an id first, then an exact case-sensitive name, and refuses a shared name with both ids
instead of picking. Keep it that way: with no delete confirmation, a wrong guess is a wrong delete.

**The preset catalog is not the Sliplane API.** `presets.rs` fetches `sliplane.io/api/presets` with its
own bare HTTP agent, so the API key can never reach it, and `presets list|show` take no `--account`.
It has no version field, so `parse` rejects anything it does not recognise instead of guessing: the
catalog decides which image runs with which secrets. `SLIPLANE_PRESETS_URL` points it at a mock, the way
`SLIPLANE_API_URL` does for the API. A preset's generated passwords are write-only once deployed, so
`presets deploy` prints them, and does so even when `--wait` fails afterwards.

**`--wait` reads events, not `status`.** Right after a redeploy is accepted, `status` still shows the
previous deploy's value. `wait::known_events` has to be read *before* the deploy is triggered, or the
deploy's own events are mistaken for old ones. The poll interval is a constant, so a test that needs a
second poll takes three seconds.

**Never follow logs without a bound in a test or an agent flow.** `--follow` with no `--for` runs until
interrupted. The tests use `--for 1s`.

**S3 paths are signed as they are sent.** S3 does not normalise or double-encode the path, so `sigv4::sign`
takes the canonical URI already encoded and `sigv4::uri_encode` is the only encoder; the query string that is
sent is the very text that was signed. `Host` is set by hand to the endpoint minus its default port, because
that exact string is both signed and sent. The payload hash of a PUT is a real SHA-256 from a first pass over
the file, so a file that changes between the passes is refused by the server instead of being stored torn.

**`ureq` decompresses `Content-Encoding: gzip` whatever it was asked, and then drops `Content-Length`.** S3 always
sends a length, so `ObjectStore::get` treats its absence as that case and refuses: a download must be the
stored bytes or an error. Do not "fix" it by accepting the decoded body.

**The S3 tests have a real-server mode.** `cargo test s3_` runs against an in-process S3 that verifies every
signature with code of its own. Point the same scenario at a real server with
`SLIPLANE_TEST_S3_ENDPOINT=http://127.0.0.1:19000 SLIPLANE_TEST_S3_BUCKET=<existing bucket>
SLIPLANE_TEST_S3_ACCESS_KEY=... SLIPLANE_TEST_S3_SECRET_KEY=...` (use a secret that differs from the access
key, or the "secret was printed" check trips on a coincidence); add `SLIPLANE_TEST_S3_MANY=1` for the 1,010-key
pagination test. `minio/minio` is gone from Docker Hub; `bitnamilegacy/minio` with `MINIO_DEFAULT_BUCKETS` works.
The tests turn `sha2` up to `opt-level = 3` in the dev profile, or hashing 20 MiB several times takes minutes.

**Parallel tests need their own directories.** `tests/cli.rs` names them with an atomic counter. The
clock is not unique enough: two tests once shared a directory and deleted each other's config.

## Sliplane API notes

The spec is at `https://ctrl.sliplane.io/spec.json` (rendered at `https://ctrl.sliplane.io/`, and as
markdown at `/llms.txt`). This build was checked against **0.11.0** (`readme::API_VERSION`); the
server reports its version in the `x-api-version` header. A copy is kept in `spec/sliplane-openapi-0.11.0.json`. When the
live version moves, fetch it, diff it against that copy, bring `src/cli.rs` up to date, and replace
the copy.

Base URL `https://ctrl.sliplane.io/v0/`, `Authorization: Bearer <key>`. Keys embed their
organization; `X-Organization-ID` is gone from the spec and is only sent when an account has a
legacy `--org-id`. Read-only keys get `403 read_only_token` on anything but GET.

Error bodies are `{code, message}`. `client::status_error` maps the status to an `ErrorCode` and
uses `code` to pick the remediation for 401/403 (`token_revoked`, `read_only_token`,
`api_access_disabled`, `feature_not_enabled`). `client::hint` adds text for the 409 on an
image/repository switch, the 409 on deleting a non-empty project, and the 400 for a missing
deployment.

No list endpoint paginates. Service logs return at most 500 lines, counting back from `to`. Rate
limits: server create 50/h, server rescale 10/h, service create and deploy 10/min, else 400/min.
There is no GET for a single project or bucket, and no server rename.

**Server autoscaling is deliberately not exposed.** Spec 0.11.0 turned `POST /servers/{id}` into
"Update a server" with an optional `autoscaling` policy (and made `instanceType` optional). `servers
rescale` still sends only `instanceType`/`billingCycle`, which stays valid, and `servers get` prints the
policy as the API returns it. Changing it from the CLI was declined; if that changes, it needs its own
command, because the API wants autoscaling sent separately from `instanceType`/`billingCycle`.

`me` answers with `authType` (`oauth`/`apikey`/`agent`), `tokenType` (`ro`/`rw`), `organizationId`,
`scopes` (e.g. `services:write`, `secrets:read`; absent on a legacy key) and a nested `user`. `account::identity` reads the label from `user.email` and the organization from
`organizationId`, probing alternatives rather than binding to that one shape.

## Releasing

CI (`.github/workflows/ci.yml`) runs fmt, clippy and tests on Linux, macOS and Windows for every
push and pull request. Publishing a GitHub Release builds binaries for each platform and attaches
them to it:

```bash
gh release create v0.3.0 --title v0.3.0 --notes "..."
```

Bump `version` in `Cargo.toml` to match the tag first. `sliplane --version` reports it.
