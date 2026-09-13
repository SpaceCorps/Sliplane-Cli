namespace Sliplane.Console.Infrastructure;

/// <summary>
/// Catches path arguments that a Unix-style shell on Windows has rewritten.
///
/// Git Bash and MSYS translate arguments that look like absolute Unix paths
/// into Windows ones before the process ever sees them, so `--healthcheck /`
/// arrives as "C:/Program Files/Git/". Setting MSYS_NO_PATHCONV=1 does not
/// help, because the rewriting happens in the shell.
///
/// The result deploys quite happily and then fails every healthcheck, with
/// nothing in the output to suggest why. Better to refuse up front.
/// </summary>
public static class PathArg
{
    public static string Check(string? value, string option)
    {
        if (string.IsNullOrEmpty(value)) return value ?? string.Empty;

        // A URL path never contains a drive-letter separator.
        var looksMangled = value.Contains(":/", StringComparison.Ordinal)
                        || value.Contains(@":\", StringComparison.Ordinal);

        if (looksMangled)
            throw new InvalidOperationException(
                $"{option} was given \"{value}\", which is a Windows path rather than a URL path."
                + "\n\nA Unix-style shell on Windows rewrites arguments starting with \"/\" before "
                + "this program sees them, turning \"/\" into your Git installation directory. "
                + "MSYS_NO_PATHCONV does not prevent it."
                + "\n\nRun this from PowerShell or cmd, or double the slash: //health");

        return value;
    }
}
