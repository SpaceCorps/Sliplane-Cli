using System.Text.Json;

namespace Sliplane.Console.Storage;

/// <summary>
/// The opt-in fallback for machines with no keystore. Warns on every read rather than only when
/// the account is added, because the whole risk of this backend is that you forget you chose it.
/// </summary>
public sealed class PlaintextSecretStore : ISecretStore
{
    private static bool _warned;

    public string Name => "plaintext";

    private static string Path => System.IO.Path.Combine(ConfigStore.ConfigDir, "secrets.json");

    public string? Get(string key)
    {
        Warn();
        return Read().GetValueOrDefault(key);
    }

    public void Set(string key, string value)
    {
        Warn();
        var all = Read();
        all[key] = value;
        Write(all);
    }

    public void Delete(string key)
    {
        var all = Read();
        if (all.Remove(key)) Write(all);
    }

    private static void Warn()
    {
        if (_warned) return;
        _warned = true;
        System.Console.Error.WriteLine(
            $"warning: API keys are stored unencrypted in {Path} (SLIPLANE_ALLOW_PLAINTEXT_STORE=1).");
    }

    private static Dictionary<string, string> Read()
    {
        var path = Path;
        if (!File.Exists(path)) return new Dictionary<string, string>();
        return JsonSerializer.Deserialize<Dictionary<string, string>>(File.ReadAllText(path))
               ?? new Dictionary<string, string>();
    }

    private static void Write(Dictionary<string, string> all)
    {
        ConfigStore.EnsureDir();
        ConfigStore.AtomicWrite(Path, JsonSerializer.Serialize(all, new JsonSerializerOptions { WriteIndented = true }));
        ConfigStore.RestrictToOwner(Path);
    }
}
