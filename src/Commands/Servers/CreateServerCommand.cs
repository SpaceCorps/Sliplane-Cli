using System.ComponentModel;
using Sliplane.Console.Infrastructure;
using Spectre.Console.Cli;

namespace Sliplane.Console.Commands.Servers;

public sealed class CreateServerCommand : AsyncCommand<CreateServerCommand.Settings>
{
    public sealed class Settings : ApiSettings
    {
        [CommandOption("--name <NAME>")]
        [Description("The name of the server")]
        public required string Name { get; init; }

        [CommandOption("--instance-type <TYPE>")]
        [Description("Instance type: starter, base, medium, large, x-large, xx-large, dedicated-base, dedicated-medium, dedicated-large, dedicated-x-large, dedicated-xx-large, dedicated-xxx-large")]
        public required string InstanceType { get; init; }

        [CommandOption("--location <LOCATION>")]
        [Description("Location: ger, fin, us-east, us-west, sin (legacy: fsn, nbg, hel, ash, hil)")]
        public required string Location { get; init; }

        [CommandOption("--disk-size-gb <SIZE>")]
        [Description("Larger data disk than bundled with the instance type: 50, 100, 250, 500, 1000")]
        public int? DiskSizeGb { get; init; }

        [CommandOption("--billing-cycle <CYCLE>")]
        [Description("Billing cycle: hourly (default), monthly, yearly")]
        public string? BillingCycle { get; init; }
    }

    public override async Task<int> ExecuteAsync(CommandContext context, Settings settings)
    {
        var client = settings.CreateClient();
        var body = new Dictionary<string, object>
        {
            ["name"] = settings.Name,
            ["instanceType"] = settings.InstanceType,
            ["location"] = settings.Location
        };
        if (settings.DiskSizeGb.HasValue) body["diskSizeGb"] = settings.DiskSizeGb.Value;
        if (!string.IsNullOrEmpty(settings.BillingCycle)) body["billingCycle"] = settings.BillingCycle;
        var result = await client.PostAsync("servers", body);
        Output.Write(result);
        return 0;
    }
}
