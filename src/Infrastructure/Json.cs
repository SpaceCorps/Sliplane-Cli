using System.Text.Json;

namespace Sliplane.Console.Infrastructure;

/// <summary>
/// Turns a parsed API response into plain objects, so it can be nested inside a payload this
/// CLI builds itself rather than printed on its own.
/// </summary>
public static class Json
{
    public static object? ToObject(JsonElement element) => element.ValueKind switch
    {
        JsonValueKind.Object => element.EnumerateObject().ToDictionary(p => p.Name, p => ToObject(p.Value)),
        JsonValueKind.Array => element.EnumerateArray().Select(ToObject).ToList(),
        JsonValueKind.String => element.GetString(),
        JsonValueKind.Number => element.TryGetInt64(out var l) ? l : element.GetDouble(),
        JsonValueKind.True => true,
        JsonValueKind.False => false,
        _ => null
    };
}
