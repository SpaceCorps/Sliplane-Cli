using System.ComponentModel;
using Sliplane.Console.Infrastructure;
using Spectre.Console.Cli;

namespace Sliplane.Console.Commands.Postgres;

public sealed class PostgresLogsCommand : AsyncCommand<PostgresLogsCommand.Settings>
{
    public sealed class Settings : PostgresSettings
    {
        [CommandOption("--since <TIMESTAMP>")]
        [Description("Earliest log timestamp (RFC 3339), clamped to the 24h retention window")]
        public string? Since { get; init; }

        [CommandOption("--until <TIMESTAMP>")]
        [Description("Latest log timestamp (RFC 3339, default: now)")]
        public string? Until { get; init; }

        [CommandOption("--limit <COUNT>")]
        [Description("Maximum number of log lines, newest first (1-1000, default: 1000)")]
        public int? Limit { get; init; }
    }

    public override async Task<int> ExecuteAsync(CommandContext context, Settings settings)
    {
        var client = settings.CreateClient();
        var parts = new List<string>();
        if (!string.IsNullOrEmpty(settings.Since)) parts.Add($"since={Uri.EscapeDataString(settings.Since)}");
        if (!string.IsNullOrEmpty(settings.Until)) parts.Add($"until={Uri.EscapeDataString(settings.Until)}");
        if (settings.Limit.HasValue) parts.Add($"limit={settings.Limit}");
        var query = parts.Count > 0 ? "?" + string.Join("&", parts) : "";
        var result = await client.GetAsync($"postgres/{settings.PostgresId}/logs{query}");
        YamlOutput.Write(result);
        return 0;
    }
}
