using System.Runtime.Versioning;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json;

namespace Sliplane.Console.Storage;

/// <summary>
/// Windows: a single JSON blob encrypted with DPAPI under the current user, so it can only be
/// decrypted by this Windows account on this machine. No key material of ours to manage.
/// </summary>
[SupportedOSPlatform("windows")]
public sealed class DpapiSecretStore : ISecretStore
{
    public string Name => "dpapi";

    private static string Path => System.IO.Path.Combine(ConfigStore.ConfigDir, "secrets.dpapi");

    public string? Get(string key) => Read().GetValueOrDefault(key);

    public void Set(string key, string value)
    {
        var all = Read();
        all[key] = value;
        Write(all);
    }

    public void Delete(string key)
    {
        var all = Read();
        if (all.Remove(key)) Write(all);
    }

    private static Dictionary<string, string> Read()
    {
        var path = Path;
        if (!File.Exists(path)) return new Dictionary<string, string>();

        try
        {
            var protectedBytes = File.ReadAllBytes(path);
            var plain = ProtectedData.Unprotect(protectedBytes, null, DataProtectionScope.CurrentUser);
            return JsonSerializer.Deserialize<Dictionary<string, string>>(Encoding.UTF8.GetString(plain))
                   ?? new Dictionary<string, string>();
        }
        catch (CryptographicException ex)
        {
            throw new Infrastructure.SliplaneException(
                Infrastructure.ErrorCode.Error,
                "The stored API keys could not be decrypted.",
                $"DPAPI failed: {ex.Message}. This happens when the file was written by a different Windows user or on a different machine.",
                "Delete " + path + " and run: sliplane accounts add <name>");
        }
    }

    private static void Write(Dictionary<string, string> all)
    {
        ConfigStore.EnsureDir();
        var json = Encoding.UTF8.GetBytes(JsonSerializer.Serialize(all));
        var encrypted = ProtectedData.Protect(json, null, DataProtectionScope.CurrentUser);
        ConfigStore.AtomicWrite(Path, encrypted);
    }
}
