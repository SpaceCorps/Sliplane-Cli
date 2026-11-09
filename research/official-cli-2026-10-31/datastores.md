# Official Sliplane CLI (Go) - data stores feature inventory (Postgres + S3)

Scope: `internal/cli/{postgres*.go,s3*.go}`, `internal/api/{postgres_insights,postgres_management,bucket_management,storage,access}.go`,
`internal/objectstore/client.go`, plus the pieces they lean on (`internal/api/client.go`, `models.go`, `cli/resolve.go`, `cli/prompt.go`).
Compared against our Rust CLI (`src/cli.rs`, `src/commands/mod.rs`, pinned spec `spec/sliplane-openapi-0.11.0.json`).

Method and caveats: static read of the clone plus a grep of our pinned spec. Nothing was run against the live API or a live
database/bucket, so every "the server accepts X" statement is inferred from their code and tests, not observed. Official repo is
Apache-2.0 (NOTICE: Sliplane GmbH) - porting ideas is fine; copying code would need the notice kept. `access.go` turned out to be
registry credentials (not Postgres access); Postgres access logic is in `postgres_access.go`.

---

## 0. Cross-cutting differences that affect every command

| Aspect | Official | Ours |
| --- | --- | --- |
| Group names | `postgres`, `s3` | `postgres`, `buckets` (no `s3` group) |
| Resource addressing | positional `<name-or-id>`; every command first does `GET /postgres` (or `GET /buckets`) and resolves an exact ID, else a unique exact name (ambiguous name -> error listing IDs) | `--postgres-id` / `--bucket-id` flags, IDs only; no extra list call |
| Identity | `--profile`, `--token`, `SLIPLANE_TOKEN`, OS keychain, active-profile default | `--account` required everywhere, no env key, no default (deliberate invariant) |
| Output | `-o text` (tables) or `-o json`; text is the default | YAML default, `--json` |
| Destructive confirm | `--yes/-y` required when not on a TTY for: postgres delete, allowlist clear, s3 rm, s3 delete, s3 key delete | none (delete runs immediately) |
| Read-only tokens | `requireS3Write()` refuses S3 writes/rm/key-create when `GET /me` says `tokenType != rw`, even though the S3 key itself could write | n/a today (no S3 data plane) |
| Terminal safety | `terminalText()` replaces control chars in remote strings before printing | not needed for structured output |

Name-or-id resolution is the one broadly useful ergonomic idea (agents tend to know names, not `pg_...` IDs). EASY, but cross-cutting, so it
belongs in a separate decision, not in this port.

---

## 1. Postgres command inventory

Legend for the last column: HAVE / PARTIAL / MISSING (against our CLI). Endpoints are all in our pinned spec 0.11.0 unless stated.

