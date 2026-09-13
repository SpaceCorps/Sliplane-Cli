using System.Text.Json;

namespace Sliplane.Console.Infrastructure;

/// <summary>
/// Renders command output as YAML by default, or raw JSON with --json.
///
/// YAML reads well but is awkward to script against: pulling an id out of a
/// listing means line-pairing or a regex, and both break quietly when the
/// shape changes. --json exists so callers can pipe into jq instead.
/// </summary>
public static class Output
{
    public static bool UseJson { get; set; }

    private static readonly JsonSerializerOptions Indented = new() { WriteIndented = true };

    public static void Write(JsonDocument doc)
    {
        if (UseJson)
        {
            System.Console.WriteLine(JsonSerializer.Serialize(doc.RootElement, Indented));
            return;
        }
        YamlOutput.Write(doc);
    }

    public static void WriteObject(object obj)
    {
        if (UseJson)
        {
            System.Console.WriteLine(JsonSerializer.Serialize(obj, Indented));
            return;
        }
        YamlOutput.WriteObject(obj);
    }
}
