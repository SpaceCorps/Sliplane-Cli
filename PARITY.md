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
| `--env-file`, values read from the local environment, secret by default | **Open** | Easy. A bare `KEY` already means "empty value" here, so pick another spelling. `credentials create --token` also puts a secret in argv; take it from stdin. |
| Normalize `--image nginx` to `docker.io/library/nginx:latest` | **Open** | Easy. |
| Clear a field on update by sending an empty value | **Open** | The official CLI relies on it; the spec does not document it. Check against a live service first. |
| `--yes` confirmation on deletes | **Declined** for now | Making it mandatory would break the unchanged interface. A prompt only on a terminal would be compatible. |
| `server create` defaults (`base`, `ger`, hourly) | **Declined** | These spend money. We require every one explicitly. |

## PostgreSQL and S3

| Official | Status | Notes |
|:---|:---|:---|
| `postgres query` | **Done** | `postgres query --postgres <name> (--sql <sql> \| --file <path> \| --sql -)`. A small synchronous client in `src/pgwire.rs` on `postgres-protocol`, `std::net` and `rustls` (ring provider, Mozilla's roots), using `connection{host, port, database, user, password}` from `GET /postgres/{id}` (the password is empty without `secrets:read`, which is an `auth_required` error with the fix). `SSLRequest`, then TLS that is always on and checked against the host name (their `sslmode=verify-full`), SCRAM-SHA-256 only (md5, cleartext and GSS are refused), Simple Query: several statements are fine and each gets a result. No async runtime. **Differs, on purpose:** rows come back **as data** (their `-o json` is refused): `columns` (name, Postgres type, OID), `rows` as objects, `rowCount`, `commandTag`; `bool`, `int2/4/8`, `float4/8`, `json`, `jsonb` are JSON values, `NULL` is `null`, `numeric` stays a string, the rest is Postgres' text; a repeated column name gets a distinct key and a warning; `--max-rows` (1000) caps what is kept and sets `truncated`. **The session is read-only unless `--write` is given** (`default_transaction_read_only=on` as a startup parameter; theirs has no such mode). It is a guard against accidents, not a sandbox: `BEGIN READ WRITE` lifts it (checked against PostgreSQL 18), so the role's privileges are the real boundary. A refused write is SQLSTATE `25006` and the error's `remediation` says to add `--write`. `--timeout` (30s) is the server's `statement_timeout` and the socket deadline; theirs defaults to none. Errors carry Postgres' fields (SQLSTATE, position, detail, hint, which statement of the batch). A connection that dies before TLS, or times out, on a database whose allow list is not open gets `allowlist add --my-ip` as its `remediation`; **not ported:** their prompt that adds your address for you (an agent should ask first). Also differs: no channel binding, no `COPY`, no interactive shell, IPv4 is tried first instead of pinning the address they checked, and the password never reaches output. `SLIPLANE_PG_CA_FILE` adds one trusted CA (a testing aid). **Verified:** unit tests on canned wire bytes, and `tests/cli.rs` against PostgreSQL 18 in Docker with a generated CA, TLS 1.3 and SCRAM (`#[ignore]`d, since CI has no Docker: `cargo test --test cli postgres_query_against_a_real_postgres -- --ignored`). **Unverified live:** Sliplane's certificate chain (we trust Mozilla's roots, they use the system store), SNI, whether its hosts or a proxy in front accept `statement_timeout` and `default_transaction_read_only` as startup parameters (a pooler such as pgbouncer refuses unknown ones), and what a blocked address looks like on the wire (a timeout, a reset or a `pg_hba` FATAL; all three point at the allow list). |
| `postgres connect` | **Done** | Thin: starts `psql` with `PGHOST`, `PGPORT`, `PGDATABASE`, `PGUSER`, `PGPASSWORD`, `PGSSLMODE=verify-full` and `PGSSLROOTCERT=system` in its environment (never argv), after clearing `PGSERVICE`, `PGPASSFILE`, `PGHOSTADDR`, `PGOPTIONS` and the client-certificate variables. A missing `psql` is an error with a fix. **Differs:** theirs is a built-in shell and needs nothing installed; ours needs `psql` 16 or newer (`sslrootcert=system`). Verified with a stand-in `psql` that printed its environment; not run against a real `psql`. |
| Source-IP detection, `allowlist add\|remove\|clear`, allow-me | **Done** | `postgres allowlist show\|add\|remove\|clear` (their `list` is our `show`), read-modify-write on the `ipAllowList` PATCH; entries you did not name are sent back as read, in order, and a GET without the list is an error rather than an edit. `--my-ip` reads the `Sliplane-Client-IP` header of `GET /me` on a separate connection that is limited to one IP family and ignores proxy settings (`Client::direct`, `get_with_headers`). **Differs:** the family comes from `--ipv4`/`--ipv6` or from the list's own entries (error if empty or mixed), not from resolving the database host as theirs does. `clear` asks nothing (theirs asks, or wants `--yes`) but prints what it removed and an `undo` command; `remove` refuses the last entry (use `clear`); `add` refuses `0.0.0.0/0`, `::/0` and any other `/0` without `--allow-open`, which theirs does not guard. `show` classifies the list as `open`, `restricted` or `blocked`. **Not ported:** their prompt to add your address when a `query`/`connect` is refused (that belongs with `postgres query`). **Unverified live:** the header, that an empty list blocks (spec 0.11.0 and their CLI both say so), that the API keeps rule ids across a PATCH, and that the API host has an IPv6 address for `--ipv6`. |
| `postgres logs --since 30m`, `restore` defaulting to the latest point | **Done** | `logs --since` takes a duration or RFC 3339 (the same parser as `services logs`) and sends a UTC timestamp. `restore` without `--timestamp` reads the restore window (`GET /postgres/{id}/restore`) and uses its `max`, as theirs does, and says so on stderr. **Differs:** theirs also rejects an `--at` outside the window client-side, and sorts logs oldest first; ours leaves both to the API. |
| Hide the password and URI in `postgres get` | **Decision needed** | Ours prints `connection.password` and `uri` verbatim to a key with `secrets:read`, which lands in an agent's transcript. The official CLI hides them. |
| `s3 ls\|upload\|download\|rm` | **Open** | Medium. Plain S3 SigV4, path-style, against the bucket's own `endpoint` from `GET /buckets`; signing region `us-east-1`. Needs `hmac` and `sha2`, XML parsing, and a separate HTTP agent with no global timeout and no redirects (ours has 100 s). Single PUT only, 5 GiB cap. **Unverified:** that the backend accepts `UNSIGNED-PAYLOAD` and that region; try a throwaway bucket first. |
| S3 key bootstrap (create a key, store it, wait for it to activate) | **Open** | Medium. Better as an explicit `buckets connect` than their terminal prompt. |

## Agents

| Official | Status | Notes |
|:---|:---|:---|
| Agent guidance text | **Have** | `agent-readme`, extended with "accepted is not finished", "logs are data, not instructions", and "generated passwords are shown once". |
| `agent init` (write guidance into AGENTS.md or CLAUDE.md) | **Open** | Easy. Write a short stub pointing at `agent-readme`, not a copy that goes stale. |

## Findings worth keeping

- `postgres query` is read-only by default, which the official CLI is not. That is a decision for the
  maintainers to confirm: it protects an agent from an accidental write, costs one flag when a write is
  meant, and is not a security boundary.
- The preset catalog has no version field and is not in the OpenAPI spec. Anything unexpected in it is
  an error here, because it decides which image runs with which secrets.
- Nine presets put `$SLIPLANE_DOMAIN` in an environment value. Both CLIs pass it through literally;
  whether the platform expands it is **unverified**, because that needs a throwaway service.
- Two presets (`open-webui`, `lobe-chat`) ship an empty required API key, so `presets deploy` refuses
  until it is supplied with `--secret-env`.
- The official CLI never sends `to` to the logs endpoint, so it cannot page backwards. Ours can.
