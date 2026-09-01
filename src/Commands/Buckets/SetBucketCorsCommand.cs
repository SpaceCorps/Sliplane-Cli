using System.ComponentModel;
using System.Text.Json;
using Sliplane.Console.Infrastructure;
using Spectre.Console.Cli;

namespace Sliplane.Console.Commands.Buckets;

public sealed class SetBucketCorsCommand : AsyncCommand<SetBucketCorsCommand.Settings>
{
    public sealed class Settings : ApiSettings
    {
        [CommandOption("--bucket-id <ID>")]
        [Description("The ID of the bucket")]
        public required string BucketId { get; init; }

        [CommandOption("--file <PATH>")]
        [Description("JSON file with the full CORS configuration (up to 10 rules)")]
        public string? File { get; init; }

        [CommandOption("--allowed-origin <ORIGIN>")]
        [Description("Allowed origin, e.g. https://app.example.com or * (repeatable)")]
        public string[]? AllowedOrigins { get; init; }

        [CommandOption("--allowed-method <METHOD>")]
        [Description("Allowed method: GET, PUT, POST, DELETE, HEAD (repeatable)")]
        public string[]? AllowedMethods { get; init; }

        [CommandOption("--allowed-header <HEADER>")]
        [Description("Request header the browser may send (repeatable)")]
        public string[]? AllowedHeaders { get; init; }

        [CommandOption("--expose-header <HEADER>")]
        [Description("Response header the browser may read (repeatable)")]
        public string[]? ExposeHeaders { get; init; }

        [CommandOption("--max-age <SECONDS>")]
        [Description("How long the browser may cache the preflight response")]
        public int? MaxAgeSeconds { get; init; }
    }

    public override async Task<int> ExecuteAsync(CommandContext context, Settings settings)
    {
        var client = settings.CreateClient();
        var result = await client.PutAsync($"buckets/{settings.BucketId}/cors", BuildBody(settings));
        YamlOutput.Write(result);
        return 0;
    }

    private static object BuildBody(Settings s)
    {
        if (!string.IsNullOrEmpty(s.File))
        {
            var json = JsonDocument.Parse(System.IO.File.ReadAllText(s.File));
            // Accept either the full config object or a bare array of rules
            return json.RootElement.ValueKind == JsonValueKind.Array
                ? new Dictionary<string, object> { ["rules"] = json.RootElement }
                : json.RootElement;
        }

        if (s.AllowedOrigins is null or { Length: 0 } || s.AllowedMethods is null or { Length: 0 })
            throw new InvalidOperationException(
                "Provide --file, or at least one --allowed-origin and one --allowed-method.");

        var rule = new Dictionary<string, object>
        {
            ["allowedOrigins"] = s.AllowedOrigins,
            ["allowedMethods"] = s.AllowedMethods.Select(m => m.ToUpperInvariant()).ToArray()
        };

        if (s.AllowedHeaders is { Length: > 0 }) rule["allowedHeaders"] = s.AllowedHeaders;
        if (s.ExposeHeaders is { Length: > 0 }) rule["exposeHeaders"] = s.ExposeHeaders;
        if (s.MaxAgeSeconds.HasValue) rule["maxAgeSeconds"] = s.MaxAgeSeconds.Value;

        return new Dictionary<string, object> { ["rules"] = new[] { rule } };
    }
}
