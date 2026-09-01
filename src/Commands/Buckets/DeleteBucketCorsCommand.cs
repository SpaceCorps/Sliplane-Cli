using System.ComponentModel;
using Sliplane.Console.Infrastructure;
using Spectre.Console;
using Spectre.Console.Cli;

namespace Sliplane.Console.Commands.Buckets;

public sealed class DeleteBucketCorsCommand : AsyncCommand<DeleteBucketCorsCommand.Settings>
{
    public sealed class Settings : ApiSettings
    {
        [CommandOption("--bucket-id <ID>")]
        [Description("The ID of the bucket")]
        public required string BucketId { get; init; }
    }

    public override async Task<int> ExecuteAsync(CommandContext context, Settings settings)
    {
        var client = settings.CreateClient();
        await client.DeleteAsync($"buckets/{settings.BucketId}/cors");
        AnsiConsole.MarkupLine("[green]CORS configuration deleted.[/]");
        return 0;
    }
}
