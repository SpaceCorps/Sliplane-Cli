using System.ComponentModel;
using Sliplane.Console.Infrastructure;
using Spectre.Console.Cli;

namespace Sliplane.Console.Commands.Servers;

public sealed class ServerMetricsCommand : AsyncCommand<ServerMetricsCommand.Settings>
{
    public sealed class Settings : ApiSettings
    {
        [CommandOption("--server-id <ID>")]
        [Description("The ID of the server")]
        public required string ServerId { get; init; }

        [CommandOption("--range <RANGE>")]
        [Description("Predefined time range: 10min, 1h, 24h, 7d (default: 1h)")]
        [DefaultValue("1h")]
        public string? Range { get; init; }

        [CommandOption("--from <TIMESTAMP>")]
        [Description("From timestamp (Unix seconds)")]
        public long? From { get; init; }

        [CommandOption("--to <TIMESTAMP>")]
        [Description("To timestamp (Unix seconds)")]
        public long? To { get; init; }
    }

    public override async Task<int> ExecuteAsync(CommandContext context, Settings settings)
    {
        var client = settings.CreateClient();
        var query = BuildQuery(settings);
        var result = await client.GetAsync($"servers/{settings.ServerId}/metrics{query}");
        YamlOutput.Write(result);
        return 0;
    }

    private static string BuildQuery(Settings s)
    {
        var parts = new List<string>();
        if (!string.IsNullOrEmpty(s.Range)) parts.Add($"range={s.Range}");
        if (s.From.HasValue) parts.Add($"from={s.From}");
        if (s.To.HasValue) parts.Add($"to={s.To}");
        return parts.Count > 0 ? "?" + string.Join("&", parts) : "";
    }
}
