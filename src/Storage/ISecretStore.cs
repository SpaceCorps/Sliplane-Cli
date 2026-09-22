namespace Sliplane.Console.Storage;

/// <summary>
/// Anything bearer-shaped - today that is one API key per account.
/// Keys are <c>account:{name}</c>.
/// </summary>
public interface ISecretStore
{
    /// <summary>Backend name, as reported by <c>sliplane accounts list</c>.</summary>
    string Name { get; }

    string? Get(string key);
    void Set(string key, string value);
    void Delete(string key);
}

public static class SecretKeys
{
    public static string Account(string name) => "account:" + name.ToLowerInvariant();
}
