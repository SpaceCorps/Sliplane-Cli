using System.ComponentModel;
using Spectre.Console.Cli;

namespace Sliplane.Console.Infrastructure;

public class ApiSettings : CommandSettings
{
    [CommandOption("--api-key <KEY>")]
    [Description("Sliplane API key (or set SLIPLANE_API_KEY env var)")]
    public string? ApiKey { get; init; }

    [CommandOption("--org-id <ORG_ID>")]
    [Description("Organization ID for legacy tokens (or set SLIPLANE_ORG_ID env var)")]
    public string? OrgId { get; init; }

    [CommandOption("--json")]
    [Description("Print raw JSON instead of YAML, for scripting")]
    public bool Json { get; init; }

    public SliplaneClient CreateClient()
    {
        // Commands render through Output.Write without knowing their settings,
        // so the choice is recorded here, once, as the client is built.
        Output.UseJson = Json;

        var key = ApiKey ?? Environment.GetEnvironmentVariable("SLIPLANE_API_KEY")
            ?? throw new InvalidOperationException("API key required. Use --api-key or set SLIPLANE_API_KEY.");
        var org = OrgId ?? Environment.GetEnvironmentVariable("SLIPLANE_ORG_ID");
        return new SliplaneClient(key, org);
    }
}
