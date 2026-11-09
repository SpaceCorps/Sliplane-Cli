# Follow-ups

Known work that is not done, written down so it does not live only in someone's head. Each item says why it
matters and how to start. Delete an item when it ships. `PARITY.md` has its own **Open** and **Declined**
rows for the official-CLI comparison; this list is for everything else.

Last reviewed after v1.3.0 (2026-11-09).

## Security

- **macOS Keychain writes put the secret in argv.** `secrets.rs` stores with
  `security add-generic-password ... -w <value>`, so for as long as that process runs the value shows up in
  `ps`. This applies to API keys and, since v1.3.0, to S3 secret keys, which breaks the "secrets never in
  argv" rule on macOS. Reading is fine (`-w` there only asks for the password on stdout). Fix: feed the
  command to `security -i` on stdin instead of passing the value as an argument. Needs a Mac to test; the
  Keychain trust behaviour in AGENTS.md ("goes through `/usr/bin/security`") must be kept.
- **`accounts remove` orphans S3 keystore entries.** It deletes the account's API key
  (`secrets::account_key`) but not the `s3:{account}:{bucketId}` entries that `s3 connect` made
  (`secrets::s3_key`), so the S3 secrets of a removed account stay in the keystore. Fix: list and delete
  them, and say in the output that the keys are not revoked (same wording rule as AGENTS.md).
- **No open-list guard on `postgres update --ip-allow 0.0.0.0/0` or `postgres create`.** `allowlist add`
  refuses a `/0` entry without `--allow-open`; these two commands can still open a database to the world.
  Give them the same guard.

## Documentation

- **`docs/llms.txt` and `docs/index.html` show a command that does not exist**:
  `sliplane buckets keys create --server-id ... --bucket-id ...`. The commands are `buckets create-key`,
  `buckets keys` (list) and `buckets delete-key`.
- **The docs site and `llms.txt` do not mention anything added in v1.3.0**: `services ssh|tunnel`,
  `presets`, `--wait`, `logs --follow`, `postgres query|allowlist`, `s3`, `update`, `agent init`,
  `--env-file`, name-or-id flags, and `--show-secrets`. `llms-full.txt` and `index.md` are likewise behind.
  `src/readme.rs` and `README.md` are current.

## Never run against a real Sliplane account

Everything in v1.3.0 was tested against mocks and local MinIO, PostgreSQL and sshd. A throwaway project
on a real account (it costs money, so it is the owner's call) would settle each of these:

- `--wait`: that `service_deploy_success` and the failure events arrive when the code expects.
- `$SLIPLANE_DOMAIN` in nine presets is sent literally; whether the platform expands it.
- The `Sliplane-Client-IP` header on `GET /me`, what an empty allow list does, and whether rule ids survive
  a PATCH. `ctrl.sliplane.io` has an A record and no AAAA, so `--my-ip --ipv6` probably fails there.
- S3: the signing region (`us-east-1`), the SHA-256 payload mode, `encoding-type=url`, and how long a new
  key takes to activate.
- SSH: whether the gateway accepts the `exit 0` probe, allows forwarding to `localhost:<port>`, how long a
  key takes to propagate, and whether an open session survives the key's expiry.
- Postgres hosts: the certificate chain (we trust Mozilla's roots), SNI, whether they accept
  `statement_timeout` and `default_transaction_read_only` as startup parameters (a pooler would refuse), and
  what a blocked IP looks like on the wire.
- Whether the API accepts a bare `nginx` as an image (we always send the full `docker.io/library/...`).
- Clearing a field on update by sending an empty value (PARITY.md, **Open**).
- `sliplane update` across two real releases: v1.3.0 is the first with `checksums.txt`, so the first real
  update is v1.3.0 to the next one.

## Smaller items

- **Postgres query**: type names for custom OIDs, a per-cell size cap, a fallback to `SET` statements when a
  proxy refuses startup parameters, channel binding, an offer to add the caller's address to the allow list,
  and a CI job for the Docker test (`cargo test --test cli postgres_query_against_a_real_postgres -- --ignored`).
- **S3**: a CI job that runs the real-server mode against MinIO (`bitnamilegacy/minio`, see AGENTS.md).
- **ssh and tunnel**: `--remote-port` defaulting from the service's `network.internalPort`; `-L` and `-N`
  passthrough; servers older than OpenSSH 7.6 have no `accept-new`; Ctrl-C on Windows and everything on
  macOS were never run. A remote exit status of 255 cannot be told from ssh's own failure.
- **update**: no cross-process lock (the official CLI has one for its background updater); Windows on ARM gets
  "no prebuilt release" although `install.ps1` hands it the x64 build.
- **Validation**: `postgres logs --until` is not validated.
- **Tests**: `tests/cli.rs`'s `Env::run` does not strip proxy environment variables. One Windows CI run failed on
  `s3_files_against_the_mock` ("The download was interrupted: Peer disconnected") and never reproduced; the
  mock now closes its connections politely, which is the likely cause. If it returns, look at the client's
  download path in `s3.rs` next.
- **Duplication**: `registry_host()` in `commands/services.rs` partly repeats the host detection in `image.rs`.
- **Line endings**: on Windows, `cargo fmt` and appended text mix CRLF and LF in a few files. Git normalises on
  commit and warns; a `.gitattributes` rule for `*.rs` and `*.md` would end the warnings.
