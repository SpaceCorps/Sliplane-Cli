using System.Text.Json;

namespace Sliplane.Console.Auth;

/// <summary>
/// Reads the two things worth remembering from <c>me</c>: who the key belongs to, and which
/// organization it is scoped to. Both are recorded when an account is added and shown whenever
/// accounts are listed - with several accounts on one machine, the name alone does not say which
/// one a command is about to change.
///
/// The response differs between a scoped key and a legacy token, so this probes the fields that
/// have been seen rather than binding to one shape. An unrecognised response yields an empty
/// label, which is cosmetic: the account still works.
/// </summary>
public static class SliplaneIdentity
{
    private static readonly string[] Candidates =
        ["email", "userEmail", "user_email", "name", "userName", "user_name", "login", "id"];

    /// <summary>A short human label - today the account email, as <c>me.user.email</c>.</summary>
    public static string Describe(JsonDocument me)
    {
        var root = me.RootElement;
        if (root.ValueKind != JsonValueKind.Object) return "";

        return FirstString(root, Candidates)
               ?? Nested(root, "user")
               ?? "";
    }

    /// <summary>
    /// The organization the key reports, which is not the same thing as the <c>--org-id</c> a
    /// legacy token has to send: this one is observed, that one is configured.
    /// </summary>
    public static string Organization(JsonDocument me)
    {
        var root = me.RootElement;
        if (root.ValueKind != JsonValueKind.Object) return "";

        return FirstString(root, ["organizationId", "organization_id", "orgId"])
               ?? Nested(root, "organization", ["id", "name"])
               ?? "";
    }

    private static string? Nested(JsonElement root, string property, string[]? names = null) =>
        root.TryGetProperty(property, out var child) && child.ValueKind == JsonValueKind.Object
            ? FirstString(child, names ?? Candidates)
            : null;

    private static string? FirstString(JsonElement element, string[] names)
    {
        foreach (var name in names)
            if (element.TryGetProperty(name, out var value) &&
                value.ValueKind == JsonValueKind.String &&
                !string.IsNullOrWhiteSpace(value.GetString()))
                return value.GetString();

        return null;
    }
}
