using System.ComponentModel;
using Sliplane.Console.Infrastructure;
using Spectre.Console.Cli;

namespace Sliplane.Console.Commands.Projects;

public sealed class UpdateProjectCommand : AsyncCommand<UpdateProjectCommand.Settings>
{
    public sealed class Settings : ApiSettings
    {
        [CommandOption("--project-id <ID>")]
        [Description("The ID of the project to update")]
        public required string ProjectId { get; init; }

        [CommandOption("--name <NAME>")]
        [Description("The new name of the project")]
        public required string Name { get; init; }
    }

    public override async Task<int> ExecuteAsync(CommandContext context, Settings settings)
    {
        var client = settings.CreateClient();
        var result = await client.PatchAsync($"projects/{settings.ProjectId}", new { name = settings.Name });
        Output.Write(result);
        return 0;
    }
}
