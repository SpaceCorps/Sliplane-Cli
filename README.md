# Sliplane CLI

A command-line tool for the [Sliplane](https://sliplane.io) API. Manage your servers, services, projects, and more from the terminal.

## Installation

A single static binary, written in Rust. It starts in a millisecond or two; the .NET tool it
replaces took about 75 ms before it made its first request.

```bash
cargo install --git https://github.com/SpaceCorps/Sliplane-Cli --locked
```

Or download a prebuilt binary for macOS, Linux or Windows from the
[releases page](https://github.com/SpaceCorps/Sliplane-Cli/releases). Config and keystore
entries are the same as the .NET version's, so accounts you added with it keep working.

Checked against Sliplane API spec **0.5.0** (`https://ctrl.sliplane.io/spec.json`), which has
65 endpoints. Every one of them has a command.

## Output

Commands print YAML by default. Pass `--json` for raw JSON, which is what you
want when scripting:

```bash
sliplane services get --project-id p --service-id s --json | jq -r .status
```

## Notes on behaviour

**`--env` replaces the whole environment.** It is not a merge - the API takes
the array it is given. Passing two variables to a service that holds three
deletes the third. The CLI refuses such an update and names what would be lost.
Pass `--merge-env` to keep the variables you did not list, or `--replace-env` if
removal is what you meant. To change one variable, use `services set-env`, which
leaves the rest alone.

**`services update` carries the current deployment over.** The API takes a whole
deployment object, and the spec gives its fields defaults (`branch: main`,
`dockerfilePath: Dockerfile`). So `services update --healthcheck /health` resends
the current branch, Dockerfile, context, path filters and registry credentials
rather than risk them resetting.

**`--branch`, `--dockerfile` and `--docker-context` work on their own.** They are
applied to the service's current repository, so `services update --branch main`
is enough. They are refused with `--image` or on a service that runs a registry
image. After the update the CLI checks the service the API returns, and fails if
a requested branch, Dockerfile or context did not take effect.

**Secret values are write-only.** They read back as `''`, so they cannot be
copied from one service to another. A secret sent back with an empty value keeps
its stored value, which is how `--merge-env` preserves secrets it cannot read.

**Actions that return no body print a status.** `delete`, `pause`, `deploy` and
the rest print a small `status:` object, so stdout stays parseable with `--json`.

**A service cannot move between a registry image and a repository build.** The
API returns 409. Delete and recreate it instead - volumes are server-level
resources and survive, so nothing on them is lost.

**Network settings and volumes are fixed at creation.** `--public`, `--protocol`
and `--volume` exist only on `services create`, and `--protocol` needs `--public`.

**Volume names resolve to an existing volume.** `--volume my-data:/data` attaches
the volume already called `my-data` rather than creating another one beside it.

**Windows: run from PowerShell or cmd, not Git Bash.** Git Bash rewrites
arguments that look like Unix paths before this program sees them, so
`--healthcheck /` arrives as `C:/Program Files/Git/`. `MSYS_NO_PATHCONV=1` does
not prevent it. The CLI detects the result and refuses rather than deploying a
service whose healthchecks can never pass.

## Accounts & Authentication

To log in interactively, run `sliplane login`. It will open your browser to the Sliplane dashboard to generate or copy an API token, prompt for the key securely, verify it, and store it in your OS keystore:

```bash
sliplane login                      # logs in, saves as account 'default'
sliplane login work                 # logs in with a specific account name
sliplane login --api-key sl_...     # non-interactive login with API key
pbpaste | sliplane login --api-key-stdin # pipe token from stdin
```

You can also use `sliplane accounts add`:

```bash
sliplane accounts add work --api-key sl_your_api_key
sliplane accounts add side-project        # prompts for the key, without echo
pbpaste | sliplane accounts add ci --api-key-stdin   # no terminal: read the key from stdin
```

`add` calls `me` with the key before storing it, so a bad key fails here rather than on some
later command. The key itself goes into the OS keystore - DPAPI on Windows, Keychain on macOS,
libsecret on Linux - and only the name, organization and a label are written to `config.yaml`.
Current keys embed their organization (`api_rw_org_...`). `--org-id` exists only for legacy
tokens that still need `X-Organization-ID`.

Then name the account on every command:

```bash
sliplane projects list --account work
sliplane services deploy --project-id p --service-id s -a work
```

There is no default account and no `SLIPLANE_API_KEY`: with keys for several accounts on one
machine, a default is how a deploy ends up in the wrong one.

```bash
sliplane accounts list [--check]    # --check calls the API once per account
sliplane accounts test work         # what this key is, and whether it still works
sliplane accounts remove work       # deletes the local key; does not revoke it at Sliplane
```

| Variable | Effect |
| --- | --- |
| `SLIPLANE_CONFIG_DIR` | Overrides where `config.yaml` and the secret store live |
| `SLIPLANE_SECRET_STORE` | Forces a backend: `dpapi`, `keychain`, `libsecret`, `plaintext` |
| `SLIPLANE_ALLOW_PLAINTEXT_STORE=1` | Permits a 0600 file where no OS keystore exists |
| `SLIPLANE_API_URL` | Overrides the API base URL (for tests against a mock; never selects a key) |

## Driving this from an agent

```bash
sliplane agent-readme           # the operating manual, as markdown
sliplane agent-readme --json    # the same rules and exit codes as data
```

Errors print a YAML (or JSON) envelope on stderr with a stable `code`, and the exit status
matches it: `1` error, `2` network, `3` auth_required, `4` not_found, `5` rate_limited,
`6` invalid_input, `7` no_account. An envelope carries `remediation` when a specific command
fixes the problem.

## Usage

```bash
sliplane <command> [options]
```

### Commands

Every command below takes `--account <name>` (short `-a`), except `accounts *` and
`agent-readme`.

| Command | Description |
|---------|-------------|
| `me` | Get current identity and token context |
| `login` | Log in with a Sliplane API token (opens browser to copy token) |
| `agent-readme` | Print the operating manual for an LLM agent |
| **Accounts** | |
| `accounts add` | Add an account and store its API key in the OS keystore |
| `accounts list` | List configured accounts |
| `accounts test` | Check that an account's stored key still works |
| `accounts remove` | Remove an account and delete its stored key |
| **Projects** | |
| `projects list` | List all projects |
| `projects create` | Create a new project |
| `projects update` | Update a project name |
| `projects delete` | Delete a project (must have no services) |
| **Servers** | |
| `servers list` | List all servers |
| `servers get` | Get server details |
| `servers create` | Create a new server |
| `servers delete` | Delete a server |
| `servers rescale` | Rescale a server (scale up only) |
| `servers rescale-disk` | Grow a server's data disk |
| `servers metrics` | Get server metrics |
| `servers volumes` | List server volumes |
| `servers create-volume` | Create a volume on a server |
| **Services** | |
| `services list` | List services in a project |
| `services get` | Get service details |
| `services create` | Create a new service (`--image` or `--repo` required) |
| `services update` | Update a service (`--merge-env` / `--replace-env` for env changes) |
| `services delete` | Delete a service |
| `services pause` | Pause a service |
| `services unpause` | Unpause a service |
| `services deploy` | Trigger a deployment |
| `services logs` | Get service logs (500 lines per call; page back with `--to`) |
| `services metrics` | Get service metrics |
| `services events` | Get service events |
| `services list-env` | List environment variables |
| `services set-env` | Create or replace an environment variable |
| `services delete-env` | Delete an environment variable |
| `services add-domain` | Add a custom domain |
| `services remove-domain` | Remove a custom domain |
| **Postgres** | |
| `postgres list` | List all Postgres databases |
| `postgres get` | Get Postgres database details |
| `postgres create` | Create a new Postgres database |
| `postgres update` | Rename, rescale, or set the IP allow list |
| `postgres delete` | Delete a Postgres database |
| `postgres pause` | Pause a Postgres database |
| `postgres unpause` | Unpause a paused Postgres database |
| `postgres restart` | Restart a Postgres database |
| `postgres rotate-credentials` | Rotate generated credentials |
| `postgres logs` | List recent server logs |
| `postgres connections` | List current active connections |
| `postgres relation-sizes` | List current table and index sizes |
| `postgres slow-queries` | List current slow queries |
| `postgres top-queries` | List current top queries by call count |
| `postgres restore-window` | Get the point-in-time restore window |
| `postgres restore` | Restore to a point in time (creates a new database) |
| **Buckets** | |
| `buckets list` | List all buckets |
| `buckets create` | Create an S3-compatible bucket |
| `buckets update` | Enable or disable bucket versioning |
| `buckets delete` | Schedule a bucket for deletion |
| `buckets cors` | Get the CORS configuration |
| `buckets set-cors` | Replace the CORS configuration |
| `buckets delete-cors` | Delete the CORS configuration |
| `buckets keys` | List bucket access keys |
| `buckets create-key` | Create a bucket access key |
| `buckets delete-key` | Delete a bucket access key |
| **Credentials** | |
| `credentials list` | List all registry credentials |
| `credentials get` | Get registry credentials details |
| `credentials create` | Create registry credentials |
| `credentials update` | Update registry credentials name |
| `credentials delete` | Delete registry credentials |
| **OAuth** | |
| `oauth list` | List OAuth clients |
| `oauth get` | Get OAuth client details |
| `oauth update` | Update OAuth client metadata |
| `oauth users` | List OAuth client authorized users |

### Examples

```bash
# Configure an account once
sliplane accounts add work --api-key sl_your_api_key

# Check your identity
sliplane me -a work

# List all projects
sliplane projects list -a work

# Create a server
sliplane servers create --name my-server --instance-type base --location ger -a work

# Add one variable to a service, keeping the rest (secrets included)
sliplane services update --project-id project_abc --service-id service_abc --env LOG_LEVEL=debug --merge-env -a work

# Deploy a service
sliplane services deploy --project-id project_abc --service-id service_abc -a work

# View service logs
sliplane services logs --project-id project_abc --service-id service_abc -a work

# Create a Postgres database
sliplane postgres create --name my-db --instance-type base --region ger -a work

# Restrict a database to a single network
sliplane postgres update --postgres-id pg_abc123 --ip-allow 203.0.113.0/24=office -a work

# Create a bucket and an access key for it
sliplane buckets create --name my-app-uploads --region ger -a work
sliplane buckets create-key --bucket-id sb_abc123 --name "Production uploads" -a work
```

> The PostgreSQL major version on `postgres create` is `--pg-version`; `--version` is reserved by the CLI itself.

## License

MIT
