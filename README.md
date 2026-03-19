# Sliplane CLI

A command-line tool for the [Sliplane](https://sliplane.io) API. Manage your servers, services, projects, and more from the terminal.

## Installation

```bash
dotnet tool install -g Sliplane.Console
```

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
| `services add-domain` | Add a custom domain |
| `services remove-domain` | Remove a custom domain |
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
```

## Output

All commands output YAML for easy reading and scripting.

## License

MIT
