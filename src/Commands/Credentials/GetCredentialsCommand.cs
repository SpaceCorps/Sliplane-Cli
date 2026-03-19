using System.ComponentModel;
using Slipline.Console.Infrastructure;
using Spectre.Console.Cli;

namespace Slipline.Console.Commands.Credentials;

public sealed class GetCredentialsCommand : AsyncCommand<GetCredentialsCommand.Settings>
{
    public sealed class Settings : ApiSettings
    {
        [CommandOption("--credential-id <ID>")]
        [Description("The ID of the registry credentials")]
        public required string CredentialId { get; init; }
    }

    public override async Task<int> ExecuteAsync(CommandContext context, Settings settings)
    {
        var client = settings.CreateClient();
        var result = await client.GetAsync($"registry-credentials/{settings.CredentialId}");
        YamlOutput.Write(result);
        return 0;
    }
}
