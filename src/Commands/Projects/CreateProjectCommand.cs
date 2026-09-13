using System.ComponentModel;
using Sliplane.Console.Infrastructure;
using Spectre.Console.Cli;

namespace Sliplane.Console.Commands.Projects;

public sealed class CreateProjectCommand : AsyncCommand<CreateProjectCommand.Settings>
{
    public sealed class Settings : ApiSettings
    {
        [CommandOption("--name <NAME>")]
        [Description("The name of the project")]
        public required string Name { get; init; }
    }

    public override async Task<int> ExecuteAsync(CommandContext context, Settings settings)
    {
        var client = settings.CreateClient();
        var result = await client.PostAsync("projects", new { name = settings.Name });
        Output.Write(result);
        return 0;
    }
}
