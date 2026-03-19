using Sliplane.Console.Commands;
using Sliplane.Console.Commands.Credentials;
using Sliplane.Console.Commands.OAuth;
using Sliplane.Console.Commands.Projects;
using Sliplane.Console.Commands.Servers;
using Sliplane.Console.Commands.Services;
using Spectre.Console.Cli;

var app = new CommandApp();

app.Configure(config =>
{
    config.AddCommand<MeCommand>("me")
        .WithDescription("Get current identity and token context");

    config.AddBranch("projects", projects =>
    {
        projects.SetDescription("Manage projects");
        projects.AddCommand<ListProjectsCommand>("list")
            .WithDescription("List all projects");
        projects.AddCommand<CreateProjectCommand>("create")
            .WithDescription("Create a new project");
        projects.AddCommand<UpdateProjectCommand>("update")
            .WithDescription("Update a project name");
        projects.AddCommand<DeleteProjectCommand>("delete")
            .WithDescription("Delete a project");
    });

    config.AddBranch("servers", servers =>
    {
        servers.SetDescription("Manage servers");
        servers.AddCommand<ListServersCommand>("list")
            .WithDescription("List all servers");
        servers.AddCommand<GetServerCommand>("get")
            .WithDescription("Get server details");
        servers.AddCommand<CreateServerCommand>("create")
            .WithDescription("Create a new server");
        servers.AddCommand<DeleteServerCommand>("delete")
            .WithDescription("Delete a server");
        servers.AddCommand<RescaleServerCommand>("rescale")
            .WithDescription("Rescale a server (scale up only)");
        servers.AddCommand<ServerMetricsCommand>("metrics")
            .WithDescription("Get server metrics");
        servers.AddCommand<ListServerVolumesCommand>("volumes")
            .WithDescription("List server volumes");
        servers.AddCommand<CreateServerVolumeCommand>("create-volume")
            .WithDescription("Create a volume on a server");
    });

    config.AddBranch("services", services =>
    {
        services.SetDescription("Manage services");
        services.AddCommand<ListServicesCommand>("list")
            .WithDescription("List services in a project");
        services.AddCommand<GetServiceCommand>("get")
            .WithDescription("Get service details");
        services.AddCommand<CreateServiceCommand>("create")
            .WithDescription("Create a new service");
        services.AddCommand<UpdateServiceCommand>("update")
            .WithDescription("Update a service");
        services.AddCommand<DeleteServiceCommand>("delete")
            .WithDescription("Delete a service");
        services.AddCommand<PauseServiceCommand>("pause")
            .WithDescription("Pause a service");
        services.AddCommand<UnpauseServiceCommand>("unpause")
            .WithDescription("Unpause a service");
        services.AddCommand<DeployServiceCommand>("deploy")
            .WithDescription("Trigger a deployment");
        services.AddCommand<ServiceLogsCommand>("logs")
            .WithDescription("Get service logs");
        services.AddCommand<ServiceMetricsCommand>("metrics")
            .WithDescription("Get service metrics");
        services.AddCommand<ServiceEventsCommand>("events")
            .WithDescription("Get service events");
        services.AddCommand<AddDomainCommand>("add-domain")
            .WithDescription("Add a custom domain");
        services.AddCommand<RemoveDomainCommand>("remove-domain")
            .WithDescription("Remove a custom domain");
    });

    config.AddBranch("credentials", credentials =>
    {
        credentials.SetDescription("Manage registry credentials");
        credentials.AddCommand<ListCredentialsCommand>("list")
            .WithDescription("List all registry credentials");
        credentials.AddCommand<GetCredentialsCommand>("get")
            .WithDescription("Get registry credentials details");
        credentials.AddCommand<CreateCredentialsCommand>("create")
            .WithDescription("Create registry credentials");
        credentials.AddCommand<UpdateCredentialsCommand>("update")
            .WithDescription("Update registry credentials name");
        credentials.AddCommand<DeleteCredentialsCommand>("delete")
            .WithDescription("Delete registry credentials");
    });

    config.AddBranch("oauth", oauth =>
    {
        oauth.SetDescription("Manage OAuth clients");
        oauth.AddCommand<ListOAuthClientsCommand>("list")
            .WithDescription("List OAuth clients");
        oauth.AddCommand<GetOAuthClientCommand>("get")
            .WithDescription("Get OAuth client details");
        oauth.AddCommand<UpdateOAuthClientCommand>("update")
            .WithDescription("Update OAuth client metadata");
        oauth.AddCommand<ListOAuthClientUsersCommand>("users")
            .WithDescription("List OAuth client authorized users");
    });
});

return app.Run(args);
