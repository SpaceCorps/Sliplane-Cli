using Sliplane.Console.Infrastructure;
using Spectre.Console.Cli;

namespace Sliplane.Console.Commands.Buckets;

public sealed class ListBucketsCommand : AsyncCommand<ApiSettings>
{
    public override async Task<int> ExecuteAsync(CommandContext context, ApiSettings settings)
    {
        var client = settings.CreateClient();
        var result = await client.GetAsync("buckets");
        Output.Write(result);
        return 0;
    }
}
