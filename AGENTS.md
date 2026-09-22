# AGENTS.md

Notes for whoever extends this next.

`sliplane` is a Rust CLI over the Sliplane v0 REST API, built to be driven by an LLM agent. It
replaced a .NET global tool of the same name and keeps its interface: the same commands and flags,
YAML-first output, the same error envelope and exit codes, and the same config file and keystore
entries, so existing accounts keep working.

For the manual the *agent* reads, run `sliplane agent-readme` - that text lives in
`src/readme.rs` and is the tool's actual interface for its main audience. This file is for the
human editing the source.

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
them back empty, which the API documents (spec 0.5.0, PATCH service) as "keep the stored value".

**`services update` sends the whole current deployment.** The spec gives the deployment fields
defaults (`branch: main`, `dockerfilePath: Dockerfile`, `autoDeploy: true`), so a PATCH carrying only
`{url}` - which the .NET version sent for `--name` or `--healthcheck` - risks resetting them, and
drops a private image's `registryAuthenticationId`. This has not been observed against the live API;
resending the current values is correct either way.

**`patharg::check` stays on every option that takes a URL path.** Git Bash rewrites `/` into the Git
installation directory before this program starts, and the resulting service deploys happily and
fails every healthcheck with nothing in the output to explain why.

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

**Parallel tests need their own directories.** `tests/cli.rs` names them with an atomic counter. The
clock is not unique enough: two tests once shared a directory and deleted each other's config.

## Sliplane API notes

The spec is at `https://ctrl.sliplane.io/spec.json` (rendered at `https://ctrl.sliplane.io/`, and as
markdown at `/llms.txt`). This build was checked against **0.5.0** (`readme::API_VERSION`); the
server reports its version in the `x-api-version` header. A copy is kept in `spec/sliplane-openapi-0.5.0.json`. When the
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

`me` answers with `authType` (`oauth`/`apikey`/`agent`), `tokenType` (`ro`/`rw`), `organizationId`
and a nested `user`. `account::identity` reads the label from `user.email` and the organization from
`organizationId`, probing alternatives rather than binding to that one shape.

## Releasing

CI (`.github/workflows/ci.yml`) runs fmt, clippy and tests on Linux, macOS and Windows for every
push and pull request. Publishing a GitHub Release builds binaries for each platform and attaches
them to it:

```bash
gh release create v0.3.0 --title v0.3.0 --notes "..."
```

Bump `version` in `Cargo.toml` to match the tag first. `sliplane --version` reports it.
