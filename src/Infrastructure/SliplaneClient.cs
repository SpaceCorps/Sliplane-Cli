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
        if (!response.IsSuccessStatusCode)
        {
            var body = await response.Content.ReadAsStringAsync();
            throw new HttpRequestException($"HTTP {(int)response.StatusCode}: {body}");
        }
    }
}
