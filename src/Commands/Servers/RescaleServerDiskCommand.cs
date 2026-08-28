using System.ComponentModel;
using Sliplane.Console.Infrastructure;
using Spectre.Console;
using Spectre.Console.Cli;

namespace Sliplane.Console.Commands.Servers;

public sealed class RescaleServerDiskCommand : AsyncCommand<RescaleServerDiskCommand.Settings>
{
    public sealed class Settings : ApiSettings
    {
        [CommandOption("--server-id <ID>")]
        [Description("The ID of the server whose disk should be rescaled")]
        public required string ServerId { get; init; }

        [CommandOption("--disk-size-gb <SIZE>")]
        [Description("New data disk size in GB: 50, 100, 250, 500, 1000 (can only grow)")]
        public required int DiskSizeGb { get; init; }
    }

    public override async Task<int> ExecuteAsync(CommandContext context, Settings settings)
    {
        var client = settings.CreateClient();
        await client.PostAsync($"servers/{settings.ServerId}/disk", new { diskSizeGb = settings.DiskSizeGb });
        AnsiConsole.MarkupLine("[green]Disk rescale request accepted.[/]");
        return 0;
    }
}
