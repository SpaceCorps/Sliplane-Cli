using Sliplane.Console.Infrastructure;
using Sliplane.Console.Storage;

namespace Sliplane.Console.Auth;

public sealed record ResolvedAccount(string Name, AccountConfig Config, string ApiKey)
{
    public string? OrgId => string.IsNullOrWhiteSpace(Config.OrgId) ? null : Config.OrgId;
    public string Identity => Config.Identity;
    public string Organization => Config.Organization;
}

/// <summary>
/// The account is always explicit - no stored default, no environment variable, no implicit
/// fallback when only one account is configured.
///
/// The failure this prevents is an agent working from a summarized transcript deleting a service
/// in the wrong organization: the call succeeds, and nothing in the output says it went to the
/// account you did not mean.
/// </summary>
public static class AccountResolver
{
    public static ResolvedAccount Resolve(string? requested)
    {
        var config = ConfigStore.Load();

        if (string.IsNullOrWhiteSpace(requested))
            throw new SliplaneException(
                ErrorCode.NoAccount,
                "No account specified. Pass --account <name>.",
                Describe(config),
                "sliplane accounts list");

        var name = config.Accounts.Keys.FirstOrDefault(k => k.Equals(requested, StringComparison.OrdinalIgnoreCase));

        if (name is null || !config.Accounts.TryGetValue(name, out var account))
            throw new SliplaneException(
                ErrorCode.NoAccount,
                $"No account named '{requested}'.",
                Describe(config),
                "sliplane accounts list");

        var key = SecretStoreFactory.Create().Get(SecretKeys.Account(name));

        // A config entry with no key is an account that was half-removed, or one whose keystore
        // entry was cleared behind our back. Either way it cannot be used and re-adding is the fix.
        if (string.IsNullOrWhiteSpace(key))
            throw new SliplaneException(
                ErrorCode.AuthRequired,
                $"Account '{name}' has no stored API key.",
                "The config entry exists but the keystore has nothing under it.",
                $"sliplane accounts add {name} --api-key <key>");

        return new ResolvedAccount(name, account, key);
    }

    private static string Describe(SliplaneConfig config)
    {
        if (config.Accounts.Count == 0)
            return "No accounts are configured yet. Run 'sliplane accounts add <name> --api-key <key>'.";

        var listed = config.Accounts
            .OrderBy(kv => kv.Key, StringComparer.OrdinalIgnoreCase)
            .Select(kv => string.IsNullOrWhiteSpace(kv.Value.Identity) ? kv.Key : $"{kv.Key} ({kv.Value.Identity})");

        return "Configured accounts: " + string.Join(", ", listed);
    }
}
