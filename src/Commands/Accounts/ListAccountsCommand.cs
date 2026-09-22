using System.ComponentModel;
using Sliplane.Console.Auth;
using Sliplane.Console.Infrastructure;
using Sliplane.Console.Storage;
using Spectre.Console.Cli;

namespace Sliplane.Console.Commands.Accounts;

public sealed class ListAccountsCommand : AsyncCommand<ListAccountsCommand.Settings>
{
    public sealed class Settings : LocalSettings
    {
        [CommandOption("--check")]
        [Description("Call the API once per account instead of reporting stored state")]
        public bool Check { get; init; }
    }

    public override async Task<int> ExecuteAsync(CommandContext context, Settings settings)
    {
        settings.ApplyOutputMode();

        var store = SecretStoreFactory.Create();
        var config = ConfigStore.Load();
        var accounts = new List<object>();

        foreach (var (name, account) in config.Accounts.OrderBy(kv => kv.Key, StringComparer.OrdinalIgnoreCase))
        {
            var entry = new Dictionary<string, object?>
            {
                ["name"] = name,
                ["identity"] = account.Identity,
                ["organization"] = account.Organization,
                ["orgId"] = account.OrgId,
                ["addedAt"] = account.AddedAt,
                ["keyStatus"] = await StatusOf(name, account, store, settings.Check)
            };
            accounts.Add(entry);
        }

        Output.WriteObject(new Dictionary<string, object?>
        {
            ["accounts"] = accounts,
            ["count"] = accounts.Count,
            ["secretStore"] = store.Name,
            ["configDir"] = ConfigStore.ConfigDir
        });

        return 0;
    }

    private static async Task<string> StatusOf(string name, AccountConfig account, ISecretStore store, bool check)
    {
        var key = store.Get(SecretKeys.Account(name));
        if (string.IsNullOrWhiteSpace(key)) return "missing_key";

        // Without --check this is what is actually known locally: a key is stored, and whether it
        // still works is a question only the API can answer.
        if (!check) return "stored";

        try
        {
            var client = new SliplaneClient(key, string.IsNullOrWhiteSpace(account.OrgId) ? null : account.OrgId);
            using var _ = await client.GetAsync("me");
            return "valid";
        }
        catch (SliplaneException ex) when (ex.Code == ErrorCode.AuthRequired)
        {
            return "rejected";
        }
        catch (Exception)
        {
            return "unreachable";
        }
    }
}
