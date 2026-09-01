using System.ComponentModel;
using Sliplane.Console.Infrastructure;
using Spectre.Console;
using Spectre.Console.Cli;

namespace Sliplane.Console.Commands.Services;

public sealed class DeleteServiceEnvCommand : AsyncCommand<DeleteServiceEnvCommand.Settings>
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
        [Description("The environment variable key to delete")]
        public required string Key { get; init; }
    }

    public override async Task<int> ExecuteAsync(CommandContext context, Settings settings)
    {
        var client = settings.CreateClient();
        await client.DeleteAsync(
            $"projects/{settings.ProjectId}/services/{settings.ServiceId}/env/{Uri.EscapeDataString(settings.Key)}");
        AnsiConsole.MarkupLine("[green]Environment variable deleted.[/]");
        return 0;
    }
}
