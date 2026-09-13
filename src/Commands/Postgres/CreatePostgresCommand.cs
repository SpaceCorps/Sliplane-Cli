using System.ComponentModel;
using Sliplane.Console.Infrastructure;
using Spectre.Console.Cli;

namespace Sliplane.Console.Commands.Postgres;

public sealed class CreatePostgresCommand : AsyncCommand<CreatePostgresCommand.Settings>
{
    public sealed class Settings : ApiSettings
    {
        [CommandOption("--name <NAME>")]
        [Description("The display name shown in the dashboard")]
        public required string Name { get; init; }

        [CommandOption("--instance-type <TYPE>")]
        [Description("Instance type: starter, base, medium, large, x-large, xx-large, dedicated-base, dedicated-medium, dedicated-large, dedicated-x-large, dedicated-xx-large, dedicated-xxx-large")]
        public required string InstanceType { get; init; }

        [CommandOption("--region <REGION>")]
        [Description("Region: ger, fin, us-east, us-west, sin")]
        public required string Region { get; init; }

        [CommandOption("--pg-version <VERSION>")]
        [Description("PostgreSQL major version (default: 18)")]
        public int? PostgresVersion { get; init; }

        [CommandOption("--database-name <NAME>")]
        [Description("Name of the database to create (default: randomly generated)")]
        public string? DatabaseName { get; init; }

        [CommandOption("--database-user <USER>")]
        [Description("Name of the role to create (default: randomly generated)")]
        public string? DatabaseUser { get; init; }

        [CommandOption("--disk-size-gb <SIZE>")]
        [Description("Disk size in GB: 10, 50, 100, 250, 500, 1000 (default: 10)")]
        public int? DiskSizeGb { get; init; }

        [CommandOption("--billing-cycle <CYCLE>")]
        [Description("Billing cycle: hourly (default), monthly, yearly")]
        public string? BillingCycle { get; init; }

        [CommandOption("--ip-allow <CIDR>")]
        [Description("Source IP range allowed to connect, as CIDR or CIDR=description (repeatable, default: all addresses)")]
        public string[]? IpAllow { get; init; }

        [CommandOption("--block-public-ips")]
        [Description("Block all public database connections (empty IP allow list)")]
        public bool BlockPublicIps { get; init; }
    }

    public override async Task<int> ExecuteAsync(CommandContext context, Settings settings)
    {
        var client = settings.CreateClient();
        var body = new Dictionary<string, object>
        {
            ["name"] = settings.Name,
            ["instanceType"] = settings.InstanceType,
            ["region"] = settings.Region
        };
        if (settings.PostgresVersion.HasValue) body["version"] = settings.PostgresVersion.Value;
        if (!string.IsNullOrEmpty(settings.DatabaseName)) body["databaseName"] = settings.DatabaseName;
        if (!string.IsNullOrEmpty(settings.DatabaseUser)) body["databaseUser"] = settings.DatabaseUser;
        if (settings.DiskSizeGb.HasValue) body["diskSizeGb"] = settings.DiskSizeGb.Value;
        if (!string.IsNullOrEmpty(settings.BillingCycle)) body["billingCycle"] = settings.BillingCycle;
        if (settings.BlockPublicIps)
            body["ipAllowList"] = Array.Empty<object>();
        else if (settings.IpAllow is not null)
            body["ipAllowList"] = IpAllowList.Parse(settings.IpAllow);

        var result = await client.PostAsync("postgres", body);
        Output.Write(result);
        return 0;
    }
}
