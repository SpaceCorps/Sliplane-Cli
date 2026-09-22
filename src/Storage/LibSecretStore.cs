using System.Diagnostics;

namespace Sliplane.Console.Storage;

/// <summary>
/// Linux: the freedesktop secret service, via <c>secret-tool</c>. The secret goes in on stdin
/// rather than argv, so it never appears in the process list.
/// </summary>
public sealed class LibSecretStore : ISecretStore
{
    private const string Service = "sliplane-cli";

    public string Name => "libsecret";

    public string? Get(string key)
    {
        var (exit, stdout, _) = Run(["lookup", "service", Service, "account", key], null);
        return exit == 0 && stdout.Length > 0 ? stdout.TrimEnd('\n') : null;
    }

    public void Set(string key, string value)
    {
        var (exit, _, stderr) = Run(
            ["store", "--label=sliplane-cli: " + key, "service", Service, "account", key], value);

        if (exit != 0)
            throw new Infrastructure.SliplaneException(
                Infrastructure.ErrorCode.Error,
                "Could not write to the system keyring.",
                stderr.Trim(),
                "Check that a secret service (gnome-keyring, kwallet) is running and unlocked.");
    }

    public void Delete(string key) => Run(["clear", "service", Service, "account", key], null);

    private static (int Exit, string Stdout, string Stderr) Run(string[] args, string? stdin)
    {
        var info = new ProcessStartInfo("secret-tool")
        {
            RedirectStandardOutput = true,
            RedirectStandardError = true,
            RedirectStandardInput = stdin is not null,
            UseShellExecute = false
        };
        foreach (var a in args) info.ArgumentList.Add(a);

        using var process = Process.Start(info)
            ?? throw new Infrastructure.SliplaneException(
                Infrastructure.ErrorCode.Error, "Could not start 'secret-tool'.");

        if (stdin is not null)
        {
            process.StandardInput.Write(stdin);
            process.StandardInput.Close();
        }

        var stdout = process.StandardOutput.ReadToEnd();
        var stderr = process.StandardError.ReadToEnd();
        process.WaitForExit();
        return (process.ExitCode, stdout, stderr);
    }
}
