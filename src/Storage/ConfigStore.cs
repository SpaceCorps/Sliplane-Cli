using YamlDotNet.Serialization;
using YamlDotNet.Serialization.NamingConventions;

namespace Sliplane.Console.Storage;

public sealed class AccountConfig
{
    /// <summary>Organization ID sent as X-Organization-ID, for legacy tokens. Empty for scoped keys.</summary>
    public string OrgId { get; set; } = "";

    /// <summary>Whatever `me` called this key when it was added - an email, a name, an id.</summary>
    public string Identity { get; set; } = "";

    /// <summary>The organization `me` reported at that time. Observed, not sent - see OrgId.</summary>
    public string Organization { get; set; } = "";

    public string AddedAt { get; set; } = "";
}

public sealed class SliplaneConfig
{
    public int Version { get; set; } = 1;
    public Dictionary<string, AccountConfig> Accounts { get; set; } = new(StringComparer.OrdinalIgnoreCase);
}

/// <summary>
/// Non-secret account metadata in a readable YAML file. The API key itself never lands here -
/// that goes to <see cref="ISecretStore"/>.
/// </summary>
public static class ConfigStore
{
    public static string ConfigDir
    {
        get
        {
            var overridden = Environment.GetEnvironmentVariable("SLIPLANE_CONFIG_DIR");
            if (!string.IsNullOrWhiteSpace(overridden)) return overridden;

            if (OperatingSystem.IsWindows())
                return Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData), "sliplane-cli");

            var home = Environment.GetFolderPath(Environment.SpecialFolder.UserProfile);

            if (OperatingSystem.IsMacOS())
                return Path.Combine(home, "Library", "Application Support", "sliplane-cli");

            var xdg = Environment.GetEnvironmentVariable("XDG_CONFIG_HOME");
            return Path.Combine(string.IsNullOrWhiteSpace(xdg) ? Path.Combine(home, ".config") : xdg, "sliplane-cli");
        }
    }

    public static string ConfigPath => Path.Combine(ConfigDir, "config.yaml");
    public static string LockPath => Path.Combine(ConfigDir, ".lock");

    public static void EnsureDir()
    {
        var dir = ConfigDir;
        if (Directory.Exists(dir)) return;
        Directory.CreateDirectory(dir);
        RestrictToOwner(dir);
    }

    public static SliplaneConfig Load()
    {
        var path = ConfigPath;
        if (!File.Exists(path)) return new SliplaneConfig();

        var yaml = File.ReadAllText(path);
        if (string.IsNullOrWhiteSpace(yaml)) return new SliplaneConfig();

        var deserializer = new DeserializerBuilder()
            .WithNamingConvention(CamelCaseNamingConvention.Instance)
            .IgnoreUnmatchedProperties()
            .Build();

        var config = deserializer.Deserialize<SliplaneConfig>(yaml) ?? new SliplaneConfig();

        // Account lookup must be case-insensitive; YamlDotNet builds an ordinal dictionary.
        config.Accounts = new Dictionary<string, AccountConfig>(config.Accounts, StringComparer.OrdinalIgnoreCase);
        return config;
    }

    public static void Save(SliplaneConfig config)
    {
        EnsureDir();

        var serializer = new SerializerBuilder()
            .WithNamingConvention(CamelCaseNamingConvention.Instance)
            .Build();

        AtomicWrite(ConfigPath, serializer.Serialize(config));
    }

    /// <summary>Write to a temp file in the same directory, then replace - never a partial config.</summary>
    public static void AtomicWrite(string path, string contents)
    {
        var temp = path + ".tmp";
        File.WriteAllText(temp, contents);
        RestrictToOwner(temp);
        File.Move(temp, path, overwrite: true);
    }

    public static void AtomicWrite(string path, byte[] contents)
    {
        var temp = path + ".tmp";
        File.WriteAllBytes(temp, contents);
        RestrictToOwner(temp);
        File.Move(temp, path, overwrite: true);
    }

    /// <summary>0600 on Unix. On Windows the DPAPI blob is already user-scoped.</summary>
    public static void RestrictToOwner(string path)
    {
        if (OperatingSystem.IsWindows()) return;
        try
        {
            var mode = Directory.Exists(path)
                ? UnixFileMode.UserRead | UnixFileMode.UserWrite | UnixFileMode.UserExecute
                : UnixFileMode.UserRead | UnixFileMode.UserWrite;
            File.SetUnixFileMode(path, mode);
        }
        catch (Exception)
        {
            // Best effort - a filesystem that cannot express permissions is not a reason to fail.
        }
    }
}
