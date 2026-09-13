using Sliplane.Console.Infrastructure;
using Spectre.Console.Cli;

namespace Sliplane.Console.Commands.Servers;

public sealed class ListServersCommand : AsyncCommand<ApiSettings>
{
    public override async Task<int> ExecuteAsync(CommandContext context, ApiSettings settings)
    {
        var client = settings.CreateClient();
        var result = await client.GetAsync("servers");
        Output.Write(result);
        return 0;
    }
}
