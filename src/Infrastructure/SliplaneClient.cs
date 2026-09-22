using System.Net.Http.Headers;
using System.Text;
using System.Text.Json;

namespace Sliplane.Console.Infrastructure;

public sealed class SliplaneClient
{
    private static readonly JsonSerializerOptions JsonOptions = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.CamelCase,
        WriteIndented = false
    };

    private readonly HttpClient _http;

    public SliplaneClient(string apiKey, string? orgId = null)
    {
        _http = new HttpClient { BaseAddress = new Uri("https://ctrl.sliplane.io/v0/") };
        _http.DefaultRequestHeaders.Authorization = new AuthenticationHeaderValue("Bearer", apiKey);
        if (!string.IsNullOrEmpty(orgId))
            _http.DefaultRequestHeaders.Add("X-Organization-ID", orgId);
    }

    public async Task<JsonDocument> GetAsync(string path)
    {
        var response = await _http.GetAsync(path);
        await EnsureSuccess(response);
        var stream = await response.Content.ReadAsStreamAsync();
        return await JsonDocument.ParseAsync(stream);
    }

    public async Task<JsonDocument> PostAsync(string path, object? body = null)
    {
        var content = body is null ? null : new StringContent(JsonSerializer.Serialize(body, JsonOptions), Encoding.UTF8, "application/json");
        var response = await _http.PostAsync(path, content);
        await EnsureSuccess(response);
        return await ReadJson(response);
    }

    public async Task<JsonDocument> PatchAsync(string path, object body)
    {
        var content = new StringContent(JsonSerializer.Serialize(body, JsonOptions), Encoding.UTF8, "application/json");
        var response = await _http.PatchAsync(path, content);
        await EnsureSuccess(response);
        var stream = await response.Content.ReadAsStreamAsync();
        return await JsonDocument.ParseAsync(stream);
    }

    public async Task<JsonDocument> PutAsync(string path, object body)
    {
        var content = new StringContent(JsonSerializer.Serialize(body, JsonOptions), Encoding.UTF8, "application/json");
        var response = await _http.PutAsync(path, content);
        await EnsureSuccess(response);
        return await ReadJson(response);
    }

    public async Task DeleteAsync(string path)
    {
        var response = await _http.DeleteAsync(path);
        await EnsureSuccess(response);
    }

    public async Task<JsonDocument> DeleteWithResponseAsync(string path)
    {
        var response = await _http.DeleteAsync(path);
        await EnsureSuccess(response);
        return await ReadJson(response);
    }

    private static async Task<JsonDocument> ReadJson(HttpResponseMessage response)
    {
        if (response.StatusCode == System.Net.HttpStatusCode.NoContent || response.Content.Headers.ContentLength == 0)
            return JsonDocument.Parse("{}");
        var body = await response.Content.ReadAsStringAsync();
        return string.IsNullOrWhiteSpace(body) ? JsonDocument.Parse("{}") : JsonDocument.Parse(body);
    }

    private static async Task EnsureSuccess(HttpResponseMessage response)
    {
        if (response.IsSuccessStatusCode) return;

        var status = (int)response.StatusCode;
        var body = (await response.Content.ReadAsStringAsync()).Trim();
        var detail = $"HTTP {status}" + (body.Length == 0 ? "" : ": " + body) + Hint(response.StatusCode, body);

        throw response.StatusCode switch
        {
            System.Net.HttpStatusCode.Unauthorized => SliplaneException.Auth(
                "The API key was rejected.", detail,
                "Replace it: sliplane accounts add <name> --api-key <key> --force"),

            System.Net.HttpStatusCode.Forbidden => SliplaneException.Auth(
                "The API key is not allowed to do that.", detail,
                "A legacy token also needs an organization: sliplane accounts add <name> --api-key <key> --org-id <id>"),

            System.Net.HttpStatusCode.NotFound => SliplaneException.NotFound(
                "The resource does not exist.", detail),

            System.Net.HttpStatusCode.TooManyRequests => new SliplaneException(
                ErrorCode.RateLimited, "Rate limited by the Sliplane API.", detail,
                "Back off before retrying."),

            System.Net.HttpStatusCode.BadRequest or System.Net.HttpStatusCode.UnprocessableEntity =>
                new SliplaneException(ErrorCode.InvalidInput, "The API refused the request.", detail),

            _ when status >= 500 => new SliplaneException(
                ErrorCode.Network, "The Sliplane API returned a server error.", detail,
                "Retry; if it persists the platform is having trouble."),

            _ => new SliplaneException(ErrorCode.Error, "The request failed.", detail)
        };
    }

    /// <summary>
    /// Turns the API's terser refusals into something actionable. The status and
    /// body are still shown verbatim; this only appends what to do about it.
    /// </summary>
    private static string Hint(System.Net.HttpStatusCode status, string body)
    {
        if (status == System.Net.HttpStatusCode.Conflict &&
            body.Contains("deployment", StringComparison.OrdinalIgnoreCase))
            return "\n\nA service cannot move between a registry image and a repository build. " +
                   "Delete and recreate it with the deployment you want - volumes are " +
                   "server-level resources and survive, so data on them is not lost.";

        if (status == System.Net.HttpStatusCode.BadRequest &&
            body.Contains("Deployment configuration is required", StringComparison.OrdinalIgnoreCase))
            return "\n\nEvery update needs the deployment object. Pass --repo/--branch " +
                   "(or --image) alongside whatever you are changing.";

        return string.Empty;
    }
}
