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

        var body = await response.Content.ReadAsStringAsync();
        var hint = Hint(response.StatusCode, body);
        throw new HttpRequestException($"HTTP {(int)response.StatusCode}: {body}{hint}");
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
