using System.Diagnostics;
using Sliplane.Console.Infrastructure;

namespace Sliplane.Console.Storage;

public static class SecretStoreFactory
{
    private static ISecretStore? _cached;

    public static ISecretStore Create() => _cached ??= Build();

    private static ISecretStore Build()
    {
        var forced = Environment.GetEnvironmentVariable("SLIPLANE_SECRET_STORE");
        if (!string.IsNullOrWhiteSpace(forced))
        {
            return forced.ToLowerInvariant() switch
            {
                "dpapi" when OperatingSystem.IsWindows() => new DpapiSecretStore(),
                "keychain" => new KeychainSecretStore(),
                "libsecret" => new LibSecretStore(),
                "plaintext" => new PlaintextSecretStore(),
                _ => throw SliplaneException.Invalid(
                    $"SLIPLANE_SECRET_STORE='{forced}' is not a backend available on this platform.",
                    "Unset SLIPLANE_SECRET_STORE, or set it to one of: dpapi, keychain, libsecret, plaintext.")
            };
        }

        if (OperatingSystem.IsWindows()) return new DpapiSecretStore();

        if (OperatingSystem.IsMacOS())
        {
            if (CommandExists("security")) return new KeychainSecretStore();
            return Fallback("The macOS 'security' command was not found.");
        }

        if (CommandExists("secret-tool")) return new LibSecretStore();

        return Fallback(
            "libsecret is not installed, so there is no OS keystore to hold your API keys. " +
            "Install it with: sudo apt install libsecret-tools  (or the equivalent for your distro).");
    }

    /// <summary>
    /// Degrading silently to a plaintext file would quietly undo the reason for storing keys
    /// outside the environment at all, so it has to be asked for explicitly.
    /// </summary>
    private static ISecretStore Fallback(string reason)
    {
        if (Environment.GetEnvironmentVariable("SLIPLANE_ALLOW_PLAINTEXT_STORE") == "1")
            return new PlaintextSecretStore();

        throw new SliplaneException(
            ErrorCode.Error,
            "No secure credential store is available on this machine.",
            reason,
            "Install a keystore, or set SLIPLANE_ALLOW_PLAINTEXT_STORE=1 to store API keys in a 0600 file instead.");
    }

    private static bool CommandExists(string command)
    {
        try
        {
            using var process = Process.Start(new ProcessStartInfo
            {
                FileName = "/usr/bin/env",
                ArgumentList = { "which", command },
                RedirectStandardOutput = true,
                RedirectStandardError = true,
                UseShellExecute = false
            });
            if (process is null) return false;
            process.WaitForExit(3000);
            return process.ExitCode == 0;
        }
        catch (Exception)
        {
            return false;
        }
    }
}
