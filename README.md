# Sliplane CLI

[![Release](https://img.shields.io/github/v/release/SpaceCorps/Sliplane-Cli?color=blue&label=version)](https://github.com/SpaceCorps/Sliplane-Cli/releases/latest)
[![CI](https://github.com/SpaceCorps/Sliplane-Cli/actions/workflows/ci.yml/badge.svg)](https://github.com/SpaceCorps/Sliplane-Cli/actions/workflows/ci.yml)
[![Docs](https://img.shields.io/badge/docs-online-success)](https://spacecorps.github.io/Sliplane-Cli/)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

A blazing fast, native command-line tool and agent interface for the [Sliplane](https://sliplane.io) cloud hosting platform API (v0, OpenAPI spec 0.11.0). Built in Rust for developers and autonomous AI workflows.

---

## Highlights

- ⚡ **Sub-3ms Startup**: Compiled as a native static binary with zero runtime dependencies. Executes in ~1–3 ms (compared to ~75 ms for managed runtimes).
- 🔐 **OS Keystore Integration**: `sliplane login` prompts for your token securely and saves it to native OS vaults (macOS Keychain, Linux Secret Service / Keyutils, Windows DPAPI).
- 🌐 **Full 65-Endpoint API Coverage**: Complete command surface across compute servers, web services, managed PostgreSQL databases, S3 object storage buckets, and OAuth clients.
- 🤖 **AI Agent Native**: Machine-readable `--json` output, standardized error envelopes with stable exit codes, and explicit `llms.txt` agent guidance.
- 🛡️ **Safety Guardrails**: Strict account scoping protects multi-tenant setups; safeguards prevent accidental deletion of environment variables or secrets.

---

## Installation

### Using Cargo

```bash
cargo install --git https://github.com/SpaceCorps/Sliplane-Cli --locked
```

### Pre-built Standalone Binaries

Download standalone binary archives directly from the [GitHub Releases](https://github.com/SpaceCorps/Sliplane-Cli/releases/latest) page:

| Platform | Architecture | Binary Package |
|:---|:---|:---|
| **macOS** | Apple Silicon (`aarch64`) | [`sliplane-v1.2.0-aarch64-apple-darwin.tar.gz`](https://github.com/SpaceCorps/Sliplane-Cli/releases/download/v1.2.0/sliplane-v1.2.0-aarch64-apple-darwin.tar.gz) |
| **macOS** | Intel (`x86_64`) | [`sliplane-v1.2.0-x86_64-apple-darwin.tar.gz`](https://github.com/SpaceCorps/Sliplane-Cli/releases/download/v1.2.0/sliplane-v1.2.0-x86_64-apple-darwin.tar.gz) |
| **Linux** | x86_64 (musl static) | [`sliplane-v1.2.0-x86_64-unknown-linux-musl.tar.gz`](https://github.com/SpaceCorps/Sliplane-Cli/releases/download/v1.2.0/sliplane-v1.2.0-x86_64-unknown-linux-musl.tar.gz) |
| **Windows**| x64 (MSVC) | [`sliplane-v1.2.0-x86_64-pc-windows-msvc.zip`](https://github.com/SpaceCorps/Sliplane-Cli/releases/download/v1.2.0/sliplane-v1.2.0-x86_64-pc-windows-msvc.zip) |

---

## Quickstart

### 1. Authenticate

Run `sliplane login` to launch your browser, copy your API token, verify it against `GET /v0/me`, and store it in your OS keystore:

```bash
# Interactive browser login (saved under account 'default')
sliplane login

# Log in with a specific account name
sliplane login staging

# Headless / CI pipeline login (reads token from stdin with no shell history trace)
echo "$SLIPLANE_API_KEY" | sliplane login ci --api-key-stdin
```

### 2. Verify Identity & List Resources

```bash
# Verify your token identity
sliplane me -a default

# List all projects
sliplane projects list -a default

# View compute servers
sliplane servers list -a default
```

### 3. Deploy & Manage Services

```bash
# Deploy a web service
sliplane services deploy --project-id prj_123 --service-id srv_abc -a default

# Stream live container logs
sliplane services logs --project-id prj_123 --service-id srv_abc -a default
```

---

## Command Reference

Every command that accesses the API accepts `--account <name>` (short `-a <name>`).

Wherever a command takes `--project-id`, `--service-id`, `--server-id`, `--postgres-id` or `--bucket-id`, it also takes the resource's name: `--project`, `--service`, `--server`, `--postgres`, `--bucket` (each accepts a name or an id). `sliplane services logs --service web --since 10m -a work` works without looking anything up first. A name shared by two things is refused with their ids, never guessed.

### Identity & Authentication

| Command | Description |
|:---|:---|
| `sliplane login [name]` | Interactively authenticate with your API token and save it to the OS keystore |
| `sliplane me` | Query identity, organization and the token's scopes (e.g. `services:write`, `secrets:read`) |
| `sliplane version` | CLI version and the API spec it targets |
| `sliplane completion <shell>` | Shell completion script for bash, zsh, fish, powershell or elvish |
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
| `sliplane services deploy --project-id <p> --service-id <s> [--wait]` | Trigger immediate redeployment; `--wait` blocks until it succeeds or fails |
| `sliplane services logs --project-id <p> --service-id <s> [--since 30m] [--to <ts>]` | Read container logs (paginated, 500 lines per call) |
| `sliplane services logs --project-id <p> --service-id <s> --follow [--for 2m]` | Stream new log lines as they arrive (JSON Lines with `--json`) |
| `sliplane services pause / unpause` | Pause or resume container execution |
| `sliplane services set-env --project-id <p> --service-id <s> --key <k> --value <v>` | Safely set or update a single environment variable |
| `sliplane services delete-env --project-id <p> --service-id <s> --key <k>` | Remove a single environment variable |
| `sliplane services add-domain / remove-domain` | Configure custom domain routing |
| `sliplane services rotate-deploy-hook --project-id <p> --service-id <s>` | New deploy hook URL; the old one stops working immediately (needs `secrets:read`) |
| `sliplane services attach-volume --project-id <p> --service-id <s> --volume <id-or-name>:/path` | Mount a volume and redeploy |
| `sliplane services update-volume --project-id <p> --service-id <s> --volume-id <v> --mount-path /path` | Change a mount path and redeploy |
| `sliplane services detach-volume --project-id <p> --service-id <s> --volume-id <v>` | Unmount a volume and redeploy (the data is kept) |

### Presets (one-click apps)

The dashboard's catalog of ready-made services (Postgres, Redis, n8n, Grafana, and about 70 more). `list` and `show` need no account.

| Command | Description |
|:---|:---|
| `sliplane presets list [--search <text>] [--tag <tag>]` | List the catalog |
| `sliplane presets show <id>` | Image, ports, environment and volumes of one preset, secrets hidden |
| `sliplane presets deploy <id> --project-id <p> --server-id <s> [--name <n>] [--env K=V] [--secret-env K=V] [--private] [--dry-run] [--wait]` | Create a service from a preset with a fresh name, volume and password |

`presets deploy` prints the passwords it generated **once**: secret values cannot be read back from the API. Seven presets (`postgres`, `mongo`, `redis`, `mysql`, `mariadb`, `rabbitmq`, `ssh`) deploy on a public TCP port by default; pass `--private` to keep them off the internet, and `--dry-run` to see the exact request first.

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

### Files in a Bucket (S3)

Objects are reached over the S3 protocol with an **S3 key of their own**, not with the API key. `s3 connect` makes that key once per bucket and keeps it in the OS keystore; the other `s3` commands read it from there. The key pair is never printed, never put on a command line and never written to `config.yaml`, and the API key is never sent to the bucket's endpoint. Everything takes `--bucket <name>` or `--bucket-id <id>`.

| Command | Description |
|:---|:---|
| `sliplane s3 connect --bucket <name> [--timeout 60]` | Create an S3 key for the bucket, store it in the keystore, wait until it works. Reuses a stored key |
| `sliplane s3 ls --bucket <name> [--prefix <p>] [--recursive] [--limit N] [--token T]` | List files (key, size, lastModified, etag), folded at `/` unless `--recursive`. A truncated listing returns a `nextToken` |
| `sliplane s3 upload --bucket <name> --file <path> [--key <k>] [--content-type <mime>] [--force]` | Upload one file in a single request (up to 5 GiB). Refuses to replace an object without `--force` |
| `sliplane s3 download --bucket <name> --key <k> [--out <path> \| -] [--force]` | Download one file; `--out -` streams it to stdout. Refuses to replace a local file without `--force` |
| `sliplane s3 rm --bucket <name> --key <k> [--key <k2> ...]` | Delete files. Every key must exist or nothing is deleted. Asks nothing |
| `sliplane s3 disconnect --bucket <name>` | Forget the stored S3 key. This does **not** revoke it; `buckets delete-key` does |

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
| `SLIPLANE_PRESETS_URL` | Override the preset catalog URL (same purpose) | `https://sliplane.io/api/presets` |

---

## Contributing & License

Contributions are welcome! Please submit issues and pull requests to [SpaceCorps/Sliplane-Cli](https://github.com/SpaceCorps/Sliplane-Cli).

Dual-licensed under the [MIT License](LICENSE).
