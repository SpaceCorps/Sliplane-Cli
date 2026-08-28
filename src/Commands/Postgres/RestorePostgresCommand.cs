using System.ComponentModel;
using Sliplane.Console.Infrastructure;
using Spectre.Console.Cli;

namespace Sliplane.Console.Commands.Postgres;

public sealed class RestorePostgresCommand : AsyncCommand<RestorePostgresCommand.Settings>
{
    public sealed class Settings : PostgresSettings
    {
        [CommandOption("--timestamp <TIMESTAMP>")]
        [Description("The point in time to restore to (RFC 3339), within the restore window")]
        public required string Timestamp { get; init; }
    }

    public override async Task<int> ExecuteAsync(CommandContext context, Settings settings)
    {
        var client = settings.CreateClient();
        var result = await client.PostAsync($"postgres/{settings.PostgresId}/restore", new { timestamp = settings.Timestamp });
        YamlOutput.Write(result);
        return 0;
    }
}
