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
| `SLIPLANE_RELEASES_URL` | Overrides the GitHub releases page for `sliplane update` (and the installers); allows plain http |

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
    logs.rs        services logs --follow: the poll loop and its dedupe
    postgres.rs    postgres allowlist (and its source-IP probe), logs --since, restore's default point
    wait.rs        --wait: poll events until a deploy has an outcome
  presets.rs       the preset catalog: fetch, validate, generate secrets, turn a preset into a request
  timeparse.rs     durations (30m) and RFC 3339, without a date-time crate
  client.rs        blocking HTTP (ureq + rustls), status -> ErrorCode, API error hints
  error.rs         ErrorCode (= exit code) and Error {message, detail, remediation}
  output.rs        YAML by default, JSON with --json, the error envelope, the obj! macro
  account.rs       --account -> Resolved (name, config, key); identity probing of `me`
  config.rs        config.yaml, paths, atomic writes, 0600, the cross-process lock
  secrets.rs       Keychain (security), libsecret (secret-tool), DPAPI, plaintext
  patharg.rs       refuses Windows paths that Git Bash rewrote out of "/"
  readme.rs        agent-readme text, and the API spec version this build targets
  release.rs       `update`: latest release, download, checksum, archive extraction (no API key in reach)
  replace.rs       `update`: stage, verify and swap the running executable
tests/cli.rs       drives the binary against an in-process mock of the API
tests/update.rs    drives `update` on a copy of the binary against a fake releases page
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
push and pull request, plus the installer tests (below). Publishing a GitHub Release builds binaries
for each platform and attaches them to it:

```bash
gh release create v0.3.0 --title v0.3.0 --notes "..."
```

Bump `version` in `Cargo.toml` to match the tag first. `sliplane --version` reports it, and the
installers refuse a binary whose version differs from the tag, so a forgotten bump fails the release
instead of shipping a mislabelled binary.

**The release asset layout is a contract.** `scripts/install.sh`, `scripts/install.ps1` and
`sliplane update` all depend on it, and so do links in the docs:

- `sliplane-vX.Y.Z-<target>.tar.gz` (`.zip` for Windows), holding `sliplane-vX.Y.Z-<target>/sliplane[.exe]`,
  for `aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-unknown-linux-musl`,
  `aarch64-unknown-linux-musl` and `x86_64-pc-windows-msvc`
- `checksums.txt` (`sha256sum` format) covering every archive and both installers
- `install.sh` and `install.ps1`, copied from `scripts/`, so `releases/latest/download/install.sh`
  always matches the latest release

The `checksums` job writes the last two after the archives are up and attests them
(`gh attestation verify <file> --repo SpaceCorps/Sliplane-Cli`); `verify-install` then installs the
release from the public URLs on Linux, macOS and Windows. Releases up to 1.2.0 have no `checksums.txt`,
so the installers cannot install them. Checksums come from the same place as the archives, so they
catch corruption, not a compromised release; the attestation is what ties a file to this workflow.

Installers are tested offline: `sh scripts/test-install.sh <binary>` and
`pwsh scripts/test-install.ps1 <binary>` build a fake release around a binary and run the real
installer against it (CI does this on every push). Both installers honour `SLIPLANE_RELEASES_URL`
for that. Keep `scripts/*.sh` shellcheck-clean, and keep `.gitattributes` forcing LF on them, since a
CRLF `install.sh` fails on `#!/bin/sh\r`.

`sliplane update` (`src/release.rs` finds, downloads, verifies and extracts; `src/replace.rs` swaps the
executable) reads that same contract: the `/releases/latest` redirect, `checksums.txt`, and the one
archive entry `sliplane-vX.Y.Z-<target>/sliplane[.exe]`. Rename or move any of them and `update` breaks
as the installers do; the `tests/update.rs` fake release mirrors the layout, so change both together.
It is explicit and never automatic (see PARITY.md), it talks only to the releases page, and a binary that
reports a version other than the tag's is refused. On Windows the running exe is renamed to
`sliplane.old.exe`; `main` deletes that on the next start.

Both CLIs are called `sliplane`, so the official Sliplane CLI and this one can fight over a PATH
slot. This one installs to `~/.sliplane-cli/bin` and uses a `sliplane-cli` config dir (theirs is
`sliplane`), and the installers warn when another `sliplane` comes first on PATH. The keystore service
name is `sliplane-cli` in both, but entries are `account:<name>` here and `credential_<hex>` there,
so they never collide; keep our `account:` prefix.
