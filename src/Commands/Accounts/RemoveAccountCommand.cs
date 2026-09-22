using System.ComponentModel;
using Sliplane.Console.Infrastructure;
using Sliplane.Console.Storage;
using Spectre.Console;
using Spectre.Console.Cli;

namespace Sliplane.Console.Commands.Accounts;

public sealed class RemoveAccountCommand : AsyncCommand<RemoveAccountCommand.Settings>
{
    public sealed class Settings : LocalSettings
    {
        [CommandArgument(0, "<NAME>")]
        [Description("Account name")]
        public string Name { get; init; } = "";

        [CommandOption("--yes")]
        [Description("Skip the confirmation prompt")]
        public bool Yes { get; init; }
    }

    public override async Task<int> ExecuteAsync(CommandContext context, Settings settings)
    {
        settings.ApplyOutputMode();

        var store = SecretStoreFactory.Create();
        var config = ConfigStore.Load();

        var name = config.Accounts.Keys.FirstOrDefault(k => k.Equals(settings.Name, StringComparison.OrdinalIgnoreCase))
            ?? throw new SliplaneException(
                ErrorCode.NoAccount,
                $"No account named '{settings.Name}'.",
                null,
                "sliplane accounts list");

        var account = config.Accounts[name];

        if (!settings.Yes)
        {
            if (System.Console.IsInputRedirected)
                throw SliplaneException.Invalid(
                    $"Removing '{name}' needs confirmation and there is no terminal to ask on.",
                    $"sliplane accounts remove {name} --yes");

            var console = AnsiConsole.Create(new AnsiConsoleSettings { Out = new AnsiConsoleOutput(System.Console.Error) });
            var label = string.IsNullOrWhiteSpace(account.Identity) ? name : $"{name} ({account.Identity})";
            if (!console.Confirm($"Remove account [green]{label}[/]?", false))
                throw SliplaneException.Invalid("Cancelled.");
        }

        using (var _ = await FileLock.AcquireAsync(ConfigStore.LockPath, CancellationToken.None))
        {
            store.Delete(SecretKeys.Account(name));

            config = ConfigStore.Load();
            config.Accounts.Remove(name);
            ConfigStore.Save(config);
        }

        Output.WriteObject(new Dictionary<string, object?>
        {
            ["status"] = "removed",
            ["name"] = name,
            ["identity"] = account.Identity,
            // The key is gone from this machine, not from Sliplane. Anything else holding a copy
            // still works until the key itself is revoked in the dashboard.
            ["note"] = "The API key was deleted locally. Revoke it at https://sliplane.io if it should stop working everywhere."
        });

        return 0;
    }
}
