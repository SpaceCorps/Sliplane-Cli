using Sliplane.Console.Infrastructure;
using Spectre.Console;
using Spectre.Console.Cli;

namespace Sliplane.Console.Commands.Postgres;

public sealed class DeletePostgresCommand : AsyncCommand<PostgresSettings>
{
    public override async Task<int> ExecuteAsync(CommandContext context, PostgresSettings settings)
    {
        var client = settings.CreateClient();
        await client.DeleteAsync($"postgres/{settings.PostgresId}");
        AnsiConsole.MarkupLine("[green]Postgres database deleted.[/]");
        return 0;
    }
}
