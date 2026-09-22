//! The command tree. One variant per command; `commands` does the work.

use clap::{Args, Parser, Subcommand};

const INSTANCE_TYPES: &str = "starter, base, medium, large, x-large, xx-large, dedicated-base, dedicated-medium, \
                              dedicated-large, dedicated-x-large, dedicated-xx-large, dedicated-xxx-large";

#[derive(Parser)]
#[command(
    name = "sliplane",
    version,
    about = "CLI for the Sliplane API - manage servers, services, Postgres, buckets and more",
    after_help = "An LLM agent should start with: sliplane agent-readme",
    propagate_version = true,
    disable_help_subcommand = true
)]
pub struct Cli {
    /// Print raw JSON instead of YAML, for scripting
    #[arg(long, global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub command: Command,
}

/// Every command that touches the API takes this. Required, but not declared required - a
/// missing value is resolved by `account::resolve` so the failure carries the list of
/// configured accounts instead of a bare parse error.
#[derive(Args, Clone)]
pub struct Account {
    /// Account to run against (required; see 'sliplane accounts list')
    #[arg(short = 'a', long, value_name = "ACCOUNT")]
    pub account: Option<String>,
}

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)] // parsed once per process
pub enum Command {
    /// Get current identity and token context
    Me(Account),
    /// Print the operating manual for an LLM agent driving this CLI
    AgentReadme,
    /// Manage Sliplane accounts and their API keys
    #[command(subcommand)]
    Accounts(Accounts),
    /// Manage projects
    #[command(subcommand)]
    Projects(Projects),
    /// Manage servers
    #[command(subcommand)]
    Servers(Servers),
    /// Manage services
    #[command(subcommand)]
    Services(Services),
    /// Manage managed Postgres databases
    #[command(subcommand)]
    Postgres(Postgres),
    /// Manage S3-compatible object storage buckets
    #[command(subcommand)]
    Buckets(Buckets),
    /// Manage registry credentials
    #[command(subcommand)]
    Credentials(Credentials),
    /// Manage OAuth clients
    #[command(subcommand)]
    Oauth(OAuth),
}

// ---------------------------------------------------------------------------------------------
// accounts

#[derive(Subcommand)]
pub enum Accounts {
    /// Add an account and store its API key in the OS keystore
    Add {
        /// Short name for this account, used as --account elsewhere
        name: String,
        /// Sliplane API key (prompted for, without echo, if omitted)
        #[arg(long, value_name = "KEY", conflicts_with = "api_key_stdin")]
        api_key: Option<String>,
        /// Read the API key from stdin, e.g. `pbpaste | sliplane accounts add work --api-key-stdin`
        #[arg(long)]
        api_key_stdin: bool,
        /// Organization ID sent as X-Organization-ID. Legacy only - current keys embed the organization
        #[arg(long, value_name = "ORG_ID")]
        org_id: Option<String>,
        /// Replace the key on an account that already exists
        #[arg(long)]
        force: bool,
        /// Store the key without calling the API to check it first
        #[arg(long)]
        no_verify: bool,
    },
    /// List configured accounts
    List {
        /// Call the API once per account instead of reporting stored state
        #[arg(long)]
        check: bool,
    },
    /// Check that an account's stored key still works
    Test {
        /// Account name
        name: String,
    },
    /// Remove an account and delete its stored key
    Remove {
        /// Account name
        name: String,
        /// Skip the confirmation prompt
        #[arg(long)]
        yes: bool,
    },
}

// ---------------------------------------------------------------------------------------------
// projects

#[derive(Subcommand)]
pub enum Projects {
    /// List all projects
    List(Account),
    /// Create a new project
    Create {
        /// The name of the project
        #[arg(long)]
        name: String,
        #[command(flatten)]
        a: Account,
    },
    /// Update a project name
    Update {
        /// The ID of the project to update
        #[arg(long, value_name = "ID")]
        project_id: String,
        /// The new name of the project
        #[arg(long)]
        name: String,
        #[command(flatten)]
        a: Account,
    },
    /// Delete a project (it must contain no services)
    Delete {
        /// The ID of the project to delete
        #[arg(long, value_name = "ID")]
        project_id: String,
        #[command(flatten)]
        a: Account,
    },
}

// ---------------------------------------------------------------------------------------------
// servers

#[derive(Args)]
pub struct ServerId {
    /// The ID of the server
    #[arg(long, value_name = "ID")]
    pub server_id: String,
    #[command(flatten)]
    pub a: Account,
}

