using System.Text.Json;

namespace Sliplane.Console.Infrastructure;

/// <summary>
/// One place where failures are rendered, so the envelope on stderr and the process exit code
/// always agree. Everything an agent needs to decide what to do next is in the envelope:
/// a stable <c>code</c>, the upstream <c>detail</c>, and a <c>remediation</c> command when
/// there is one worth running.
/// </summary>
public static class ErrorOutput
{
    public static int Write(Exception exception)
    {
        var ex = Translate(exception);

        var payload = new Dictionary<string, object?> { ["error"] = ex.Message, ["code"] = ErrorCodes.Name(ex.Code) };
        if (!string.IsNullOrWhiteSpace(ex.Detail)) payload["detail"] = ex.Detail;
        if (!string.IsNullOrWhiteSpace(ex.Remediation)) payload["remediation"] = ex.Remediation;

        var text = Output.UseJson
            ? JsonSerializer.Serialize(payload, Output.Indented)
            : YamlOutput.Serialize(payload);

        System.Console.Error.WriteLine(text);
        return (int)ex.Code;
    }

    private static SliplaneException Translate(Exception exception) => exception switch
    {
        SliplaneException ex => ex,

        // Thrown by argument checks that predate the error codes - PathArg and the option
        // validation inside the commands.
        InvalidOperationException ex => SliplaneException.Invalid(ex.Message),

        HttpRequestException ex => new SliplaneException(
            ErrorCode.Network, "Could not reach the Sliplane API.", ex.Message, "Retry once, then stop."),

        TaskCanceledException => new SliplaneException(
            ErrorCode.Network, "The request timed out.", null, "Retry once, then stop."),

        FileNotFoundException ex => SliplaneException.Invalid(ex.Message),
        DirectoryNotFoundException ex => SliplaneException.Invalid(ex.Message),

        _ => new SliplaneException(ErrorCode.Error, exception.Message, exception.GetType().Name)
    };
}
