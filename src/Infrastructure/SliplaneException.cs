namespace Sliplane.Console.Infrastructure;

/// <summary>
/// Exit codes, doubling as the machine-readable <c>code:</c> field in the error envelope.
/// An agent branches on these, so they must stay stable.
/// </summary>
public enum ErrorCode
{
    Ok = 0,
    Error = 1,
    Network = 2,
    AuthRequired = 3,
    NotFound = 4,
    RateLimited = 5,
    InvalidInput = 6,
    NoAccount = 7
}

public static class ErrorCodes
{
    public static string Name(ErrorCode code) => code switch
    {
        ErrorCode.Ok => "ok",
        ErrorCode.Network => "network",
        ErrorCode.AuthRequired => "auth_required",
        ErrorCode.NotFound => "not_found",
        ErrorCode.RateLimited => "rate_limited",
        ErrorCode.InvalidInput => "invalid_input",
        ErrorCode.NoAccount => "no_account",
        _ => "error"
    };
}

public class SliplaneException : Exception
{
    public ErrorCode Code { get; }

    /// <summary>Extra context - the upstream response body, the list of known accounts, and so on.</summary>
    public string? Detail { get; }

    /// <summary>A literal command that fixes this. Agents surface it verbatim.</summary>
    public string? Remediation { get; }

    public SliplaneException(ErrorCode code, string message, string? detail = null, string? remediation = null)
        : base(message)
    {
        Code = code;
        Detail = detail;
        Remediation = remediation;
    }

    public static SliplaneException Invalid(string message, string? remediation = null) =>
        new(ErrorCode.InvalidInput, message, remediation: remediation);

    public static SliplaneException NotFound(string message, string? detail = null) =>
        new(ErrorCode.NotFound, message, detail);

    public static SliplaneException Auth(string message, string? detail = null, string? remediation = null) =>
        new(ErrorCode.AuthRequired, message, detail, remediation);
}