| Official command | Args / flags | API calls | Behavior beyond "one request and print" | Ours |
| --- | --- | --- | --- | --- |
| `postgres list` (alias `ls`) | `-a/--all` extra columns | `GET /postgres` | sorted by name then id; table; `--all` adds id, user, host, port, allowed IPs, created-by | HAVE (`postgres list`, raw) |
| `postgres show <name-or-id>` | - | `GET /postgres`, `GET /postgres/{id}` | **password and uri are deliberately excluded from output** (Go field tagged `json:"-"`; no uri field) | HAVE (`postgres get`) - but see section 8: we print `connection.password`/`uri` verbatim |
| `postgres create <name>` | `--region` (def `ger`), `--instance-type` (def `base`), `--billing-cycle` (def `hourly`, validated hourly/monthly/yearly), `--disk-size` int, `--version` int, `--database-name`, `--database-user` | `POST /postgres` | client-side non-empty/positive checks; prints created db; help says provisioning continues async | HAVE, and we are ahead: we also take `--ip-allow/--block-public-ips` on create; we require region/instance-type (no defaults, so no accidental spend) |
| `postgres update <name-or-id>` | `--name`, `--instance-type`, `--disk-size` (at least one) | `GET /postgres`, `PATCH /postgres/{id}` | client-side check that new disk size > current; sends only changed fields | HAVE (superset: plus `--ip-allow/--block-public-ips`) |
| `postgres delete <name-or-id>` | `-y/--yes` | `GET /postgres`, `DELETE /postgres/{id}` | confirmation prompt on TTY, `--yes` otherwise; prints `{id,name,deleted:true}` | HAVE (no confirmation) |
| `postgres restart\|pause\|unpause <name-or-id>` | - | `POST /postgres/{id}/{restart,pause,unpause}` (202, returns Postgres object) | prints refreshed db | HAVE (`done()` prints the returned body when non-empty) |
| `postgres rotate-password <name-or-id>` | - | `POST /postgres/{id}/credentials/rotate` (200, returns Postgres with new connection) | prints host/port/db/user/**password**; errors if API returned no password (token lacks `secrets:read`) | HAVE as `rotate-credentials` |
| `postgres logs <name-or-id>` (alias `log`) | `--since` (**RFC3339 or lookback duration like `30m`**), `--until` RFC3339, `--limit` (def 100, 1-1000) | `GET /postgres/{id}/logs?limit&since&until` | `--since` > `--until` rejected client-side; logs sorted oldest-first; text mode prints `time  severity  message` one per line, JSON mode prints the array | PARTIAL: we take RFC3339 `--since` only; default limit is the API's 1000; raw YAML. Adding duration parsing is EASY (pure client-side). |
| `postgres active-connections <name-or-id>` (alias `active-connection`) | - | `GET /postgres/{id}/active-connections` | table of pid/user/db/client/state/wait/query | HAVE as `connections` (alias `active-connections` is trivial to add) |
| `postgres relation-sizes <name-or-id>` (alias `relation-size`) | - | `GET /postgres/{id}/relation-sizes` | sorted by totalBytes desc; humanised sizes | HAVE (unsorted raw bytes - fine for agents) |
| `postgres slow-queries` / `top-queries <name-or-id>` | - | `GET /postgres/{id}/slow-queries`, `/top-queries` | sorted by rank | HAVE |
| `postgres restore-window <name-or-id>` | - | `GET /postgres/{id}/restore` | prints min/max | HAVE |
| `postgres restore <name-or-id>` | `--at` RFC3339, **optional - defaults to the latest recoverable point** | `GET /postgres/{id}/restore`, `POST /postgres/{id}/restore {timestamp}` | fetches window first, rejects `--at` outside [min,max] client-side, then restores | PARTIAL: ours requires `--timestamp` and does no window check. EASY: one extra GET, default to `max`. |
| `postgres connect <name-or-id>` | - | see section 2 | interactive SQL shell (embedded usql) | MISSING |
| `postgres query <name-or-id> [sql]` | `-f/--file PATH` (`-` = stdin), `--timeout DURATION` (Go duration, 0 = off) | see section 2 | runs SQL from arg, file or piped stdin | MISSING |
| `postgres allowlist list <name-or-id>` | - | `GET /postgres` + `GET /postgres/{id}` | table source/description/id | PARTIAL (visible inside `postgres get`) |
| `postgres allowlist add <name-or-id> <ip-or-cidr>` | `--description` | `PATCH /postgres/{id} {ipAllowList:[...]}` | see section 3 | MISSING (we only have replace-all via `update --ip-allow`) |
| `postgres allowlist remove <name-or-id> <ip-cidr-or-rule-id>` | - | `PATCH /postgres/{id}` | see section 3 | MISSING |
| `postgres allowlist clear <name-or-id>` | `-y/--yes` | `PATCH /postgres/{id} {ipAllowList:[]}` | block all public access | HAVE-ish (`update --block-public-ips`) |

Not in the official CLI at all (so nothing to port): `postgres metrics`. Ours has it (`GET /postgres/{id}/metrics`); the official top-level `metrics`
command only covers services and servers. Also no official command for the restore-window-derived "create from backup" other than `restore`.

---

## 2. `postgres query` / `postgres connect` in detail

### 2.1 Do they use a real wire-protocol driver? Yes.
- Stack: `github.com/lib/pq` (pure-Go Postgres wire protocol driver) wrapped by **usql** (`github.com/xo/usql`), a psql-like engine. `connect` is the usql
  interactive REPL (readline editor via `gohxs/readline`, `\?` help, `\q` exit, `\d*` metacommands); `query` feeds the same engine a non-interactive
  line source. Both go through `runPostgres()` (`postgres_shell.go`). The help text sells it as "without installing psql".
- There is no HTTP "session" concept. The words *session* and *driver* in the file names mean: `postgres_session.go` = building the DSN + the `connect` command;
  `postgres_driver.go` = a shim that re-registers usql's `postgres` driver so that (a) connections are made with an explicit `pq.Config` built from the
  API credentials, (b) `service=''` and `passfile=''` are forced so inherited `PGSERVICE`/`~/.pgpass` can never redirect the connection, (c) the dialer is
  tied to the command context so Ctrl-C/timeout closes sockets, (d) `SetMaxOpenConns(1)` so temp tables, `SET` and transactions persist across statements,
  (e) the DSN shown by `\conninfo` is rewritten to omit the password, (f) server NOTICEs and LISTEN notifications are echoed to stderr/stdout.

### 2.2 Where the credentials come from
- No separate credentials endpoint. Credentials are the `connection` object on **`GET /postgres/{id}`**: `{host, port, database, user, password, uri}`.
  Per the spec it is only present once the database is running, and the password is **empty unless the token has the `secrets:read` scope**
  (spec intro: "Without `secrets:read`, environment values and database passwords are returned empty").
- `validatePostgresConnection`: error if `connection` is missing ("the database must be running"), port outside 1-65535, or host/database/user/password empty or
  containing NUL. They ignore `connection.uri` and build their own URL so special characters in the password/user/db are escaped correctly.
- Resolution flow (`postgresDSN`): resolve name-or-id (`GET /postgres`, `GET /postgres/{id}`) -> validate -> DNS-resolve `connection.host` with the system
  resolver, prefer the first IPv4 address, else the first address -> access check (2.3) -> build URL.
- Connection URL they build:
  `postgres://USER:PASSWORD@HOST:PORT/DB?hostaddr=<resolved ip>&sslmode=verify-full&ssl_min_protocol_version=TLSv1.2&connect_timeout=10&application_name=sliplane-cli&target_session_attrs=any`
  i.e. TLS is mandatory with full certificate and hostname verification against `HOST`, while the TCP connection is **pinned to the exact IP that was allowlist-checked**
  (`hostaddr`) so DNS cannot change between the check and the connect.

### 2.3 The IP allowlist / "source IP detection through the Sliplane API"
Endpoint: **`GET /me`** (nothing new - the endpoint we already use for `me`). The response carries a header **`Sliplane-Client-IP`** ("Public IP address of the caller,
when known. Use it to check database IP allowlists" - this header IS in our pinned spec, under `GET /me` -> 200 -> headers).
Algorithm (`postgres_access.go`):
1. If any allowlist entry is a `/0` prefix of the same address family as the destination IP, skip detection (everything is allowed).
2. Otherwise `client.ClientIP(ctx, "tcp4"|"tcp6")`: build a one-off HTTP client whose dialer is **forced to IPv4 or IPv6 to match the database address family**
   and with **proxies disabled**, call `GET /me`, read `Sliplane-Client-IP`, parse it, check its family equals the destination's family. (The forced family matters: the
   database sees whichever family you connect with, and the API would otherwise report the family your OS happened to prefer for the API host.)
3. `postgresAllowsIP`: exact-address match or CIDR containment, IPv4-mapped IPv6 unmapped first; an empty list allows nothing.
4. If not allowed:
   - not on a TTY (or `-o json`): error `IP x is not allowed for database "n"; run sliplane postgres allowlist add <id> <ip>/32 ...`.
   - on a TTY: ask y/N; on yes, **re-fetch the database** (`GET /postgres/{id}`) so rules added concurrently are not clobbered, re-check, then
     `PATCH /postgres/{id}` with existing rules + `{source: "<ip>/32" (or /128), description: "Sliplane CLI"}`; prints to stderr how to remove it. The rule is persistent.
   - A read-only token fails the PATCH (`403 read_only_token`) and the error is surfaced.
`postgres restore`, `logs`, `restore-window`, `allowlist *` etc. do NOT do this check - only `connect` and `query` touch the database over the wire.

### 2.4 `query` specifics
- Input precedence: SQL argument XOR `--file` (both = error; `--file ""` = error; `--file -` or no arg + piped stdin = read stdin; no arg + TTY stdin = error "provide a SQL argument, --file, or pipe SQL").
  Whitespace-only SQL = error "SQL must not be empty".
- **Refuses `-o json`**: "PostgreSQL commands require --output text" (output is usql's aligned-table text, streamed natively). So the official CLI offers agents no structured SQL output.
- `--timeout` covers the whole command including stdin read and connection; on expiry the error is "PostgreSQL query timed out"; Ctrl-C closes the socket.
- Non-interactive scripts run with `ON_ERROR_STOP=on` (stop at the first failing statement, non-zero exit); interactive sessions keep going.
  usql splits statements itself (handles `$$ ... $$`, comments, missing final semicolon at EOF) and runs them one at a time in autocommit mode, and accepts psql-style meta-commands
  inside scripts (`\dt`, `\conninfo`). Pager forced off.
- Unusual: usql's own parser/metacommands are what make multi-statement scripts "just work"; a plain driver does not give you those for free (see 2.6).

### 2.5 What we would need to port it (blocking, no async runtime)
Prerequisites for any variant (all EASY): fetch `GET /postgres/{id}`, validate `connection` (and turn an empty password into a precise remediation: "this key lacks `secrets:read`;
`sliplane me` lists scopes" - we already have `account::can_read_secrets`), resolve host with `std::net::ToSocketAddrs`, prefer IPv4, run the allowlist check (section 3), then connect.

| Option | What it takes | Verdict | Pros | Cons |
| --- | --- | --- | --- | --- |
| A. Shell out to `psql` | build env for the child: `PGHOST`, `PGHOSTADDR`, `PGPORT`, `PGDATABASE`, `PGUSER`, `PGPASSWORD`, `PGSSLMODE=verify-full`, `PGSSLMINPROTOCOLVERSION=TLSv1.2`, `PGCONNECT_TIMEOUT=10`, `PGAPPNAME=sliplane-cli`; unset `PGSERVICE`/`PGPASSFILE`/`PGSYSCONFDIR`; run `psql -X -v ON_ERROR_STOP=1 -c SQL` or `-f -`; for `connect` just exec with inherited stdio. `std::process::Command`, no new crate. | EASY (~80 lines) | gives `\d*`, pager, readline, real statement splitting for free; works for `connect` | psql must be installed (often not on Windows or in slim Docker images - the official's whole point is avoiding that); output is psql text (use `--csv`/`-A -t` for machine-readable); password only via env (acceptable) |
| B. `postgres` crate (sync API) + a rustls connector | add `postgres` (0.19.x) + a TLS connector crate for rustls (third-party `postgres-rustls`/`tokio-postgres-rustls` style, not verified here). Use `tokio_postgres::Config::hostaddr()` (exists in 0.7.18) + `SslMode::Require` plus verification of the hostname via the connector's server name, `connect_timeout`, `application_name`. Run with `Client::simple_query(sql)` which returns every column as text (`SimpleQueryMessage::Row::get(i) -> Option<&str>`, so NULL is distinguishable) and accepts multi-statement scripts. Convert to `{commands:[{columns,rows,rowsAffected}]}`. | MEDIUM | typed errors with SQLSTATE; structured JSON/YAML result is natural; no external binary | `postgres` internally pulls **tokio (rt, time) + tokio-postgres + futures** and builds its own runtime - conflicts with AGENTS.md "no async runtime"; larger binary/compile time; a multi-statement simple-query message runs as one **implicit transaction** (an error rolls back earlier statements in the same message - different from usql/psql which commit per statement); no meta-commands; no interactive shell |
| C. Hand-rolled client on `postgres-protocol` + `rustls::StreamOwned` | `postgres-protocol` 0.6.x has **no tokio dependency** (deps: bytes, byteorder, fallible-iterator, hmac, sha2, md-5, stringprep, rand, base64, memchr, getrandom) and provides message encode/decode plus the SCRAM-SHA-256 client (`authentication::sasl`). We already have `rustls` + `webpki-roots` + `ring` in the tree (via ureq). Steps: TCP connect to the pinned IP with `connect_timeout`, send SSLRequest, expect `S`, wrap in `rustls` with `ServerName` = host (gives verify-full semantics), send StartupMessage (user, database, application_name=sliplane-cli, client_encoding=UTF8), handle AuthenticationSASL / Ok / cleartext/MD5, loop to ReadyForQuery, send simple `Query`, decode RowDescription / DataRow / CommandComplete / ErrorResponse / NoticeResponse / ReadyForQuery, send Terminate. Per-statement splitting would need our own lexer (dollar quotes, comments) if we want psql-like autocommit-per-statement. | MEDIUM-HARD (~400-500 lines + tests; auth and error-field parsing are the fiddly parts) | zero async, small dependency delta (only `postgres-protocol`, maybe a direct `rustls` dep), full control over output, timeouts via socket timeouts, `CancelRequest` possible | most code to own; PG 18 requires SCRAM correctness (use the crate's implementation, do not write it); TLS trust roots: webpki-roots (Mozilla bundle) vs system roots - Go uses system roots; assumed Sliplane's DB cert chains to a public CA (unverified) |

Recommendation: ship `postgres query` as B or C with structured output (agents want rows, not an aligned table), and add `postgres connect` as a thin "exec `psql` if present,
else print the exact `psql` env/command and point to `query`" (A). A native REPL is HARD and low value for an LLM-driven CLI - skip.

---

## 3. `postgres allowlist` and the access helpers

Semantics worth copying (all client-side, EASY; endpoint is only `PATCH /postgres/{id}` with `{ipAllowList:[{source,description}]}` which we already call from `update --ip-allow`):
- `add <db> <ip-or-cidr> [--description]`: normalise input (`postgresCIDR`): a bare address becomes `/32` or `/128` (IPv4-mapped IPv6 unmapped, zoned addresses rejected),
  a network is masked to its network address (`10.0.0.5/24` -> `10.0.0.0/24`), invalid input -> "invalid IP address or CIDR". Read-modify-write: GET current list, append unless an equal
  normalised entry exists. Adding an existing rule is a no-op unless `--description` is given, in which case it updates the description. PATCH sends the full list using only writable
  fields (`source`, `description`; ids are server-assigned `pgfw_...` and createdAt is read-only). Prints the saved list from the PATCH response.
- `remove <db> <ip-cidr-or-rule-id>`: match by rule ID (`pgfw_...`) or by normalised CIDR; error `allowlist rule "x" not found` if nothing matched; PATCH the remainder.
- `clear <db> [--yes]`: PATCH `[]` = block all public access; confirmation unless `--yes`.
- `list <db>`: prints source, description, id.
- Race: all of these are read-modify-write with no ETag, so concurrent edits can clobber each other (same weakness upstream; they only re-fetch right before the auto-add).
- Matching helper `postgresAllowsIP` (exact or CIDR contains, v4-mapped unmapped, empty list = deny). In Rust: ~30 lines of bit comparison for v4/v6, or the `ipnet` crate (small, no deps of note).

For an LLM-agent CLI the highest-value slice is not the prompt flow but two stateless commands:
1. `postgres my-ip` (or put it in `me` output): `GET /me`, read `Sliplane-Client-IP`. Needs our `client.rs` to expose response headers (today `read()` drops them) - small change.
   Family forcing: ureq 3.4.2 has `Config::ip_family(IpFamily::Ipv4Only|Ipv6Only)` and `Config::proxy(None)` (both verified in the local source), so a one-off Agent replicates their tcp4/tcp6 + no-proxy dialer.
2. `postgres allow-me <db> [--description]` = detect the IP in the database's family, skip if already allowed, otherwise add `<ip>/32` (or `/128`) with description "Sliplane CLI". No TTY prompt
   (agents cannot answer it); the invocation itself is the consent, and the output states the rule that was added and how to remove it.

---

## 4. S3 (`s3 ...`, ours: `buckets ...`) inventory

| Official command | Args / flags | Calls | Behavior beyond one request | Ours |
| --- | --- | --- | --- | --- |
| `s3 list` (alias `ls`) with **no args** | `-a/--all` | `GET /buckets` | sorted table name/status/region/endpoint (+id, versioning, locking, created-by, created) | HAVE (`buckets list`) |
| `s3 show <name-or-id>` | - | `GET /buckets` (+filter; **there is no `GET /buckets/{id}` in the API**) | - | MISSING, trivial client-side filter, low value |
| `s3 create <bucket-name>` | `--region` (def `ger`; only `ger`/`us-east` accepted), `--versioning`, `--object-lock` (also forces versioning; error if `--versioning=false` is also explicit) | `POST /buckets` | client-side validation | HAVE (`buckets create --object-locking`; flag name differs: add alias `--object-lock`) |
| `s3 update <bucket>` | `--versioning[=false]` (required) | `PATCH /buckets/{id} {versioning}` | refuses to disable versioning when object locking is on | HAVE |
| `s3 delete <bucket>` | `-y/--yes` | `DELETE /buckets/{id}` (202) | confirmation; prints message + `deletesAt` (24h grace period, access keys deleted too) | HAVE (no confirmation) |
| `s3 cors show <bucket>` | - | `GET /buckets/{id}/cors` | table | HAVE (`buckets cors`) |
| `s3 cors set <bucket>` | `--origin` (repeat/comma), `--method` (def `GET,HEAD`), `--header`, `--expose-header`, `--max-age` (def 3600, 0-86400), `--file JSON` (`{"rules":[...]}`; mutually exclusive with the flags) | `PUT /buckets/{id}/cors` | client-side validation: 1-10 rules, origins non-empty, methods in GET/HEAD/PUT/POST/DELETE, max-age range | HAVE (`buckets set-cors`); ours also accepts a bare array in `--file` and has no flag defaults |
| `s3 cors delete <bucket>` | - | `DELETE /buckets/{id}/cors` (204) | prints empty rules | HAVE |
| `s3 key list <bucket>` (aliases `keys`, `ls`) | - | `GET /buckets/{id}/keys` | sorted by name | HAVE (`buckets keys`) |
| `s3 key create <bucket> <name>` | - | `POST /buckets/{id}/keys` | needs rw API token; validates the response (id and both key parts present, else best-effort `DELETE` of the half-made key); secret printed once; does **not** touch the CLI's saved credentials | HAVE (`buckets create-key`; no response validation) |
| `s3 key delete <bucket> <name-or-id>` | `-y/--yes` | `GET /buckets/{id}/keys`, `DELETE /buckets/{id}/keys/{keyId}` (204) | resolves key by name or id; confirmation ("Applications using it lose access"); 404 on delete is treated as already gone; also drops the locally saved credential if it is that key | PARTIAL: ours takes `--key-id` only (name resolution is EASY) |
| `s3 list <bucket> [prefix]` | `-r/--recursive` (no delimiter), `-a/--all` (adds TYPE column) | S3 data plane `ListObjectsV2` | follows every page; folders appear as `directory` entries (CommonPrefixes) when not recursive; sorted by key; JSON entry shape `{key,type,size,lastModified}` | MISSING |
| `s3 upload <bucket> <file> [key]` | `--force` | S3 `HeadObject` then `PutObject` | key defaults to file basename; regular files only; max 5 GiB (single PUT, no multipart); Content-Type from local extension else `application/octet-stream`; refuses to overwrite unless `--force` (HEAD check, not atomic); needs rw API token | MISSING |
| `s3 download <bucket> <key> [file]` | `--force` | S3 `GetObject` | destination defaults to key basename; existing directory -> file inside it; refuses to overwrite unless `--force`; streams to `.sliplane-download-*` temp file in the target dir, verifies byte count equals Content-Length, then `link` (no-clobber) or `rename` (force); progress meter on stderr only when stdout+stderr are TTYs and `-o text` | MISSING |
| `s3 remove <bucket> <key>` (alias `rm`) | `-y/--yes` | S3 `HeadObject` then `DeleteObject` | "not found" error if HEAD is 404 (S3 DELETE alone would succeed silently); confirmation; versioned buckets keep earlier versions behind a delete marker; needs rw API token | MISSING |
| persistent flag `--region` / env `SLIPLANE_S3_REGION` | default `us-east-1` | - | **signing region only** (see 5.1) | MISSING |

Not present in the official CLI: presigned URLs, `sync`, `cp` between buckets/locally, `mv`, multipart upload (so >5 GiB fails), object versions listing, metadata/ACL/lifecycle/object-lock retention
commands, `--content-type` override, recursive upload/download of directories. Any of these would be net-new for us.

### 4.1 Pagination, listing semantics
`ListObjectsV2` with `ContinuationToken`, guarded against a repeating token. Non-recursive uses `delimiter=/` and reports CommonPrefixes as `directory`.
The JSON output is the `[]Entry` array (empty array, not null, when nothing matches).

---

## 5. S3 protocol, endpoint, credentials

### 5.1 Protocol and endpoint
- The data plane is plain **S3 API with AWS Signature V4** (they use `aws-sdk-go-v2/service/s3` with a static credentials provider). No Sliplane-specific protocol.
- Endpoint: the **per-bucket `endpoint` field** returned by `GET /buckets` (spec example: `https://eu-central-2.storage.impossibleapi.net`, i.e. an Impossible-Cloud-style S3-compatible host).
  So there is **no per-region URL table to maintain**: look up the bucket via `GET /buckets` (there is no single-bucket GET), take `endpoint` and `name`.
  They accept only `https` endpoints (plain `http` only for loopback, for tests), no userinfo/query/fragment.
- **Path-style addressing** (`UsePathStyle: true`): requests go to `{endpoint}/{bucketName}/{key}` (list: `{endpoint}/{bucketName}?list-type=2...`). The bucket *name* is used, not the id.
- Signing region: `--region` / `SLIPLANE_S3_REGION`, default **`us-east-1`**, regardless of the bucket region. This is evidently accepted by the backend (their tests and default rely on it) but I could not
  verify it live. If a live test returns a region-mismatch error, the first alternative to try is the region label from the endpoint host.
- SDK knobs worth noting for a hand-rolled client: retries disabled (`RetryMaxAttempts: 1`), redirects not followed, 30 s response-header timeout (no global timeout, so large transfers work),
  and **`RequestChecksumCalculation/ResponseChecksumValidation = WhenRequired`** - i.e. they turn off the SDK's newer default trailing CRC checksums, a well-known incompatibility with third-party S3 backends.
  A hand-rolled signer simply never sends those headers.
- Errors: S3 error XML `<Error><Code>..</Code><Message>..</Message></Error>`; they print `Code: Message` and redact the access key/secret from any message.

### 5.2 Where S3 credentials come from (this is the "credentials / revocation / allowlist" part of your question, which turns out to be different from what you guessed)
- S3 data-plane credentials are **bucket access keys from the control API**: `POST /buckets/{id}/keys {name}` returns `{id, name, accessKeyId, secretAccessKey, createdAt}`; the secret is shown only at creation.
  There is no "IP allowlist" for S3 and no "allowlist IDs" for keys; the IDs you saw are Postgres allowlist rule IDs (`pgfw_...`) and bucket key IDs (`sk_...`).
- Resolution order in `objectClient`:
  1. env `SLIPLANE_S3_ACCESS_KEY_ID` + `SLIPLANE_S3_SECRET_ACCESS_KEY` (both or neither - setting one is an error). Bypasses everything below.
  2. A key saved for (profile, bucket id) in the OS keychain (config.json maps `profile.s3Credentials[bucketId] -> keychain reference`).
  3. If none and the session is an interactive terminal (stdin/stdout/stderr TTY, text output): **auto-provision**. Requires a profile and an rw API token. Asks "Create an S3 access key for bucket X and save it in your keychain
     for profile Y?"; on yes `connectBucket` does: create key named `Sliplane CLI (<profile>)` (`POST /buckets/{id}/keys`), validate fields, store `{keyId, accessKeyId, secretAccessKey}` JSON in the keychain,
     record the reference in config, and **roll back by `DELETE /buckets/{id}/keys/{keyId}`** (with a fresh 30 s context so cancellation cannot block cleanup) if keychain or config writes fail.
  4. Otherwise error with instructions (non-interactive callers must pre-provision or set the env vars).
- **Propagation wait** (`WaitReady`): a new key is not usable instantly. After creating it they poll `ListObjectsV2(max-keys=1)` once per second for up to 1 minute while the answer is
  `InvalidAccessKeyId` or `AccessDenied`; any other error aborts; success ends the wait. On timeout the key stays saved and the message says to retry shortly. Any agent-facing port needs this
  (otherwise the first call after creating a key fails mysteriously).
- "Revocation confirmation" = `s3 key delete` asking "Revoke key ... Applications using it lose access" (TTY prompt, or `--yes`). Our `buckets delete-key` has no prompt; `done()` returns immediately.
  Deleting is `DELETE /buckets/{id}/keys/{keyId}`; there is no server-side "revoke" separate from delete. Deleting the bucket (`s3 delete`) also schedules all its keys for deletion after 24 h.

### 5.3 What we need to port the file operations (ls / upload / download / rm)
Verdict: **MEDIUM** overall; the signer itself is EASY, the transfer plumbing is where the time goes.

New code:
- **SigV4 signer** (~150-200 lines). Crates: `hmac` + `sha2` (neither is a direct dependency today; `ring` is already in the tree via rustls and could do HMAC/SHA-256 instead - a design choice, but direct `hmac`/`sha2` are the idiomatic choice).
  Steps: UTC timestamp `YYYYMMDDTHHMMSSZ` (compute from `SystemTime` by hand, no chrono needed); canonical request = method, canonical URI, sorted canonical query, canonical headers (`host`, `x-amz-content-sha256`, `x-amz-date` at minimum),
  signed-headers list, payload hash; string-to-sign `AWS4-HMAC-SHA256\n<ts>\n<date>/<region>/s3/aws4_request\n<hex sha256(canonical request)>`; signing key = HMAC chain over `"AWS4"+secret`, date, region, `s3`, `aws4_request`; `Authorization` header.
  S3 specifics: URI-encode each path segment of the key (RFC 3986 unreserved set, keep `/`), **do not double-encode** the path (unlike other AWS services), encode query values the same way, sort query params.
  Payload hash: `e3b0c442...b855` (SHA-256 of empty) for GET/HEAD/DELETE; for PUT either hash the file in a streaming pre-pass (constant memory) or send `UNSIGNED-PAYLOAD` (valid over HTTPS on AWS; whether this backend accepts it is unverified - test both).
  Test with the published AWS SigV4 examples (the S3 GET Object example with the documented example key pair in the S3 docs) before touching the network.
- **Separate ureq Agent for the data plane**: our API agent has `timeout_global(100s)` and follows redirects; S3 transfers need no global timeout (per-phase timeouts instead) and **redirects off** (`max_redirects(0)`) so a signed Authorization header is never replayed to another host. Keep `http_status_as_error(false)`.
  ureq 3 can stream a `File` as the PUT body (Content-Length known) and expose the response body as a `Read` for download; I did not compile an example.
- **XML parsing** for `ListBucketResult` (`Contents/Key,Size,LastModified`, `CommonPrefixes/Prefix`, `IsTruncated`, `NextContinuationToken`) and `Error/Code,Message`. Options: hand-rolled tag scanner (fine for this fixed shape, but must unescape `&amp;` etc.), or `quick-xml`. For awkward key characters consider sending `encoding-type=url` and percent-decoding keys (check the backend honours it).
- Commands/behavior to port: `ls` with prefix/recursive and pagination; `upload` (basename default key, regular-file check, 5 GiB cap, `--force` HEAD check, Content-Type by extension); `download` (temp file in target dir, length check, `--force` rename vs no-clobber link, TTY-only progress); `rm` (HEAD first -> not-found error, `--yes`).
- **Credential plumbing** (MEDIUM): env vars with the same names (`SLIPLANE_S3_ACCESS_KEY_ID`, `SLIPLANE_S3_SECRET_ACCESS_KEY`) for drop-in compatibility. This does not violate our "no `SLIPLANE_API_KEY`" invariant: the invariant exists because an API key's reach is the whole organization, whereas a bucket key reaches one bucket and the bucket is named on the command line, so the worst case is AccessDenied, not a deletion in the wrong org.
  For persistence, `secrets::Store` already supports arbitrary string keys (`account:<name>` is just a naming convention), so a `bucketkey:<account>:<bucketId>` entry fits without touching `config.yaml` (keeps the "secrets never in config.yaml" rule).
  For agents I would not copy the implicit TTY prompt; make it an explicit command, e.g. `buckets connect --bucket-id X` (create key named `Sliplane CLI (<account>)`, store it, run the 1-minute readiness poll, roll back the key on store failure), and make `ls/upload/download/rm` fail with that remediation when no key is found.
  Optional alternative that fits agents even better: an `--ephemeral-key` mode that creates a key, waits for readiness, does the operation, then deletes the key - at the cost of a propagation wait and extra API calls on every run.
- Read-only guard: before `upload`/`rm`, require the account's `me.tokenType == "rw"` (we already probe `me`), to preserve the read-only-key promise even when the bucket key could write.
- Need bucket `name` + `endpoint`: `GET /buckets` and match by `--bucket-id` (or accept a bucket name and match on `name`); both fields are in the pinned spec (`Bucket.endpoint`, `Bucket.name`).

Spec check: the **control-plane** endpoints used above (`GET/POST /buckets`, `PATCH/DELETE /buckets/{id}`, `GET/PUT/DELETE /buckets/{id}/cors`, `GET/POST /buckets/{id}/keys`, `DELETE /buckets/{id}/keys/{keyId}`,
`GET /me` + `Sliplane-Client-IP`) are all in our pinned spec 0.11.0. The **S3 data-plane** requests are not part of the Sliplane OpenAPI spec at all (standard S3 against the bucket `endpoint`).

---

## 6. Gap list with portability verdicts

| # | Feature | Status | Verdict | Needs |
| --- | --- | --- | --- | --- |
| 1 | `postgres query` (SQL from arg/file/stdin, `--timeout`, scripts stop on first error) | MISSING | MEDIUM (psql shell-out EASY; hand-rolled over `postgres-protocol` MEDIUM-HARD) | Credentials from existing `GET /postgres/{id}` (spec: yes, needs `secrets:read`); a SQL client (section 2.5); allowlist check (row 2) |
| 2 | Source-IP detection + `postgres allow-me`/`my-ip` and the pre-connect access check | MISSING | EASY-MEDIUM | `GET /me` header `Sliplane-Client-IP` (in spec); `client.rs` must return response headers; one-off ureq agent with `ip_family(...)` + `proxy(None)` (both exist in ureq 3.4.2); CIDR-contains helper (own code or `ipnet`) |
| 3 | `postgres allowlist add/remove/clear/list` (normalised CIDR, dedupe, remove by IP or rule id) | PARTIAL (replace-all via `update --ip-allow`; list via `get`) | EASY | `PATCH /postgres/{id}` already used; pure client-side read-modify-write |
| 4 | S3 `ls` / `download` / `upload` / `rm` | MISSING | MEDIUM | SigV4 signer (`hmac`+`sha2`), XML parsing, data-plane ureq agent without global timeout and without redirects, bucket `endpoint` from `GET /buckets` |
| 5 | Bucket key bootstrap (auto-create, keychain store, readiness poll, rollback on failure, env-var override) | MISSING | MEDIUM | `POST/DELETE /buckets/{id}/keys` (have), `secrets::Store` entries, readiness poll via signed `ListObjectsV2?max-keys=1` (needs row 4) |
| 6 | `postgres connect` interactive shell | MISSING | EASY via `exec psql`; HARD to embed a REPL (skip) | `psql` on PATH |
| 7 | `postgres logs --since 30m` duration form, default limit 100, oldest-first plain lines | PARTIAL | EASY | client-side parse only |
| 8 | `postgres restore` defaulting to latest recoverable point + window validation | PARTIAL | EASY | one `GET /postgres/{id}/restore` first |
| 9 | Key delete by name or id; validate key-create response and roll back a half-made key | PARTIAL | EASY | existing endpoints |
| 10 | Name-or-id resolution for databases/buckets/keys | MISSING (IDs only) | EASY, cross-cutting | one list call per command |
| 11 | Confirmation prompts / `--yes` for delete-class commands | MISSING | design call | conflicts with agent-driven, prompt-free design unless gated on a TTY |
| 12 | Read-only-token guard on S3 writes | MISSING | EASY | `me.tokenType` (field is in `GET /me` per AGENTS.md) |
| 13 | Aliases: `postgres active-connections`, `buckets create --object-lock`, `ls`, `log`, `relation-size` | MISSING | trivial | clap `alias` |
| 14 | `s3 show` single bucket | MISSING | EASY | client-side filter of `GET /buckets`; low value |
| 15 | Presigned URLs (not in official) | n/a | EASY once row 4 exists | same signer using query-string auth, `X-Amz-Expires` up to 7 days; useful for handing an object to a human without sharing keys |

---

## 7. Behaviors/surprises worth knowing before porting

1. **Official redacts the DB password on `postgres show`/`list`; ours does not.** `postgres get` prints whatever the API returns, which includes `connection.password` and `connection.uri` for a key with `secrets:read`. In an LLM transcript that leaks a credential. Their model: password only appears in `rotate-password` output and is otherwise held in memory for SQL sessions. Worth deciding whether `get` should blank those fields by default (an opt-in flag for when the user really wants the URI is the obvious complement). Same consideration for `rotate-credentials`.
2. The official CLI gives agents **no structured SQL output** (`query` refuses `-o json`). Our port can be better than upstream here by returning rows as data.
3. `usql` executes per statement (autocommit); a plain driver's simple-query call with a multi-statement string is one implicit transaction. Decide which semantics `query` promises, and document it in `agent-readme`.
4. The upload "does it already exist" check is HEAD-then-PUT and not atomic; S3 conditional writes (`If-None-Match: *`) would be stricter but support on this backend is unverified.
5. `s3 delete` of a bucket schedules deletion in 24 h and takes all keys with it; `postgres delete` is immediate and irreversible - both upstream commands are confirm-gated; ours are not.
6. Time: Postgres log/restore timestamps are RFC 3339 with nanoseconds; their parser is `time.RFC3339Nano`.
7. Rate limits (spec): other endpoints 400/min; the 1-per-second readiness poll is well within that.
8. Everything above was read from source and tests only; the exact acceptance of `us-east-1` signing, `UNSIGNED-PAYLOAD`, and public-CA trust for `*.db.sliplane.app` are the three assumptions to verify first on a throwaway bucket/database.

---

## 8. Suggested port order

1. Allowlist trio + `postgres my-ip` / `allow-me` (EASY, no new crates, immediately useful, prerequisite for 2).
2. `postgres query` (JSON/YAML rows) - choose C (hand-rolled on `postgres-protocol` + existing rustls) if keeping zero-async matters, otherwise B; add `connect` as a psql wrapper.
3. S3 signer + `buckets ls/download/upload/rm` with env-var credentials, then `buckets connect` (keystore + readiness poll).
4. Small wins any time: logs duration `--since`, restore default, aliases, key-delete by name, read-only guard, password redaction decision.
