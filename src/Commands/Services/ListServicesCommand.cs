using System.ComponentModel;
using System.Text.Json;
using Sliplane.Console.Infrastructure;
using Spectre.Console.Cli;

namespace Sliplane.Console.Commands.Services;

public sealed class ListServicesCommand : AsyncCommand<ListServicesCommand.Settings>
{
    public sealed class Settings : ApiSettings
    {
        [CommandOption("--project-id <ID>")]
        [Description("The ID of the project (lists all projects if omitted)")]
        public string? ProjectId { get; init; }
    }

    public override async Task<int> ExecuteAsync(CommandContext context, Settings settings)
    {
        var client = settings.CreateClient();

        if (settings.ProjectId is not null)
        {
            var result = await client.GetAsync($"projects/{settings.ProjectId}/services");
            YamlOutput.Write(result);
            return 0;
        }

        var projects = await client.GetAsync("projects");
        var allServices = new List<object?>();

        foreach (var project in projects.RootElement.EnumerateArray())
        {
            var projectId = project.GetProperty("id").GetString();
            var services = await client.GetAsync($"projects/{projectId}/services");
            foreach (var service in services.RootElement.EnumerateArray())
            {
                allServices.Add(JsonToObject(service));
            }
        }

        YamlOutput.WriteObject(allServices);
        return 0;
    }

    private static object? JsonToObject(JsonElement element) => element.ValueKind switch
    {
        JsonValueKind.Object => element.EnumerateObject().ToDictionary(p => p.Name, p => JsonToObject(p.Value)),
        JsonValueKind.Array => element.EnumerateArray().Select(JsonToObject).ToList(),
        JsonValueKind.String => element.GetString(),
        JsonValueKind.Number => element.TryGetInt64(out var l) ? l : element.GetDouble(),
        JsonValueKind.True => true,
        JsonValueKind.False => false,
        _ => null
    };
}