#[derive(Args)]
pub struct Range {
    /// Predefined time range: 10min, 1h, 24h, 7d
    #[arg(long, default_value = "1h")]
    pub range: String,
    /// From timestamp (Unix seconds)
    #[arg(long, value_name = "TIMESTAMP")]
    pub from: Option<i64>,
    /// To timestamp (Unix seconds)
    #[arg(long, value_name = "TIMESTAMP")]
    pub to: Option<i64>,
}

#[derive(Subcommand)]
pub enum Servers {
    /// List all servers
    List(Account),
    /// Get server details
    Get(ServerId),
    /// Create a new server
    Create {
        /// The name of the server
        #[arg(long)]
        name: String,
        #[arg(long, value_name = "TYPE", help = format!("Instance type: {INSTANCE_TYPES}"))]
        instance_type: String,
        /// Location: ger, fin, us-east, us-west, sin, conf (Confidential Germany - enabled by support, dedicated types only). Legacy: fsn, nbg, hel, ash, hil
        #[arg(long)]
        location: String,
        /// Larger data disk than bundled with the instance type: 50, 100, 250, 500, 1000
        #[arg(long, value_name = "SIZE")]
        disk_size_gb: Option<u32>,
        /// Billing cycle: hourly (default), monthly, yearly
        #[arg(long, value_name = "CYCLE")]
        billing_cycle: Option<String>,
        #[command(flatten)]
        a: Account,
    },
    /// Delete a server
    Delete(ServerId),
    /// Rescale a server (scale up only; 10 per hour)
    Rescale {
        /// New instance type (can only scale up)
        #[arg(long, value_name = "TYPE")]
        instance_type: String,
        /// Billing cycle for the rescaled server: hourly, monthly, yearly
        #[arg(long, value_name = "CYCLE")]
        billing_cycle: Option<String>,
        #[command(flatten)]
        s: ServerId,
    },
    /// Rescale a server's data disk (grow only)
    RescaleDisk {
        /// New data disk size in GB: 50, 100, 250, 500, 1000 (can only grow)
        #[arg(long, value_name = "SIZE")]
        disk_size_gb: u32,
        #[command(flatten)]
        s: ServerId,
    },
    /// Get server metrics
    Metrics {
        #[command(flatten)]
        s: ServerId,
        #[command(flatten)]
        r: Range,
    },
    /// List server volumes
    Volumes(ServerId),
    /// Create a volume on a server
    CreateVolume {
        /// The name of the volume
        #[arg(long)]
        name: String,
        #[command(flatten)]
        s: ServerId,
    },
}

// ---------------------------------------------------------------------------------------------
// services

#[derive(Args)]
pub struct ServiceRef {
    /// The ID of the project
    #[arg(long, value_name = "ID")]
    pub project_id: String,
    /// The ID of the service
    #[arg(long, value_name = "ID")]
    pub service_id: String,
    #[command(flatten)]
    pub a: Account,
}

/// Flags shared by `services create` and `services update`.
#[derive(Args)]
pub struct Deploy {
    /// Container image URL (e.g. docker.io/library/nginx:latest)
    #[arg(long, value_name = "URL")]
    pub image: Option<String>,
    /// Repository URL (e.g. https://github.com/user/repo)
    #[arg(long, value_name = "URL")]
    pub repo: Option<String>,
    /// Branch to deploy from (default: main)
    #[arg(long)]
    pub branch: Option<String>,
    /// Path to Dockerfile (default: Dockerfile)
    #[arg(long = "dockerfile", value_name = "PATH")]
    pub dockerfile_path: Option<String>,
    /// Docker build context (default: .)
    #[arg(long, value_name = "PATH")]
    pub docker_context: Option<String>,
    /// Auto-deploy on push (default: true). `--auto-deploy false` turns it off
    #[arg(long, value_name = "BOOL", num_args = 0..=1, default_missing_value = "true")]
    pub auto_deploy: Option<bool>,
    /// Only deploy if changes in these paths (repeatable, replaces all)
    #[arg(long = "deploy-include-paths", value_name = "PATTERN")]
    pub include_paths: Option<Vec<String>>,
    /// Skip deploy if changes only in these paths (repeatable, replaces all)
    #[arg(long = "deploy-ignore-paths", value_name = "PATTERN")]
    pub ignore_paths: Option<Vec<String>>,
    /// Registry credential ID for private images
    #[arg(long, value_name = "ID")]
    pub registry_credential_id: Option<String>,
    /// Health check path (default: /)
    #[arg(long, value_name = "PATH")]
    pub healthcheck: Option<String>,
    /// Override Docker CMD
    #[arg(long)]
    pub cmd: Option<String>,
}

