using System.ComponentModel;
using Sliplane.Console.Infrastructure;
using Spectre.Console.Cli;

namespace Sliplane.Console.Commands.Services;

public sealed class GetServiceCommand : AsyncCommand<GetServiceCommand.Settings>
{
    public sealed class Settings : ApiSettings
    {
        [CommandOption("--project-id <ID>")]
        [Description("The ID of the project")]
        public required string ProjectId { get; init; }

        [CommandOption("--service-id <ID>")]
        [Description("The ID of the service")]
        public required string ServiceId { get; init; }
    }

    public override async Task<int> ExecuteAsync(CommandContext context, Settings settings)
    {
        var client = settings.CreateClient();
        var result = await client.GetAsync($"projects/{settings.ProjectId}/services/{settings.ServiceId}");
        Output.Write(result);
        return 0;
    }
}
