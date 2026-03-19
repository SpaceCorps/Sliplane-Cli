using System.ComponentModel;
using Sliplane.Console.Infrastructure;
using Spectre.Console.Cli;

namespace Sliplane.Console.Commands.Services;

public sealed class UpdateServiceCommand : AsyncCommand<UpdateServiceCommand.Settings>
{
    public sealed class Settings : ApiSettings
    {
        [CommandOption("--project-id <ID>")]
        [Description("The ID of the project")]
        public required string ProjectId { get; init; }

        [CommandOption("--service-id <ID>")]
        [Description("The ID of the service")]
        public required string ServiceId { get; init; }

        [CommandOption("--name <NAME>")]
        [Description("New name for the service")]
        public string? Name { get; init; }

        [CommandOption("--image <URL>")]
        [Description("New container image URL")]
        public string? Image { get; init; }

        [CommandOption("--repo <URL>")]
        [Description("New repository URL")]
        public string? Repo { get; init; }

        [CommandOption("--branch <BRANCH>")]
        [Description("Branch to deploy from")]
        public string? Branch { get; init; }

        [CommandOption("--dockerfile <PATH>")]
        [Description("Path to Dockerfile")]
        public string? DockerfilePath { get; init; }

        [CommandOption("--docker-context <PATH>")]
        [Description("Docker build context")]
        public string? DockerContext { get; init; }

        [CommandOption("--auto-deploy")]
        [Description("Auto-deploy on push")]
        public bool? AutoDeploy { get; init; }

        [CommandOption("--registry-credential-id <ID>")]
        [Description("Registry credential ID")]
        public string? RegistryCredentialId { get; init; }

        [CommandOption("--healthcheck <PATH>")]
        [Description("Health check path")]
        public string? Healthcheck { get; init; }

        [CommandOption("--cmd <CMD>")]
        [Description("Override Docker CMD")]
        public string? Cmd { get; init; }

        [CommandOption("--env <KEY=VALUE>")]
        [Description("Environment variable (repeatable, replaces all)")]
        public string[]? Env { get; init; }

        [CommandOption("--secret-env <KEY=VALUE>")]
        [Description("Secret environment variable (repeatable)")]
        public string[]? SecretEnv { get; init; }
    }

    public override async Task<int> ExecuteAsync(CommandContext context, Settings settings)
    {
        var client = settings.CreateClient();
        var body = BuildBody(settings);
        var result = await client.PatchAsync($"projects/{settings.ProjectId}/services/{settings.ServiceId}", body);
        YamlOutput.Write(result);
        return 0;
    }

    private static Dictionary<string, object> BuildBody(Settings s)
    {
        var body = new Dictionary<string, object>();

        if (!string.IsNullOrEmpty(s.Name)) body["name"] = s.Name;
        if (!string.IsNullOrEmpty(s.Healthcheck)) body["healthcheck"] = s.Healthcheck;
        if (!string.IsNullOrEmpty(s.Cmd)) body["cmd"] = s.Cmd;

        if (!string.IsNullOrEmpty(s.Image))
        {
            var deployment = new Dictionary<string, object> { ["url"] = s.Image };
            if (!string.IsNullOrEmpty(s.RegistryCredentialId))
                deployment["registryAuthenticationId"] = s.RegistryCredentialId;
            body["deployment"] = deployment;
        }
        else if (!string.IsNullOrEmpty(s.Repo))
        {
            var deployment = new Dictionary<string, object> { ["url"] = s.Repo };
            if (!string.IsNullOrEmpty(s.Branch)) deployment["branch"] = s.Branch;
            if (!string.IsNullOrEmpty(s.DockerfilePath)) deployment["dockerfilePath"] = s.DockerfilePath;
            if (!string.IsNullOrEmpty(s.DockerContext)) deployment["dockerContext"] = s.DockerContext;
            if (s.AutoDeploy.HasValue) deployment["autoDeploy"] = s.AutoDeploy.Value;
            body["deployment"] = deployment;
        }

        var envVars = new List<Dictionary<string, object>>();
        if (s.Env is not null)
        {
            foreach (var e in s.Env)
            {
                var parts = e.Split('=', 2);
                envVars.Add(new Dictionary<string, object> { ["key"] = parts[0], ["value"] = parts.Length > 1 ? parts[1] : "", ["secret"] = false });
            }
        }
        if (s.SecretEnv is not null)
        {
            foreach (var e in s.SecretEnv)
            {
                var parts = e.Split('=', 2);
                envVars.Add(new Dictionary<string, object> { ["key"] = parts[0], ["value"] = parts.Length > 1 ? parts[1] : "", ["secret"] = true });
            }
        }
        if (envVars.Count > 0) body["env"] = envVars;

        return body;
    }
}