#[derive(Subcommand)]
pub enum Services {
    /// List services in a project, or in every project
    List {
        /// The ID of the project (lists all projects if omitted)
        #[arg(long, value_name = "ID")]
        project_id: Option<String>,
        #[command(flatten)]
        a: Account,
    },
    /// Get service details
    Get(ServiceRef),
    /// Create a new service (10 per minute)
    Create {
        /// The ID of the project
        #[arg(long, value_name = "ID")]
        project_id: String,
        /// The name of the service
        #[arg(long)]
        name: String,
        /// The ID of the server to run the service on
        #[arg(long, value_name = "ID")]
        server_id: String,
        #[command(flatten)]
        d: Deploy,
        /// Make the service publicly accessible
        #[arg(long)]
        public: bool,
        /// Network protocol for a public service: http, tcp, udp
        #[arg(long)]
        protocol: Option<String>,
        /// Environment variable (repeatable)
        #[arg(long, value_name = "KEY=VALUE")]
        env: Vec<String>,
        /// Secret environment variable (repeatable)
        #[arg(long, value_name = "KEY=VALUE")]
        secret_env: Vec<String>,
        /// Volume mount as id:/path or name:/path (repeatable). A name resolves to the existing volume of that name
        #[arg(long = "volume", value_name = "VOLUME")]
        volumes: Vec<String>,
        #[command(flatten)]
        a: Account,
    },
    /// Update a service
    Update {
        #[command(flatten)]
        s: ServiceRef,
        /// New name for the service
        #[arg(long)]
        name: Option<String>,
        #[command(flatten)]
        d: Deploy,
        /// Environment variable (repeatable). REPLACES the whole environment unless --merge-env
        #[arg(long, value_name = "KEY=VALUE")]
        env: Vec<String>,
        /// Secret environment variable (repeatable). Shares one list with --env
        #[arg(long, value_name = "KEY=VALUE")]
        secret_env: Vec<String>,
        /// Allow --env/--secret-env to drop variables not listed. Without it, an update that would delete a variable is refused
        #[arg(long, conflicts_with = "merge_env")]
        replace_env: bool,
        /// Keep every variable not listed; listed ones are added or overwritten. Existing secrets are kept as they are
        #[arg(long)]
        merge_env: bool,
    },
    /// Delete a service
    Delete(ServiceRef),
    /// Pause a service
    Pause(ServiceRef),
    /// Unpause a service
    Unpause(ServiceRef),
    /// Trigger a deployment (10 per minute)
    Deploy {
        #[command(flatten)]
        s: ServiceRef,
        /// Image tag to deploy (for image-based services)
        #[arg(long)]
        tag: Option<String>,
    },
    /// Get service logs (at most 500 lines per call, newest first back from --to)
    Logs {
        #[command(flatten)]
        s: ServiceRef,
        /// From timestamp (Unix seconds)
        #[arg(long, value_name = "TIMESTAMP")]
        from: Option<i64>,
        /// To timestamp (Unix seconds). Page back by passing the earliest timestamp you received
        #[arg(long, value_name = "TIMESTAMP")]
        to: Option<i64>,
    },
    /// Get service metrics
    Metrics {
        #[command(flatten)]
        s: ServiceRef,
        #[command(flatten)]
        r: Range,
    },
    /// Get service events
    Events(ServiceRef),
    /// List service environment variables
    ListEnv(ServiceRef),
    /// Create or replace one service environment variable
    SetEnv {
        #[command(flatten)]
        s: ServiceRef,
        /// The environment variable key
        #[arg(long)]
        key: String,
        /// The environment variable value
        #[arg(long, allow_hyphen_values = true)]
        value: String,
        /// Mask the value in API responses
        #[arg(long)]
        secret: bool,
    },
    /// Delete one service environment variable
    DeleteEnv {
        #[command(flatten)]
        s: ServiceRef,
        /// The environment variable key to delete
        #[arg(long)]
        key: String,
    },
    /// Add a custom domain
    AddDomain {
        #[command(flatten)]
        s: ServiceRef,
        /// The custom domain to add
        #[arg(long)]
        domain: String,
    },
    /// Remove a custom domain
    RemoveDomain {
        #[command(flatten)]
        s: ServiceRef,
        /// The ID of the custom domain to remove
        #[arg(long, value_name = "ID")]
        domain_id: String,
    },
}

// ---------------------------------------------------------------------------------------------
// postgres

#[derive(Args)]
pub struct PgId {
    /// The ID of the Postgres database
    #[arg(long, value_name = "ID")]
    pub postgres_id: String,
    #[command(flatten)]
    pub a: Account,
}

