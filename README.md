# Sliplane CLI

[![Release](https://img.shields.io/github/v/release/SpaceCorps/Sliplane-Cli?color=blue&label=version)](https://github.com/SpaceCorps/Sliplane-Cli/releases/latest)
[![CI](https://github.com/SpaceCorps/Sliplane-Cli/actions/workflows/ci.yml/badge.svg)](https://github.com/SpaceCorps/Sliplane-Cli/actions/workflows/ci.yml)
[![Docs](https://img.shields.io/badge/docs-online-success)](https://spacecorps.github.io/Sliplane-Cli/)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

Manage [Sliplane](https://sliplane.io) servers, services, PostgreSQL databases and S3 buckets from your terminal, or let an AI agent do it. A single static binary built in Rust, covering the whole Sliplane API (v0, OpenAPI spec 0.11.0).

## Install

macOS, Linux, and WSL:

```sh
curl -fsSL https://github.com/SpaceCorps/Sliplane-Cli/releases/latest/download/install.sh | sh
```

Windows (PowerShell):

```powershell
irm https://github.com/SpaceCorps/Sliplane-Cli/releases/latest/download/install.ps1 | iex
```

Open a new terminal and check it worked with `sliplane --version`. No admin rights needed: the installer downloads the binary for your platform, verifies its SHA-256 checksum, and puts it in `~/.sliplane-cli/bin` (Windows: `%LOCALAPPDATA%\sliplane-cli\bin`). To update, run the same command again.

<details>
<summary>Other ways to install, pin a version, or uninstall</summary>

**A specific version.** Pass it to the installer.

```sh
curl -fsSL https://github.com/SpaceCorps/Sliplane-Cli/releases/latest/download/install.sh | sh -s -- 1.3.0
```

```powershell
$env:SLIPLANE_VERSION = '1.3.0'; irm https://github.com/SpaceCorps/Sliplane-Cli/releases/latest/download/install.ps1 | iex
```

**Another directory.** Set `SLIPLANE_INSTALL_DIR` before running the installer. On Windows, `SLIPLANE_NO_PATH=1` leaves your PATH alone; on macOS and Linux the installer asks before it edits your shell profile (`--setup-path` says yes without asking, `--non-interactive` says no).

**By hand.** Download the archive for your platform from the [releases page](https://github.com/SpaceCorps/Sliplane-Cli/releases/latest), check it against `checksums.txt`, and put `sliplane` anywhere on your PATH. Release files are named `sliplane-vX.Y.Z-<target>.tar.gz` (`.zip` on Windows) for `aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl` and `x86_64-pc-windows-msvc`. You can also verify where a file came from with `gh attestation verify <file> --repo SpaceCorps/Sliplane-Cli`.

**From source.** `cargo install --git https://github.com/SpaceCorps/Sliplane-Cli --locked`

**Uninstall.** Run `sliplane accounts remove <name>` for each account (it deletes the stored key), then delete the install directory.

**Using the official `sliplane` CLI too?** Both are called `sliplane`. This installer uses its own directory and the two keep separate settings, but whichever directory comes first on your PATH wins. The installer tells you if another `sliplane` shadows this one.

</details>

## Install with your coding agent

Paste this into Claude Code, Codex, Cursor, or any other coding agent:

```text
Install the Sliplane CLI on my machine, not in a sandbox or container. On macOS, Linux or WSL run `curl -fsSL https://github.com/SpaceCorps/Sliplane-Cli/releases/latest/download/install.sh | sh`; on Windows run `irm https://github.com/SpaceCorps/Sliplane-Cli/releases/latest/download/install.ps1 | iex` in PowerShell. Make sure the install directory is on my PATH (the installer prints what to add if it isn't), confirm it works with `sliplane --version`, then run `sliplane agent-readme` and read it: it is the manual for driving this CLI. Do not ask me for my Sliplane API key or put it anywhere; tell me to run `sliplane login` myself in a terminal. If you can't run commands directly on my machine, don't install it anywhere else. Instead, give me step-by-step instructions for my OS and shell: what to paste into my terminal, how to add it to PATH, and how to check it worked.
```

## Get started

```sh
sliplane login                          # opens the API keys page; the key goes into your OS keystore
sliplane me -a default                  # who am I, and what can this key do?
sliplane projects list -a default
sliplane services list -a default
sliplane services logs --project-id prj_123 --service-id srv_abc -a default
```

Every command that talks to the API takes `--account` (`-a`). That is deliberate: a machine can hold keys for several Sliplane teams, and there is no default that could send a delete to the wrong one.

## Authentication

`sliplane login [name]` opens the API keys page in your browser, asks for the key without echoing it, checks it against the API, and stores it in the OS keystore (macOS Keychain, Windows DPAPI, Linux Secret Service). The name defaults to `default`. In CI or scripts, pipe the key in:

```sh
printf '%s' "$KEY" | sliplane login ci --api-key-stdin
```

## Scripting

Output is YAML. Add `--json` to any command for JSON. Errors go to stderr as a JSON envelope with a stable exit code (see [Machine-Readable Error Envelopes](#machine-readable-error-envelopes)), and the three commands that prompt (`login`, `accounts add`, `accounts remove`) refuse to when stdin is not a terminal and name the flag to use instead.

---

## Highlights

- ⚡ **Sub-3ms Startup**: Compiled as a native static binary with zero runtime dependencies. Executes in ~1–3 ms (compared to ~75 ms for managed runtimes).
- 🔐 **OS Keystore Integration**: API keys live in native OS vaults (macOS Keychain, Linux Secret Service / Keyutils, Windows DPAPI), never in a config file.
- 🌐 **Full 65-Endpoint API Coverage**: Complete command surface across compute servers, web services, managed PostgreSQL databases, S3 object storage buckets, and OAuth clients.
- 🤖 **AI Agent Native**: Machine-readable `--json` output, standardized error envelopes with stable exit codes, and explicit `llms.txt` agent guidance.
- 🛡️ **Safety Guardrails**: Strict account scoping protects multi-tenant setups; safeguards prevent accidental deletion of environment variables or secrets.

---

## Command Reference

Every command that accesses the API accepts `--account <name>` (short `-a <name>`).

### Identity & Authentication

| Command | Description |
|:---|:---|
| `sliplane login [name]` | Interactively authenticate with your API token and save it to the OS keystore |
| `sliplane me` | Query identity, organization and the token's scopes (e.g. `services:write`, `secrets:read`) |
| `sliplane accounts list [--check]` | List configured accounts (pass `--check` to verify token validity) |
| `sliplane accounts test <name>` | Test credentials for a specific account |
| `sliplane accounts add <name>` | Manually add an account and API key to the keystore |
| `sliplane accounts remove <name>` | Remove an account and purge its key from the local keystore |

### Projects & Servers

| Command | Description |
|:---|:---|
| `sliplane projects list` | List all projects |
| `sliplane projects create --name <name>` | Create a new project |
| `sliplane projects update --project-id <id> --name <name>` | Rename a project |
| `sliplane projects delete --project-id <id>` | Delete an empty project |
| `sliplane servers list` | List all servers with instance sizes and locations |
| `sliplane servers get --server-id <id>` | Inspect server details and resource allocations |
| `sliplane servers create --name <n> --instance-type <t> --location <loc>` | Provision a new compute server |
| `sliplane servers rescale --server-id <id> --instance-type <t>` | Scale server instance size up |
| `sliplane servers rescale-disk --server-id <id> --disk-size <gb>` | Expand server storage volume disk size |
| `sliplane servers metrics --server-id <id> [--range 1h \| --from <ts> --to <ts>]` | Query CPU, memory and disk utilization metrics |
| `sliplane servers volumes --server-id <id>` | List attached persistent storage volumes |
| `sliplane servers rename-volume --server-id <id> --volume-id <v> --name <n>` | Rename a volume (services are not redeployed) |

### Services & Deployments

| Command | Description |
|:---|:---|
| `sliplane services list --project-id <id>` | List services within a project |
| `sliplane services get --project-id <p> --service-id <s>` | Fetch detailed service configuration |
| `sliplane services create --project-id <p> (--image <img> \| --repo <r>)` | Deploy a new container service |
| `sliplane services update --project-id <p> --service-id <s>` | Update service deployment settings (see safeguards below) |
| `sliplane services deploy --project-id <p> --service-id <s>` | Trigger immediate redeployment |
| `sliplane services logs --project-id <p> --service-id <s> [--to <ts>]` | Read container logs (paginated, 500 lines per call) |
| `sliplane services pause / unpause` | Pause or resume container execution |
| `sliplane services set-env --project-id <p> --service-id <s> --key <k> --value <v>` | Safely set or update a single environment variable |
| `sliplane services delete-env --project-id <p> --service-id <s> --key <k>` | Remove a single environment variable |
| `sliplane services add-domain / remove-domain` | Configure custom domain routing |
| `sliplane services rotate-deploy-hook --project-id <p> --service-id <s>` | New deploy hook URL; the old one stops working immediately (needs `secrets:read`) |
| `sliplane services attach-volume --project-id <p> --service-id <s> --volume <id-or-name>:/path` | Mount a volume and redeploy |
| `sliplane services update-volume --project-id <p> --service-id <s> --volume-id <v> --mount-path /path` | Change a mount path and redeploy |
| `sliplane services detach-volume --project-id <p> --service-id <s> --volume-id <v>` | Unmount a volume and redeploy (the data is kept) |

### Managed PostgreSQL & Storage

| Command | Description |
|:---|:---|
| `sliplane postgres list` | List all managed PostgreSQL instances |
| `sliplane postgres get --postgres-id <id>` | Retrieve database connection endpoints and credentials |
| `sliplane postgres create --name <n> --instance-type <t> --region <r>` | Provision a managed PostgreSQL instance |
| `sliplane postgres update --postgres-id <id> --ip-allow <cidr>=<label>` | Update instance configuration or IP allowlist |
| `sliplane postgres rotate-credentials --postgres-id <id>` | Rotate database access password |
| `sliplane postgres slow-queries / top-queries` | Inspect query performance and call frequency |
| `sliplane postgres metrics --postgres-id <id> [--range 1h \| --from <ts> --to <ts>]` | CPU, memory, disk and connection history |
| `sliplane buckets list` | List S3-compatible object storage buckets |
| `sliplane buckets create --name <n> --region <r>` | Create an S3 object storage bucket |
| `sliplane buckets keys --bucket-id <id>` | List S3 access keys |
| `sliplane buckets create-key --bucket-id <id> --name <label>` | Generate scoped S3 access key and secret |

### SSH Access & Pricing

| Command | Description |
|:---|:---|
| `sliplane ssh-keys list` | List your SSH public keys |
| `sliplane ssh-keys create --name <n> (--public-key <key> \| --public-key-file <path>) [--expires-at <rfc3339>]` | Register a public key (optionally self-deleting, 5 minutes to 30 days) |
| `sliplane ssh-keys delete --key-id <id>` | Delete a key (detach it from every service first) |
| `sliplane services ssh-keys --project-id <p> --service-id <s>` | List the keys attached to a service |
| `sliplane services attach-ssh-key --project-id <p> --service-id <s> --key-id <k>` | Attach a key; prints the host, port and username to connect with |
| `sliplane services detach-ssh-key --project-id <p> --service-id <s> --key-id <k>` | Detach a key |
| `sliplane pricing` | Standard prices for servers, Postgres and object storage (excl. tax) |

---

## Output Formats & AI Agent Readiness

Commands format stdout as clean YAML by default. Pass `--json` when parsing outputs with `jq`, Python, or LLM tool-calling loops:

```bash
# Parse status with jq
sliplane services get --project-id prj_123 --service-id srv_abc -a default --json | jq -r .status
```

### Machine-Readable Error Envelopes

Errors are output to `stderr` as structured envelopes with stable exit codes:

```json
{
  "code": "auth_required",
  "message": "Token expired or missing.",
  "remediation": "Run 'sliplane login default' to authenticate."
}
```

| Exit Code | Error Symbol | Handling Directive |
|:---|:---|:---|
| `0` | `ok` | Command succeeded |
| `1` | `error` | General failure; unclassified |
| `2` | `network` | Network connectivity failure; retry once |
| `3` | `auth_required` | Unauthenticated; surface remediation to user |
| `4` | `not_found` | Resource does not exist; do not retry |
| `5` | `rate_limited` | API rate limit reached; back off before retrying |
| `6` | `invalid_input` | Parameter schema validation failed |
| `7` | `no_account` | Requested account not configured in keystore |

### Agent Manuals

Inspect built-in agent guides directly from the CLI:

```bash
sliplane agent-readme          # Human-readable markdown guide
sliplane agent-readme --json   # Machine-readable rules and schemas
```

For web-based LLMs and crawlers, refer to [llms.txt](https://spacecorps.github.io/Sliplane-Cli/llms.txt) and [llms-full.txt](https://spacecorps.github.io/Sliplane-Cli/llms-full.txt).

---

## Important Operational Behaviors

1. **Environment Variables (`--env` vs `--merge-env`)**:
   - Passing `--env` to `services update` replaces the entire environment array. The CLI warns and aborts if unmentioned variables would be dropped.
   - Use `--merge-env` to preserve unmentioned variables, or `--replace-env` if removal was intentional.
   - Use `services set-env` to change a single variable safely.
2. **Preserving Secrets**:
   - In Sliplane, secret values are write-only and read back as empty strings. `--merge-env` safely retains existing secrets without exposing or overwriting them.
   - API keys have per-area permissions, and **Secrets: Read** is one of them. Without it *every* environment value and database password reads back empty, not only the secret ones, so `--merge-env` would overwrite plain variables with nothing. The CLI refuses that (`auth_required`) and points you to `services set-env` or a key with Secrets: Read. `sliplane me` lists a key's scopes, and `accounts add` / `accounts test` warn when `secrets:read` is missing.
3. **Carrying Over Deployments**:
   - `services update` automatically carries over existing deployment parameters (branch, Dockerfile path, build context, path filters) so they do not inadvertently revert to defaults.
4. **Volume Resolution**:
   - `--volume my-data:/data` automatically binds to an existing server volume named `my-data` rather than creating duplicates.
5. **Windows Shells**:
   - Run from PowerShell or `cmd.exe`. Avoid Git Bash / MSYS, which automatically rewrites Unix-style path arguments (`/health` &rarr; `C:/Program Files/Git/...`).

---

## Configuration & Environment Variables

| Variable | Description | Default |
|:---|:---|:---|
| `SLIPLANE_CONFIG_DIR` | Custom directory path for `config.yaml` | `~/.config/sliplane` (or OS equivalent) |
| `SLIPLANE_SECRET_STORE` | Force specific credential store: `dpapi`, `keychain`, `libsecret`, `plaintext` | Auto-detected |
| `SLIPLANE_ALLOW_PLAINTEXT_STORE` | Set to `1` to allow a chmod 0600 file store on headless Linux without DBus/SecretService | `0` |
| `SLIPLANE_API_URL` | Override the API base URL (useful for integration testing against mocks) | `https://ctrl.sliplane.io/v0` |

---

## Contributing & License

Contributions are welcome! Please submit issues and pull requests to [SpaceCorps/Sliplane-Cli](https://github.com/SpaceCorps/Sliplane-Cli).

Dual-licensed under the [MIT License](LICENSE).
