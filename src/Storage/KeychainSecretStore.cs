using System.Diagnostics;

namespace Sliplane.Console.Storage;

/// <summary>macOS Keychain, via the <c>security</c> command.</summary>
public sealed class KeychainSecretStore : ISecretStore
{
    private const string Service = "sliplane-cli";

    public string Name => "keychain";

    public string? Get(string key)
    {
        var (exit, stdout, _) = Run(["find-generic-password", "-s", Service, "-a", key, "-w"]);
        return exit == 0 ? stdout.TrimEnd('\n') : null;
    }

    public void Set(string key, string value)
    {
        // -U updates in place if the item already exists.
        var (exit, _, stderr) = Run(["add-generic-password", "-U", "-s", Service, "-a", key, "-w", value]);
        if (exit != 0)
            throw new Infrastructure.SliplaneException(
                Infrastructure.ErrorCode.Error, "Could not write to the macOS Keychain.", stderr.Trim());
    }

    public void Delete(string key) => Run(["delete-generic-password", "-s", Service, "-a", key]);

    private static (int Exit, string Stdout, string Stderr) Run(string[] args)
    {
        var info = new ProcessStartInfo("security")
        {
            RedirectStandardOutput = true,
            RedirectStandardError = true,
            UseShellExecute = false
        };
        foreach (var a in args) info.ArgumentList.Add(a);

        using var process = Process.Start(info)
            ?? throw new Infrastructure.SliplaneException(
                Infrastructure.ErrorCode.Error, "Could not start the 'security' command.");

        var stdout = process.StandardOutput.ReadToEnd();
        var stderr = process.StandardError.ReadToEnd();
        process.WaitForExit();
        return (process.ExitCode, stdout, stderr);
    }
}