#[derive(Args)]
pub struct IpAllow {
    /// Source IP range allowed to connect, as CIDR or CIDR=description (repeatable, replaces the whole list)
    #[arg(long, value_name = "CIDR")]
    pub ip_allow: Option<Vec<String>>,
    /// Block all public database connections (empty IP allow list)
    #[arg(long, conflicts_with = "ip_allow")]
    pub block_public_ips: bool,
}

#[derive(Subcommand)]
pub enum Postgres {
    /// List all Postgres databases
    List(Account),
    /// Get Postgres database details
    Get(PgId),
    /// Create a new Postgres database
    Create {
        /// The display name shown in the dashboard
        #[arg(long)]
        name: String,
        #[arg(long, value_name = "TYPE", help = format!("Instance type: {INSTANCE_TYPES}"))]
        instance_type: String,
        /// Region: ger, fin, us-east, us-west, sin, conf (Confidential Germany - enabled by support, dedicated types only)
        #[arg(long)]
        region: String,
        /// PostgreSQL major version (default and only supported today: 18)
        #[arg(long, value_name = "VERSION")]
        pg_version: Option<u32>,
        /// Name of the database to create (default: randomly generated)
        #[arg(long, value_name = "NAME")]
        database_name: Option<String>,
        /// Name of the role to create (default: randomly generated)
        #[arg(long, value_name = "USER")]
        database_user: Option<String>,
        /// Disk size in GB: 10, 50, 100, 250, 500, 1000 (default: 10)
        #[arg(long, value_name = "SIZE")]
        disk_size_gb: Option<u32>,
        /// Billing cycle: hourly (default), monthly, yearly
        #[arg(long, value_name = "CYCLE")]
        billing_cycle: Option<String>,
        #[command(flatten)]
        ip: IpAllow,
        #[command(flatten)]
        a: Account,
    },
    /// Update a Postgres database (rename, rescale, IP allow list)
    Update {
        #[command(flatten)]
        p: PgId,
        /// The new display name shown in the dashboard
        #[arg(long)]
        name: Option<String>,
        /// Compute tier to rescale to (cannot scale down)
        #[arg(long, value_name = "TYPE")]
        instance_type: Option<String>,
        /// New disk size in GB: 10, 50, 100, 250, 500, 1000 (can only grow)
        #[arg(long, value_name = "SIZE")]
        disk_size_gb: Option<u32>,
        #[command(flatten)]
        ip: IpAllow,
    },
    /// Delete a Postgres database
    Delete(PgId),
    /// Pause a Postgres database
    Pause(PgId),
    /// Unpause a paused Postgres database
    Unpause(PgId),
    /// Restart a Postgres database
    Restart(PgId),
    /// Rotate generated Postgres credentials
    RotateCredentials(PgId),
    /// List recent Postgres server logs
    Logs {
        #[command(flatten)]
        p: PgId,
        /// Earliest log timestamp (RFC 3339), clamped to the 24h retention window
        #[arg(long, value_name = "TIMESTAMP")]
        since: Option<String>,
        /// Latest log timestamp (RFC 3339, default: now)
        #[arg(long, value_name = "TIMESTAMP")]
        until: Option<String>,
        /// Maximum number of log lines, newest first (1-1000, default: 1000)
        #[arg(long, value_name = "COUNT", value_parser = clap::value_parser!(u32).range(1..=1000))]
        limit: Option<u32>,
    },
    /// List current active connections
    Connections(PgId),
    /// List current table and index sizes
    RelationSizes(PgId),
    /// List current slow queries
    SlowQueries(PgId),
    /// List current top queries by call count
    TopQueries(PgId),
    /// Get the point-in-time restore window
    RestoreWindow(PgId),
    /// Restore a database to a point in time (creates a new database)
    Restore {
        #[command(flatten)]
        p: PgId,
        /// The point in time to restore to (RFC 3339), within the restore window
        #[arg(long)]
        timestamp: String,
    },
}

// ---------------------------------------------------------------------------------------------
// buckets

#[derive(Args)]
pub struct BucketId {
    /// The ID of the bucket
    #[arg(long, value_name = "ID")]
    pub bucket_id: String,
    #[command(flatten)]
    pub a: Account,
}

