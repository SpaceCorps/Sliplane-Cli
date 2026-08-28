using System.ComponentModel;
using Sliplane.Console.Infrastructure;
using Spectre.Console.Cli;

namespace Sliplane.Console.Commands.Postgres;

public sealed class UpdatePostgresCommand : AsyncCommand<UpdatePostgresCommand.Settings>
{
    public sealed class Settings : PostgresSettings
    {
        [CommandOption("--name <NAME>")]
        [Description("The new display name shown in the dashboard")]
        public string? Name { get; init; }

        [CommandOption("--instance-type <TYPE>")]
        [Description("Compute tier to rescale to (cannot scale down)")]
        public string? InstanceType { get; init; }

        [CommandOption("--disk-size-gb <SIZE>")]
        [Description("New disk size in GB: 10, 50, 100, 250, 500, 1000 (can only grow)")]
        public int? DiskSizeGb { get; init; }

        [CommandOption("--ip-allow <CIDR>")]
        [Description("Source IP range allowed to connect, as CIDR or CIDR=description (repeatable, replaces the whole list)")]
        public string[]? IpAllow { get; init; }

        [CommandOption("--block-public-ips")]
        [Description("Block all public database connections (empty IP allow list)")]
        public bool BlockPublicIps { get; init; }
    }

    public override async Task<int> ExecuteAsync(CommandContext context, Settings settings)
    {
        var client = settings.CreateClient();
        var body = new Dictionary<string, object>();
        if (!string.IsNullOrEmpty(settings.Name)) body["name"] = settings.Name;
        if (!string.IsNullOrEmpty(settings.InstanceType)) body["instanceType"] = settings.InstanceType;
        if (settings.DiskSizeGb.HasValue) body["diskSizeGb"] = settings.DiskSizeGb.Value;
        if (settings.BlockPublicIps)
            body["ipAllowList"] = Array.Empty<object>();
        else if (settings.IpAllow is not null)
            body["ipAllowList"] = IpAllowList.Parse(settings.IpAllow);

        var result = await client.PatchAsync($"postgres/{settings.PostgresId}", body);
        YamlOutput.Write(result);
        return 0;
    }
}
