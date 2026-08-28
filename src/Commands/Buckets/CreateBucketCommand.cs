using System.ComponentModel;
using Sliplane.Console.Infrastructure;
using Spectre.Console.Cli;

namespace Sliplane.Console.Commands.Buckets;

public sealed class CreateBucketCommand : AsyncCommand<CreateBucketCommand.Settings>
{
    public sealed class Settings : ApiSettings
    {
        [CommandOption("--name <NAME>")]
        [Description("Bucket name, globally unique and following S3 naming rules")]
        public required string Name { get; init; }

        [CommandOption("--region <REGION>")]
        [Description("Region: ger (Frankfurt), us-east (New York)")]
        public required string Region { get; init; }

        [CommandOption("--versioning")]
        [Description("Enable S3 versioning (keeps every version of every object)")]
        public bool Versioning { get; init; }

        [CommandOption("--object-locking")]
        [Description("Enable S3 Object Lock (implicitly enables versioning)")]
        public bool ObjectLocking { get; init; }
    }

    public override async Task<int> ExecuteAsync(CommandContext context, Settings settings)
    {
        var client = settings.CreateClient();
        var result = await client.PostAsync("buckets", new
        {
            name = settings.Name,
            region = settings.Region,
            versioning = settings.Versioning,
            objectLocking = settings.ObjectLocking
        });
        YamlOutput.Write(result);
        return 0;
    }
}
