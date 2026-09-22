using System.ComponentModel;
using Sliplane.Console.Auth;
using Sliplane.Console.Infrastructure;
using Spectre.Console.Cli;

namespace Sliplane.Console.Commands.Accounts;

public sealed class TestAccountCommand : AsyncCommand<TestAccountCommand.Settings>
{
    public sealed class Settings : LocalSettings
    {
        [CommandArgument(0, "<NAME>")]
        [Description("Account name")]
        public string Name { get; init; } = "";
    }

    public override async Task<int> ExecuteAsync(CommandContext context, Settings settings)
    {
        settings.ApplyOutputMode();

        var account = AccountResolver.Resolve(settings.Name);
        var client = new SliplaneClient(account.ApiKey, account.OrgId);

        using var me = await client.GetAsync("me");
        var identity = SliplaneIdentity.Describe(me);
        var organization = SliplaneIdentity.Organization(me);

        var result = new Dictionary<string, object?>
        {
            ["name"] = account.Name,
            ["identity"] = identity,
            ["organization"] = organization,
            ["orgId"] = account.OrgId ?? "",
            ["keyStatus"] = "valid",
            ["me"] = Json.ToObject(me.RootElement)
        };

        // Drift means the key was replaced with one belonging to somewhere else - worth saying
        // out loud, because every command run against this account now goes somewhere new.
        var drifted = Drift(account.Identity, identity) ?? Drift(account.Organization, organization);
        if (drifted is not null) result["warning"] = drifted;

        Output.WriteObject(result);
        return 0;
    }

    private static string? Drift(string recorded, string current) =>
        !string.IsNullOrWhiteSpace(recorded) &&
        !string.IsNullOrWhiteSpace(current) &&
        !current.Equals(recorded, StringComparison.OrdinalIgnoreCase)
            ? $"This account was added as {recorded}, but the stored key now reports {current}."
            : null;
}
