# AGENTS.md

Notes for whoever extends this next.

`Sliplane.Console` is a .NET 10 global tool (`sliplane`) over the Sliplane v0 REST API, built to
be driven by an LLM agent. It follows the house conventions in `../ConsoleGuidelines.md`:
Spectre.Console.Cli, one command class per file under `src/Commands/`, YAML-first output, no
solution file, published to NuGet from a GitHub Release.

For the manual the *agent* reads, run `sliplane agent-readme` — that text lives in
`src/Commands/AgentReadmeCommand.cs` and is the tool's actual interface for its main audience.
This file is for the human editing the source.

## Commands

```bash
dotnet build src/Sliplane.Console.csproj -c Release

# run without installing
dotnet src/bin/Release/net10.0/Sliplane.Console.dll <args>

# install from source, or reinstall after a change
dotnet tool uninstall --global Sliplane.Console
dotnet pack src/Sliplane.Console.csproj -c Release -p:Version=0.2.0 --output ./nupkgs
dotnet tool install --global --add-source ./nupkgs Sliplane.Console --version 0.2.0
```

Two things about that cycle:

- **`dotnet build` does not update the installed `sliplane`.** The global tool runs from an
  installed package, not from `src/bin`. Editing source and rebuilding changes nothing about the
  command on PATH — repack and reinstall, then verify against the installed command.
- **Bump the version every time.** Reinstalling the same version number can be served from the
  NuGet cache, silently giving you the old package back.

Use a throwaway config directory when testing so you never touch real credentials:

```powershell
$env:SLIPLANE_CONFIG_DIR = "$env:TEMP\sliplane-test"
```

| Variable | Effect |
| --- | --- |
| `SLIPLANE_CONFIG_DIR` | Overrides the config/secrets location |
| `SLIPLANE_SECRET_STORE` | Forces a backend: `dpapi`, `keychain`, `libsecret`, `plaintext` |
| `SLIPLANE_ALLOW_PLAINTEXT_STORE=1` | Permits the plaintext fallback where no keystore exists |

There is deliberately **no** `SLIPLANE_API_KEY`. See the invariants below.

## Layout

```
src/
  Program.cs                 command registration, exception handler
  Infrastructure/
    ApiSettings.cs           LocalSettings (--json) and ApiSettings (-a|--account) + CreateClient
    SliplaneClient.cs        HTTP, HTTP status -> ErrorCode translation, response hints
    SliplaneException.cs     ErrorCode enum; exit codes and the `code:` field are the same thing
    ErrorOutput.cs           the error envelope on stderr, and exception -> ErrorCode mapping
    Output.cs / YamlOutput.cs  YAML by default, JSON with --json
    Json.cs                  JsonElement -> plain objects, for nesting a response in a payload
    PathArg.cs               refuses Windows paths that Git Bash rewrote out of "/"
  Auth/
    AccountResolver.cs       --account -> ResolvedAccount (name, config, key), or a no_account error
    SliplaneIdentity.cs      reads the label and organization out of `me`
  Storage/
    ISecretStore.cs          Get/Set/Delete + SecretKeys
    SecretStoreFactory.cs    platform selection; refuses to fall back silently
    *SecretStore.cs          DPAPI (Windows), Keychain (macOS), libsecret (Linux), plaintext
    ConfigStore.cs           config.yaml, paths, atomic writes, 0600 on Unix
    FileLock.cs              cross-process lock around config + secret writes
  Commands/                  one file per command, mirroring the CLI tree
    Accounts/                add, list, test, remove
    AgentReadmeCommand.cs    the operating manual an agent reads first
```

## How a command is wired

A command derives from `AsyncCommand<TSettings>` with a nested `Settings`, and its settings
derive from one of two bases in `Infrastructure/ApiSettings.cs`:

```csharp
LocalSettings   // --json only. For commands that never call the API: accounts *, agent-readme.
ApiSettings     // adds -a|--account and CreateClient(). Everything that touches the API.
```

`settings.CreateClient()` resolves the account, pulls its key out of the keystore and returns a
`SliplaneClient`; it also records the `--json` choice for `Output`. A `LocalSettings` command must
call `settings.ApplyOutputMode()` itself before writing anything, since nothing else will.

Write results with `Output.Write(doc)` (an API response) or `Output.WriteObject(obj)` (a payload
this CLI builds). Throw `SliplaneException` to fail — `Program.cs` installs the handler that
writes the envelope to stderr and returns the matching exit code. Never call `Environment.Exit`,
and never catch an exception just to print it.

### Adding a command

1. Create `src/Commands/<Area>/<Verb>Command.cs` deriving from `AsyncCommand<Settings>`, with a
   nested `public sealed class Settings : ApiSettings`.
2. Options get `[CommandOption("--name <VALUE>")]` + `[Description]`; arguments get
   `[CommandArgument(0, "<NAME>")]`. Validate in `Settings.Validate()` returning
   `ValidationResult.Error`.
