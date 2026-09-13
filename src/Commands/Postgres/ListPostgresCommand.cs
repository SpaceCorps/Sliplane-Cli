using Sliplane.Console.Infrastructure;
using Spectre.Console.Cli;

namespace Sliplane.Console.Commands.Postgres;

public sealed class ListPostgresCommand : AsyncCommand<ApiSettings>
{
    public override async Task<int> ExecuteAsync(CommandContext context, ApiSettings settings)
    {
        var client = settings.CreateClient();
        var result = await client.GetAsync("postgres");
        Output.Write(result);
        return 0;
    }
}
