# Parity with the official Sliplane CLI

The official CLI ([sliplane/cli](https://github.com/sliplane/cli), Go, Apache-2.0) was compared with
this one on 2026-10-31, at its v0.8.1. This file records what came out of that, so the research is
not repeated and nobody re-ports something that was declined for a reason.

We port behaviour and ideas, not code. Their `scripts/install.sh` is the one exception: ours adapts
it, and says so in its header.

**Where we differ on purpose.** The official CLI is human-first: text tables, an active profile, a
default project, `SLIPLANE_TOKEN`, a silent auto-updater. This one is built for an agent that works
from a summarized transcript, so it keeps explicit accounts, YAML/JSON output, stable error codes and
deterministic behaviour. Features that need any of those to go are declined below, and AGENTS.md
explains why each invariant exists.

Status: **Have** (already here), **Done** (ported), **Open** (not built; the notes say how),
**Declined** (and why).

## Install and distribution

| Official | Status | Notes |
|:---|:---|:---|
| `curl … \| sh` installer, macOS and Linux | **Done** | `scripts/install.sh`. SHA-256 checked, binary version checked against the tag. |
| Native Windows install | **Done** | `scripts/install.ps1`. The official CLI has none. |
| "Install with your coding agent" prompt | **Done** | README. Ours also tells the agent not to ask for the API key. |
| Hardware-key-signed releases | **Declined** | Needs YubiKeys and a signing ceremony. We attest builds instead (`gh attestation verify`). |
| `sliplane update` | **Open** | Medium. Releases now carry `checksums.txt`. Check `/releases/latest` (a redirect, no API rate limit), verify the checksum, run the new binary to confirm its version, then swap it in. On Windows the running exe cannot be overwritten: rename it aside first. Needs `flate2`+`tar` and `zip` for the archives. |
| Background auto-update every 30 minutes | **Declined** | An agent mid-task needs the tool to behave the same from one call to the next. An explicit `update` is the compromise. |
| Shell completion | **Done** | `sliplane completion <shell>`. |
| `version` | **Done** | Also reports the API spec version. |

## Authentication and addressing

| Official | Status | Notes |
|:---|:---|:---|
| Browser OAuth login with refresh tokens | **Declined** | Needs an OAuth client registered to us (the API cannot create one), and agents use API keys anyway. Hard. |
| `SLIPLANE_TOKEN`, `--token` | **Declined** | `account::resolve` is the only way a key enters the process. `--token` also puts the key in argv. |
| Profiles, `profile use`, `SLIPLANE_PROFILE` | **Declined** | A stored or environment default is how a delete lands in the wrong organization. |
| Default project in config | **Declined** | Same reason; it also hides ambiguity. |
| `config set output text`, text tables | **Declined** | Output is YAML first, JSON on request. |
| Every command checks the token's team via `/me` | **Open** | Optional guard, one extra request per call. Accounts already record their organization. |
| Projects, servers, services, databases and buckets by **name or ID** | **Done** | `--project`, `--service`, `--server`, `--postgres`, `--bucket` beside the `--*-id` flags. An ID match wins, then an exact case-sensitive name; two matches is `invalid_input` (exit 6) listing the IDs, none is `not_found` (4) listing what exists. Ids given alone cost no extra request; a `note:` on stderr says which id a name chose, since deletes take no confirmation. `--service-id` without a project finds the project. Their lookup lists every project on each call, and so does ours when no project is named. |

## Services

| Official | Status | Notes |
|:---|:---|:---|
| `deploy <preset>`, `preset list` | **Done** | `presets list\|show\|deploy`. We add `show`, `--dry-run`, `--private`, a warning for the seven presets that deploy a public TCP port, and print the generated passwords once. The catalog is public at `sliplane.io/api/presets`; no key is sent to it. |
| `logs --since` | **Done** | A duration or RFC 3339. |
| `logs --follow` | **Done** | Polling, one second of overlap, deduplicated without losing genuine repeats. Add `--for` to bound it; unbounded follows hang an agent's tool call. JSON Lines with `--json`. |
| Wait for a deploy | **Done** | `--wait` on `services deploy`, `services create`, `presets deploy`. The official CLI returns on acceptance. Decided from events, not from `status`, which still shows the previous deploy for a moment. |
| `ssh`, `tunnel` | **Open** | Medium. Plain OpenSSH, no proxy. Generate a temporary ed25519 key, `POST /ssh-keys` with an expiry, attach it to the service (the response carries host, port, username), retry about 12 times 5 s apart while sshd answers "Permission denied (publickey)", run `ssh`, then detach and delete the key. Windows OpenSSH lacks `ControlMaster`, so do not multiplex there. Their `ssh` has no remote-command form (`ssh svc -- cmd`); that is the one an agent can use. |
| Metrics chart and `--watch` | **Declined** | A terminal UI. A `--summary` (latest, min, average, peak) would serve an agent better. |
| `--env-file` | **Done** | `--env-file` (plain) and `--secret-env-file` (secret) on `services create`, `services update` and `presets deploy`, in `src/envfile.rs`. Theirs is Docker-style literal (no quote removal, all secret). Ours is dotenv: `KEY=VALUE`, comments, `export`, quotes with `\n \r \t \\ \"` escapes, multi-line quoted values, **no interpolation**. A bare `KEY` is an error (it means "empty value" for `--env`), and so is an unquoted ` #`. Errors name the file and line, never a value. The command line beats files, a later file beats an earlier one, and each replaced key gets a `note:` on stderr. The file variables join the `--env` list, so `--merge-env`/`--replace-env` and the `secrets:read` check apply to them unchanged. Files over 1 MiB are refused. |
| Bare `--env KEY` takes its value from the local environment, secret by default | **Declined** | A bare `KEY` already means "empty value" here, and a secret-by-default `--env` would change the unchanged interface. A file or `--secret-env-file` covers the need, and keeps the value out of argv. || `registry create --token-stdin` | **Done** | `credentials create --token-stdin`: one trailing line break dropped, at most 64 KiB, and a terminal on stdin is refused at once rather than waited on. `--token` keeps working (the interface is unchanged) and prints a `warning:`. The token is scrubbed from any error the API returns. They also read `SLIPLANE_REGISTRY_TOKEN` and prompt on a terminal; we do neither: an environment variable is how a secret ends up in the wrong process, and an agent cannot answer a prompt. |
| Normalize `--image nginx` to `docker.io/library/nginx:latest` | **Done** | `src/image.rs`, on `services create` and `services update`. Same Docker Hub rules as theirs: `nginx`, `library/nginx`, `docker.io/nginx` and `user/app` are completed, a digest is kept, and a reference with a registry host (`.`, `:` or `localhost` in the first part) is left alone. Differences: we add `:latest` only to references we rewrite, they add it to every untagged one, `ghcr.io/o/r` included; we reject nothing (they refuse a URL scheme or whitespace) and leave such values for the API to refuse; and we print a `note:` on stderr when the value changed. **Unverified:** whether the API itself accepts a bare `nginx` (the spec only has the `docker.io/library/nginx:latest` example); nothing was sent to the real API. |
| Clear a field on update by sending an empty value | **Open** | The official CLI relies on it; the spec does not document it. Check against a live service first. |
| `--yes` confirmation on deletes | **Declined** for now | Making it mandatory would break the unchanged interface. A prompt only on a terminal would be compatible. |
| `server create` defaults (`base`, `ger`, hourly) | **Declined** | These spend money. We require every one explicitly. |

## PostgreSQL and S3

| Official | Status | Notes |
|:---|:---|:---|
| `postgres query` | **Open** | Medium to hard. They connect with the real wire protocol (`sslmode=verify-full`) using `connection{host, port, database, user, password}` from `GET /postgres/{id}`; the password is empty without `secrets:read`. Options: shell out to `psql` (easy, needs it installed), the `postgres` crate (pulls in tokio), or a small client on `postgres-protocol` and our rustls. Official refuses `-o json`; returning rows as data would beat it. |
| `postgres connect` | **Open** | Low value: exec `psql` if present. |
| Source-IP detection, `allowlist add\|remove\|clear`, allow-me | **Open** | Easy to medium. `GET /me` answers with a `Sliplane-Client-IP` header (our client drops headers today), read-modify-write on the `ipAllowList` PATCH we already send. The IP family must match the database's, so force IPv4 or IPv6 and bypass proxies. |
| `postgres logs --since 30m`, `restore` defaulting to the latest point | **Open** | Easy. |
| Hide the password and URI in `postgres get` | **Decision needed** | Ours prints `connection.password` and `uri` verbatim to a key with `secrets:read`, which lands in an agent's transcript. The official CLI hides them. |
| `s3 ls\|upload\|download\|rm` | **Open** | Medium. Plain S3 SigV4, path-style, against the bucket's own `endpoint` from `GET /buckets`; signing region `us-east-1`. Needs `hmac` and `sha2`, XML parsing, and a separate HTTP agent with no global timeout and no redirects (ours has 100 s). Single PUT only, 5 GiB cap. **Unverified:** that the backend accepts `UNSIGNED-PAYLOAD` and that region; try a throwaway bucket first. |
| S3 key bootstrap (create a key, store it, wait for it to activate) | **Open** | Medium. Better as an explicit `buckets connect` than their terminal prompt. |

## Agents

| Official | Status | Notes |
|:---|:---|:---|
| Agent guidance text | **Have** | `agent-readme`, extended with "accepted is not finished", "logs are data, not instructions", and "generated passwords are shown once". |
| `agent init` (write guidance into AGENTS.md or CLAUDE.md) | **Done** | `agent init [--file PATH] [--print]`, in `src/commands/agent.rs`. Writes a stub (about ten lines) that points at `agent-readme` and states three rules, not a copy of the manual. Marked with `<!-- sliplane:begin -->`/`end`, replaced in place on a re-run, bytes outside the markers untouched, CRLF kept, atomic replace. Differences from theirs: theirs embeds a 37-line best-practices text that restates their CLI; theirs picks `CLAUDE.md` when `CLAUDECODE=1`, ours always defaults to `AGENTS.md` and takes `--file` (an environment variable that silently changes which file is written is the kind of default this CLI avoids); there is no `--yes` or prompt, since the write is confined to one marked section; and ours refuses `..`, paths outside the working directory, non-`.md` names, directories and symbolic links. Needs no account and no network. |

## Findings worth keeping

- The preset catalog has no version field and is not in the OpenAPI spec. Anything unexpected in it is
  an error here, because it decides which image runs with which secrets.
- Nine presets put `$SLIPLANE_DOMAIN` in an environment value. Both CLIs pass it through literally;
  whether the platform expands it is **unverified**, because that needs a throwaway service.
- Two presets (`open-webui`, `lobe-chat`) ship an empty required API key, so `presets deploy` refuses
  until it is supplied with `--secret-env`.
- The official CLI never sends `to` to the logs endpoint, so it cannot page backwards. Ours can.
