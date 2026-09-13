using Sliplane.Console.Infrastructure;
using Spectre.Console.Cli;

namespace Sliplane.Console.Commands.OAuth;

public sealed class ListOAuthClientsCommand : AsyncCommand<ApiSettings>
{
    public override async Task<int> ExecuteAsync(CommandContext context, ApiSettings settings)
    {
        var client = settings.CreateClient();
        var result = await client.GetAsync("oauth-clients");
        Output.Write(result);
        return 0;
    }
}
