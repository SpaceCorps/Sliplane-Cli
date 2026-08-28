using System.ComponentModel;
using Sliplane.Console.Infrastructure;
using Spectre.Console.Cli;

namespace Sliplane.Console.Commands.Buckets;

public sealed class DeleteBucketCommand : AsyncCommand<DeleteBucketCommand.Settings>
{
    public sealed class Settings : ApiSettings
    {
        [CommandOption("--bucket-id <ID>")]
        [Description("The ID of the bucket to delete")]
        public required string BucketId { get; init; }
    }

    public override async Task<int> ExecuteAsync(CommandContext context, Settings settings)
    {
        var client = settings.CreateClient();
        var result = await client.DeleteWithResponseAsync($"buckets/{settings.BucketId}");
        YamlOutput.Write(result);
        return 0;
    }
}
