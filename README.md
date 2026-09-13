# Sliplane CLI

A command-line tool for the [Sliplane](https://sliplane.io) API. Manage your servers, services, projects, and more from the terminal.

## Installation

```bash
dotnet tool install -g Sliplane.Console
```

## Output

Commands print YAML by default. Pass `--json` for raw JSON, which is what you
want when scripting:

```bash
sliplane services get --project-id p --service-id s --json | jq -r .status
```

## Notes on behaviour

**`--env` replaces the whole environment.** It is not a merge - the API takes
the array it is given. Passing two variables to a service that holds three
deletes the third. The CLI refuses such an update and names what would be lost;
pass `--replace-env` if removal is what you meant. To change one variable, use
`services set-env`, which leaves the rest alone.

**Secret values are write-only.** They read back as `''`, so they cannot be
copied from one service to another, and a merge could not preserve them.

**A service cannot move between a registry image and a repository build.** The
API returns 409. Delete and recreate it instead - volumes are server-level
resources and survive, so nothing on them is lost.

**Volume names resolve to an existing volume.** `--volume my-data:/data` attaches
the volume already called `my-data` rather than creating another one beside it.

**Windows: run from PowerShell or cmd, not Git Bash.** Git Bash rewrites
arguments that look like Unix paths before this program sees them, so
`--healthcheck /` arrives as `C:/Program Files/Git/`. `MSYS_NO_PATHCONV=1` does
not prevent it. The CLI detects the result and refuses rather than deploying a
service whose healthchecks can never pass.

## Authentication

Set your API key as an environment variable:

```bash
export SLIPLANE_API_KEY=your-api-key
```

Or pass it directly:

```bash
sliplane me --api-key your-api-key
```

For legacy tokens, you can also provide an organization ID via `--org-id` or `SLIPLANE_ORG_ID`.

## Usage

```bash
sliplane <command> [options]
```

### Commands

| Command | Description |
|---------|-------------|
| `me` | Get current identity and token context |
| **Projects** | |
| `projects list` | List all projects |
| `projects create` | Create a new project |
| `projects update` | Update a project name |
| `projects delete` | Delete a project |
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
| `services create` | Create a new service |
| `services update` | Update a service |
| `services delete` | Delete a service |
| `services pause` | Pause a service |
| `services unpause` | Unpause a service |
| `services deploy` | Trigger a deployment |
| `services logs` | Get service logs |
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
# Check your identity
sliplane me

# List all projects
sliplane projects list

# Create a server
sliplane servers create --name my-server --plan starter --region eu

# Deploy a service
sliplane services deploy --service-id abc123

# View service logs
sliplane services logs --service-id abc123

# Create a Postgres database
sliplane postgres create --name my-db --instance-type base --region ger

# Restrict a database to a single network
sliplane postgres update --postgres-id pg_abc123 --ip-allow 203.0.113.0/24=office

# Create a bucket and an access key for it
sliplane buckets create --name my-app-uploads --region ger
sliplane buckets create-key --bucket-id sb_abc123 --name "Production uploads"
```

> The PostgreSQL major version on `postgres create` is `--pg-version`; `--version` is reserved by the CLI itself.

## Output

All commands output YAML for easy reading and scripting.

## License

MIT
