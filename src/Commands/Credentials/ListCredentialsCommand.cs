using Slipline.Console.Infrastructure;
using Spectre.Console.Cli;

namespace Slipline.Console.Commands.Credentials;

public sealed class ListCredentialsCommand : AsyncCommand<ApiSettings>
{
    public override async Task<int> ExecuteAsync(CommandContext context, ApiSettings settings)
    {
        var client = settings.CreateClient();
        var result = await client.GetAsync("registry-credentials");
        YamlOutput.Write(result);
        return 0;
    }
}
