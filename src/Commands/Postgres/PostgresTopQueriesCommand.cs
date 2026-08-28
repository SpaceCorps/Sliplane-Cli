using Sliplane.Console.Infrastructure;
using Spectre.Console.Cli;

namespace Sliplane.Console.Commands.Postgres;

public sealed class PostgresTopQueriesCommand : AsyncCommand<PostgresSettings>
{
    public override async Task<int> ExecuteAsync(CommandContext context, PostgresSettings settings)
    {
        var client = settings.CreateClient();
        var result = await client.GetAsync($"postgres/{settings.PostgresId}/top-queries");
        YamlOutput.Write(result);
        return 0;
    }
}
