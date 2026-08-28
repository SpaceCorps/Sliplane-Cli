using Sliplane.Console.Infrastructure;
using Spectre.Console.Cli;

namespace Sliplane.Console.Commands.Postgres;

public sealed class UnpausePostgresCommand : AsyncCommand<PostgresSettings>
{
    public override async Task<int> ExecuteAsync(CommandContext context, PostgresSettings settings)
    {
        var client = settings.CreateClient();
        var result = await client.PostAsync($"postgres/{settings.PostgresId}/unpause");
        YamlOutput.Write(result);
        return 0;
    }
}
