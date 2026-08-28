using System.ComponentModel;
using Sliplane.Console.Infrastructure;
using Spectre.Console.Cli;

namespace Sliplane.Console.Commands.Buckets;

public sealed class UpdateBucketCommand : AsyncCommand<UpdateBucketCommand.Settings>
{
    public sealed class Settings : ApiSettings
    {
        [CommandOption("--bucket-id <ID>")]
        [Description("The ID of the bucket")]
        public required string BucketId { get; init; }

        [CommandOption("--versioning <BOOL>")]
        [Description("Set to true to enable S3 versioning, false to disable it")]
        public required bool Versioning { get; init; }
    }

    public override async Task<int> ExecuteAsync(CommandContext context, Settings settings)
    {
        var client = settings.CreateClient();
        var result = await client.PatchAsync($"buckets/{settings.BucketId}", new { versioning = settings.Versioning });
        YamlOutput.Write(result);
        return 0;
    }
}
