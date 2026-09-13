using System.ComponentModel;
using Sliplane.Console.Infrastructure;
using Spectre.Console.Cli;

namespace Sliplane.Console.Commands.OAuth;

public sealed class ListOAuthClientUsersCommand : AsyncCommand<ListOAuthClientUsersCommand.Settings>
{
    public sealed class Settings : ApiSettings
    {
        [CommandOption("--client-id <ID>")]
        [Description("The OAuth client ID")]
        public required string ClientId { get; init; }
    }

    public override async Task<int> ExecuteAsync(CommandContext context, Settings settings)
    {
        var client = settings.CreateClient();
        var result = await client.GetAsync($"oauth-clients/{settings.ClientId}/users");
        Output.Write(result);
        return 0;
    }
}
