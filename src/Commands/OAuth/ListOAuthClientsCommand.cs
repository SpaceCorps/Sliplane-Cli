using Slipline.Console.Infrastructure;
using Spectre.Console.Cli;

namespace Slipline.Console.Commands.OAuth;

public sealed class ListOAuthClientsCommand : AsyncCommand<ApiSettings>
{
    public override async Task<int> ExecuteAsync(CommandContext context, ApiSettings settings)
    {
        var client = settings.CreateClient();
        var result = await client.GetAsync("oauth-clients");
        YamlOutput.Write(result);
        return 0;
    }
}
