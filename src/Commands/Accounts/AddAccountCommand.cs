using System.ComponentModel;
using Sliplane.Console.Auth;
using Sliplane.Console.Infrastructure;
using Sliplane.Console.Storage;
using Spectre.Console;
using Spectre.Console.Cli;

namespace Sliplane.Console.Commands.Accounts;

public sealed class AddAccountCommand : AsyncCommand<AddAccountCommand.Settings>
{
    public sealed class Settings : LocalSettings
    {
        [CommandArgument(0, "<NAME>")]
        [Description("Short name for this account, used as --account elsewhere")]
        public string Name { get; init; } = "";

        [CommandOption("--api-key <KEY>")]
        [Description("Sliplane API key (prompted for, without echo, if omitted)")]
        public string? ApiKey { get; init; }

        [CommandOption("--org-id <ORG_ID>")]
        [Description("Organization ID, for legacy tokens that need X-Organization-ID")]
        public string? OrgId { get; init; }

        [CommandOption("--force")]
        [Description("Replace the key on an account that already exists")]
        public bool Force { get; init; }

        [CommandOption("--no-verify")]
        [Description("Store the key without calling the API to check it first")]
        public bool NoVerify { get; init; }

        public override ValidationResult Validate() =>
            string.IsNullOrWhiteSpace(Name)
                ? ValidationResult.Error("An account name is required.")
                : base.Validate();
    }

    public override async Task<int> ExecuteAsync(CommandContext context, Settings settings)
    {
        settings.ApplyOutputMode();

        var name = settings.Name.Trim();
        var store = SecretStoreFactory.Create();
        var config = ConfigStore.Load();

        var existing = config.Accounts.Keys.FirstOrDefault(k => k.Equals(name, StringComparison.OrdinalIgnoreCase));
        if (existing is not null && !settings.Force)
            throw SliplaneException.Invalid(
                $"An account named '{existing}' already exists.",
                $"Pick a different name, or replace its key: sliplane accounts add {existing} --api-key <key> --force");

        name = existing ?? name;

        var key = settings.ApiKey?.Trim();
        if (string.IsNullOrWhiteSpace(key)) key = Prompt(name);
        if (string.IsNullOrWhiteSpace(key)) throw SliplaneException.Invalid("An API key is required.");

        var identity = "";
        var organization = "";
        if (!settings.NoVerify)
        {
            // Verify before storing: a key that does not work is worse than no account at all,
            // because every later failure looks like a problem with the command being run.
            var client = new SliplaneClient(key, settings.OrgId);
            using var me = await client.GetAsync("me");
            identity = SliplaneIdentity.Describe(me);
            organization = SliplaneIdentity.Organization(me);
        }

        using (var _ = await FileLock.AcquireAsync(ConfigStore.LockPath, CancellationToken.None))
        {
            // The key goes in first: a config entry with no key is a broken account, while an
            // orphaned secret is invisible and harmless.
            store.Set(SecretKeys.Account(name), key);

            config = ConfigStore.Load();
            config.Accounts[name] = new AccountConfig
            {
                OrgId = settings.OrgId ?? "",
                Identity = identity,
                Organization = organization,
                AddedAt = DateTimeOffset.UtcNow.ToString("yyyy-MM-ddTHH:mm:ssZ")
            };
            ConfigStore.Save(config);
        }

        Output.WriteObject(new Dictionary<string, object?>
        {
            ["status"] = existing is null ? "added" : "replaced",
            ["name"] = name,
            ["identity"] = identity,
            ["organization"] = organization,
            ["orgId"] = settings.OrgId ?? "",
            ["verified"] = !settings.NoVerify,
            ["secretStore"] = store.Name,
            ["configDir"] = ConfigStore.ConfigDir,
            ["nextStep"] = $"sliplane projects list --account {name}"
        });

        return 0;
    }

    /// <summary>
    /// Prompts on stderr, so stdout stays a clean payload even when the key is typed in.
    /// </summary>
    private static string Prompt(string name)
    {
        if (System.Console.IsInputRedirected)
            throw SliplaneException.Invalid(
                "No API key given and no terminal to prompt on.",
                $"sliplane accounts add {name} --api-key <key>");

        var console = AnsiConsole.Create(new AnsiConsoleSettings { Out = new AnsiConsoleOutput(System.Console.Error) });

        return console.Prompt(new TextPrompt<string>($"API key for [green]{name}[/]:")
            .Secret()
            .Validate(value => string.IsNullOrWhiteSpace(value)
                ? ValidationResult.Error("Cannot be empty")
                : ValidationResult.Success()));
    }
}
