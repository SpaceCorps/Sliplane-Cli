using System.ComponentModel;
using Spectre.Console.Cli;

namespace Slipline.Console.Infrastructure;

public class ApiSettings : CommandSettings
{
    [CommandOption("--api-key <KEY>")]
    [Description("Sliplane API key (or set SLIPLINE_API_KEY env var)")]
    public string? ApiKey { get; init; }

    [CommandOption("--org-id <ORG_ID>")]
    [Description("Organization ID for legacy tokens (or set SLIPLINE_ORG_ID env var)")]
    public string? OrgId { get; init; }

    public SliplaneClient CreateClient()
    {
        var key = ApiKey ?? Environment.GetEnvironmentVariable("SLIPLINE_API_KEY")
            ?? throw new InvalidOperationException("API key required. Use --api-key or set SLIPLINE_API_KEY.");
        var org = OrgId ?? Environment.GetEnvironmentVariable("SLIPLINE_ORG_ID");
        return new SliplaneClient(key, org);
    }
}