3. Register it in `Program.cs` under the right branch with a `.WithDescription`.
4. Add the row to the command table in `README.md` **and** to `AgentReadmeCommand.Readme`. A
   command an agent cannot discover in `agent-readme` effectively does not exist.

## Invariants — do not casually revert these

**`--account` is required everywhere, and there is no `SLIPLANE_API_KEY`.** No stored default, no
environment variable, no fallback when only one account is configured. This machine holds keys for
several Sliplane accounts; a convenience default is exactly how an agent working from a summarized
transcript deletes a service in the wrong organization — the call succeeds, and nothing in the
output says it went somewhere you did not mean. `AccountResolver.Resolve` is the only way a key
enters the process.

**Secrets never touch `config.yaml`.** Config holds the account name, the organization, a label and
a date. The API key goes through `ISecretStore` into the OS keystore. When no keystore is available
the tool refuses to start rather than silently writing a file — `SLIPLANE_ALLOW_PLAINTEXT_STORE=1`
is the explicit opt-out, and that backend warns on every read.

**Exit codes are the `code:` field.** `ErrorCode` is both, so an agent can branch on either. They
are documented in `agent-readme` and in the README; changing a number breaks callers silently.

**`--env` replaces the whole environment, and the CLI refuses to do it by accident.** The API takes
the array it is given. `services update` names what would be lost and requires `--replace-env`;
`services set-env` is the single-variable path. Secret values read back empty, so no merge can
preserve them.

**`PathArg.Check` stays on every option that takes a URL path.** Git Bash rewrites `/` into the Git
installation directory before this program starts, and the resulting service deploys happily and
fails every healthcheck with nothing in the output to explain why.

## Traps in this codebase

**The root namespace is `Sliplane.Console`, which shadows `System.Console`.** Write
`System.Console.WriteLine` explicitly.

**Spectre discovers options across the whole inheritance chain.** Declaring the same
`[CommandOption]` on a base and a derived settings class — with `new` *or* `override` — makes the
app refuse to start with "Option is duplicated". `--json` lives on `LocalSettings` and nowhere
else; `-a|--account` lives on `ApiSettings` and nowhere else.

**`Output.UseJson` is global mutable state.** `Program.cs` seeds it from the raw `args` before
Spectre parses anything, because the error envelope can be rendered before any command has run.
Commands then set it again from their own settings. If you add another output switch, seed it the
same way or errors will come out in the wrong format.

**`FileLock` is not reentrant.** `accounts add` and `accounts remove` hold `ConfigStore.LockPath`
around the secret write and the config write together. Keep every network call outside the lock —
`accounts add` verifies the key against `me` *before* taking it.

**`SecretStoreFactory.Create()` caches the backend for the process.** Changing
`SLIPLANE_SECRET_STORE` mid-process has no effect; a test that wants another backend needs another
process.

**Prompts and warnings go to stderr.** Interactive commands build their own console:
`AnsiConsole.Create(new AnsiConsoleSettings { Out = new AnsiConsoleOutput(System.Console.Error) })`.
Never use the ambient `AnsiConsole` for prompts — it writes to stdout and corrupts the payload.
`accounts add` and `accounts remove` also refuse to prompt when stdin is redirected, so a scripted
call fails with a remediation instead of hanging.

**`accounts remove` does not revoke anything.** It deletes the local copy of the key. The key keeps
working anywhere else it was pasted until it is revoked in the Sliplane dashboard, and the command
says so in its output. Do not reword that into something that sounds like revocation.

## Sliplane API notes

Base URL `https://ctrl.sliplane.io/v0/`, `Authorization: Bearer <key>`. Legacy tokens additionally
need `X-Organization-ID`, which is what `accounts add --org-id` stores; scoped keys do not.

`me` answers with `authType`, `organizationId`, `tokenType` and a nested `user` object
(`email`, `name`, `githubLogin`, …). `SliplaneIdentity` reads the label from `user.email` and the
organization from `organizationId`, probing alternatives rather than binding to that one shape —
an unrecognised response degrades to an empty label, not an error.

`SliplaneClient.Hint` appends remediation text to two responses that are otherwise hard to act on:
the `409` when a service is moved between a registry image and a repository build, and the `400`
when an update omits the deployment object.

## Testing

There is no test project. The logic worth asserting on today (`PathArg`, `SliplaneIdentity`,
the env-merge guard in `services update`) would fit one; API plumbing would not. If you add one,
keep it out of anything that needs credentials — CI has none.

## Releasing

CI builds every push to `main`. Publishing to NuGet happens only on a published GitHub Release:
the workflow strips the `v` from the tag, packs with that version, and pushes using the
`NUGET_API_KEY` repository secret.

```bash
gh release create v0.2.0 --title v0.2.0 --notes "..."
```

Accounts replaced `SLIPLANE_API_KEY`, which is a breaking change — the next release should be a
minor bump, not a patch, and the notes should tell existing users to run `sliplane accounts add`.
