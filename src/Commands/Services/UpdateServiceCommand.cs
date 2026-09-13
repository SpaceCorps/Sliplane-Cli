using System.Text.Json;
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

        [CommandOption("--deploy-include-paths <PATTERN>")]
        [Description("Only deploy if changes in these paths (repeatable, replaces all)")]
        public string[]? DeployIncludePaths { get; init; }

        [CommandOption("--deploy-ignore-paths <PATTERN>")]
        [Description("Skip deploy if changes only in these paths (repeatable, replaces all)")]
        public string[]? DeployIgnorePaths { get; init; }

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
        [Description("Environment variable (repeatable). REPLACES the whole environment - pass every variable, or use set-env for one")]
        public string[]? Env { get; init; }

        [CommandOption("--replace-env")]
        [Description("Allow --env/--secret-env to drop variables not listed. Without it, an update that would delete a variable is refused")]
        public bool ReplaceEnv { get; init; }

        [CommandOption("--secret-env <KEY=VALUE>")]
        [Description("Secret environment variable (repeatable). Shares one list with --env, so it replaces the whole environment too")]
        public string[]? SecretEnv { get; init; }
    }

    public override async Task<int> ExecuteAsync(CommandContext context, Settings settings)
    {
        var client = settings.CreateClient();
        var body = await BuildBodyAsync(client, settings);
        var result = await client.PatchAsync($"projects/{settings.ProjectId}/services/{settings.ServiceId}", body);
        Output.Write(result);
        return 0;
    }

    private static async Task<Dictionary<string, object>> BuildBodyAsync(SliplaneClient client, Settings s)
    {
        var body = new Dictionary<string, object>();

        if (!string.IsNullOrEmpty(s.Name)) body["name"] = s.Name;
        if (!string.IsNullOrEmpty(s.Healthcheck)) body["healthcheck"] = PathArg.Check(s.Healthcheck, "--healthcheck");
        if (!string.IsNullOrEmpty(s.Cmd)) body["cmd"] = s.Cmd;

        var deployment = new Dictionary<string, object>();
        // The API wants a deployment object on every PATCH, so any update has to
        // carry the current one over - not just the deploy-related flags. Without
        // this, `--healthcheck`, `--name` or `--cmd` on their own come back with
        // "Deployment configuration is required".
        var needsDeploymentUrl = true;

        if (!string.IsNullOrEmpty(s.Image))
        {
            deployment["url"] = s.Image;
            if (!string.IsNullOrEmpty(s.RegistryCredentialId))
                deployment["registryAuthenticationId"] = s.RegistryCredentialId;
        }
        else if (!string.IsNullOrEmpty(s.Repo))
        {
            deployment["url"] = s.Repo;
            if (!string.IsNullOrEmpty(s.Branch)) deployment["branch"] = s.Branch;
            if (!string.IsNullOrEmpty(s.DockerfilePath)) deployment["dockerfilePath"] = s.DockerfilePath;
            if (!string.IsNullOrEmpty(s.DockerContext)) deployment["dockerContext"] = s.DockerContext;
        }
        else if (needsDeploymentUrl)
        {
            // API requires url in deployment object; fetch current service to carry it over
            var current = await client.GetAsync($"projects/{s.ProjectId}/services/{s.ServiceId}");
            var dep = current.RootElement.GetProperty("deployment");
            deployment["url"] = dep.GetProperty("url").GetString()!;
        }

        if (s.AutoDeploy.HasValue) deployment["autoDeploy"] = s.AutoDeploy.Value;
        if (s.DeployIncludePaths is not null) deployment["includePaths"] = s.DeployIncludePaths;
        if (s.DeployIgnorePaths is not null) deployment["ignorePaths"] = s.DeployIgnorePaths;

        if (deployment.Count > 0) body["deployment"] = deployment;

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
        if (envVars.Count > 0)
        {
            // The API replaces the whole env array rather than merging, so an
            // update that lists two variables on a service holding three deletes
            // the third - silently, and the service only fails later when it
            // cannot find it. Merging here is not an option either: secrets read
            // back masked, so carrying them over would write empty strings over
            // real values.
            //
            // So refuse instead, and name what would be lost.
            if (!s.ReplaceEnv)
            {
                var current = await client.GetAsync($"projects/{s.ProjectId}/services/{s.ServiceId}");
                if (current.RootElement.TryGetProperty("env", out var existing) &&
                    existing.ValueKind == JsonValueKind.Array)
                {
                    var supplied = envVars.Select(v => (string)v["key"]).ToHashSet(StringComparer.Ordinal);
                    var dropped = existing.EnumerateArray()
                        .Select(e => e.TryGetProperty("key", out var k) ? k.GetString() : null)
                        .Where(k => k is not null && !supplied.Contains(k))
                        .ToList();

                    if (dropped.Count > 0)
                        throw new InvalidOperationException(
                            $"This would delete {dropped.Count} environment variable(s) not listed: {string.Join(", ", dropped)}. " +
                            "--env replaces the whole environment. Change one variable with " +
                            "`services set-env`, or pass --replace-env if deleting them is intended.");
                }
            }

            body["env"] = envVars;
        }

        return body;
    }
}
