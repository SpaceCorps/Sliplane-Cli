namespace Sliplane.Console.Storage;

/// <summary>
/// Cross-process exclusive lock, held while config and secrets are written together.
///
/// Two invocations adding or removing accounts at the same moment would otherwise each write a
/// whole config file from their own stale read, and the loser's account silently disappears.
/// </summary>
public sealed class FileLock : IDisposable
{
    private readonly FileStream _stream;

    private FileLock(FileStream stream) => _stream = stream;

    public static async Task<FileLock> AcquireAsync(string path, CancellationToken ct, int timeoutMs = 15000)
    {
        ConfigStore.EnsureDir();

        var deadline = Environment.TickCount64 + timeoutMs;
        var delay = 25;

        while (true)
        {
            ct.ThrowIfCancellationRequested();
            try
            {
                var stream = new FileStream(path, FileMode.OpenOrCreate, FileAccess.ReadWrite, FileShare.None,
                    bufferSize: 1, FileOptions.DeleteOnClose);
                return new FileLock(stream);
            }
            catch (IOException)
            {
                if (Environment.TickCount64 > deadline)
                    throw new IOException($"Timed out waiting for the lock at {path}. Another sliplane process may be stuck.");

                await Task.Delay(delay, ct);
                delay = Math.Min(delay * 2, 400);
            }
        }
    }

    public void Dispose() => _stream.Dispose();
}
