using System.ComponentModel;
using Sliplane.Console.Infrastructure;
using Spectre.Console.Cli;

namespace Sliplane.Console.Commands.Services;

public sealed class SetServiceEnvCommand : AsyncCommand<SetServiceEnvCommand.Settings>
{
    public sealed class Settings : ApiSettings
    {
        [CommandOption("--project-id <ID>")]
        [Description("The ID of the project")]
        public required string ProjectId { get; init; }

        [CommandOption("--service-id <ID>")]
        [Description("The ID of the service")]
        public required string ServiceId { get; init; }

        [CommandOption("--key <KEY>")]
        [Description("The environment variable key")]
        public required string Key { get; init; }

        [CommandOption("--value <VALUE>")]
        [Description("The environment variable value")]
        public required string Value { get; init; }

        [CommandOption("--secret")]
        [Description("Mask the value in API responses")]
        public bool Secret { get; init; }
    }

    public override async Task<int> ExecuteAsync(CommandContext context, Settings settings)
    {
        var client = settings.CreateClient();
        var result = await client.PutAsync(
            $"projects/{settings.ProjectId}/services/{settings.ServiceId}/env/{Uri.EscapeDataString(settings.Key)}",
            new { value = settings.Value, secret = settings.Secret });
        Output.Write(result);
        return 0;
    }
}
