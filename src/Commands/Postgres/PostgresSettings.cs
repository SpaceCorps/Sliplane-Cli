using System.ComponentModel;
using Sliplane.Console.Infrastructure;
using Spectre.Console.Cli;

namespace Sliplane.Console.Commands.Postgres;

public class PostgresSettings : ApiSettings
{
    [CommandOption("--postgres-id <ID>")]
    [Description("The ID of the Postgres database")]
    public required string PostgresId { get; init; }
}

internal static class IpAllowList
{
    /// <summary>
    /// Parses <c>CIDR</c> or <c>CIDR=description</c> entries into the API's ipAllowList shape.
    /// </summary>
    public static List<Dictionary<string, object>> Parse(string[] entries)
    {
        var list = new List<Dictionary<string, object>>();
        foreach (var entry in entries)
        {
            var parts = entry.Split('=', 2);
            var item = new Dictionary<string, object> { ["source"] = parts[0].Trim() };
            if (parts.Length > 1) item["description"] = parts[1];
            list.Add(item);
        }
        return list;
    }
}