#[derive(Subcommand)]
pub enum Buckets {
    /// List all buckets
    List(Account),
    /// Create a bucket
    Create {
        /// Bucket name: 3-63 chars, lowercase letters, digits, dots and hyphens, globally unique
        #[arg(long)]
        name: String,
        /// Region: ger (Frankfurt), us-east (New York)
        #[arg(long)]
        region: String,
        /// Enable S3 versioning (keeps every version of every object)
        #[arg(long)]
        versioning: bool,
        /// Enable S3 Object Lock (implicitly enables versioning)
        #[arg(long)]
        object_locking: bool,
        #[command(flatten)]
        a: Account,
    },
    /// Enable or disable bucket versioning
    Update {
        #[command(flatten)]
        b: BucketId,
        /// true to enable S3 versioning, false to disable it
        #[arg(long, value_name = "BOOL", action = clap::ArgAction::Set)]
        versioning: bool,
    },
    /// Schedule a bucket for deletion
    Delete(BucketId),
    /// Get the bucket CORS configuration
    Cors(BucketId),
    /// Replace the bucket CORS configuration
    SetCors {
        #[command(flatten)]
        b: BucketId,
        /// JSON file with the full CORS configuration (up to 10 rules), or a bare array of rules
        #[arg(long, value_name = "PATH", conflicts_with_all = ["allowed_origins", "allowed_methods"])]
        file: Option<String>,
        /// Allowed origin, e.g. https://app.example.com or * (repeatable)
        #[arg(long = "allowed-origin", value_name = "ORIGIN")]
        allowed_origins: Vec<String>,
        /// Allowed method: GET, PUT, POST, DELETE, HEAD (repeatable)
        #[arg(long = "allowed-method", value_name = "METHOD")]
        allowed_methods: Vec<String>,
        /// Request header the browser may send (repeatable)
        #[arg(long = "allowed-header", value_name = "HEADER")]
        allowed_headers: Vec<String>,
        /// Response header the browser may read (repeatable)
        #[arg(long = "expose-header", value_name = "HEADER")]
        expose_headers: Vec<String>,
        /// How long the browser may cache the preflight response (0-86400)
        #[arg(long = "max-age", value_name = "SECONDS", value_parser = clap::value_parser!(u32).range(0..=86400))]
        max_age_seconds: Option<u32>,
    },
    /// Delete the bucket CORS configuration
    DeleteCors(BucketId),
    /// List bucket access keys
    Keys(BucketId),
    /// Create a bucket access key (the secret is shown once)
    CreateKey {
        #[command(flatten)]
        b: BucketId,
        /// Human-readable name for this access key
        #[arg(long)]
        name: String,
    },
    /// Delete a bucket access key
    DeleteKey {
        #[command(flatten)]
        b: BucketId,
        /// The ID of the access key to delete
        #[arg(long, value_name = "ID")]
        key_id: String,
    },
}

// ---------------------------------------------------------------------------------------------
// credentials

#[derive(Args)]
pub struct CredId {
    /// The ID of the registry credentials
    #[arg(long, value_name = "ID")]
    pub credential_id: String,
    #[command(flatten)]
    pub a: Account,
}

#[derive(Subcommand)]
pub enum Credentials {
    /// List all registry credentials
    List(Account),
    /// Get registry credentials details
    Get(CredId),
    /// Create registry credentials
    Create {
        /// A name to identify these credentials
        #[arg(long)]
        name: String,
        /// Registry type: ghcr, dockerhub, dhi, generic
        #[arg(long = "type", value_name = "TYPE")]
        kind: String,
        /// Registry username
        #[arg(long)]
        username: String,
        /// Registry authentication token
        #[arg(long)]
        token: String,
        #[command(flatten)]
        a: Account,
    },
    /// Update registry credentials name
    Update {
        #[command(flatten)]
        c: CredId,
        /// The new name for the credentials
        #[arg(long)]
        name: String,
    },
    /// Delete registry credentials
    Delete(CredId),
}

// ---------------------------------------------------------------------------------------------
// oauth

#[derive(Args)]
pub struct ClientId {
    /// The OAuth client ID
    #[arg(long, value_name = "ID")]
    pub client_id: String,
    #[command(flatten)]
    pub a: Account,
}

#[derive(Subcommand)]
pub enum OAuth {
    /// List OAuth clients
    List(Account),
    /// Get OAuth client details
    Get(ClientId),
    /// Update OAuth client metadata
    Update {
        #[command(flatten)]
        c: ClientId,
        /// Updated display name
        #[arg(long)]
        name: Option<String>,
        /// Updated image URL (empty string to clear)
        #[arg(long, value_name = "URL")]
        image_url: Option<String>,
        /// Redirect URI (repeatable, replaces all)
        #[arg(long = "redirect-uri", value_name = "URI")]
        redirect_uris: Option<Vec<String>>,
    },
    /// List OAuth client authorized users
    Users(ClientId),
}

#[cfg(test)]
mod tests {
    #[test]
    fn command_tree_is_valid() {
        use clap::CommandFactory;
        super::Cli::command().debug_assert();
    }
}
