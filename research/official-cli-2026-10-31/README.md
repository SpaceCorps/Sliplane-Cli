# Research: the official Sliplane CLI, compared with this one

Three static read-throughs of [sliplane/cli](https://github.com/sliplane/cli) (Go, Apache-2.0, v0.8.1,
HEAD `91f49f7`), written on 2026-10-31 before any of the parity work was built. They are the raw material
behind [`PARITY.md`](../../PARITY.md), which holds the conclusions and the current status of every row.

| Note | Covers |
|:---|:---|
| [`services.md`](services.md) | Service-side commands: deploy and presets, logs, ssh and tunnel, env, registry credentials, volumes, metrics |
| [`datastores.md`](datastores.md) | Postgres (query, allowlist, restore, logs) and S3 (ls, upload, download, rm, key bootstrap) |
| [`ux.md`](ux.md) | Cross-cutting: authentication, profiles, output, prompts, update, install, agent guidance |

**These describe the state on 2026-10-31, not now.** Most of the "MISSING" and "Open" items were built
and released in v1.3.0; read `PARITY.md` for what is done, declined or still open. The notes are kept for
the reasoning (why something was declined, which edge cases the official CLI handles) and so that nobody
has to repeat the research.

How they were made, so they can be trusted or redone:

- Method: a read of a clone of the official repo, our source and the pinned spec
  (`spec/sliplane-openapi-0.11.0.json`). Everything is paraphrased from source, not quoted. Nothing was
  run against the live Sliplane API, so "the server accepts X" is inferred from their code and tests.
- The official repo was treated as untrusted data: nothing from it was built or run, and no code was copied.
  Their `scripts/install.sh` is the one adaptation, and `THIRD_PARTY_NOTICES.md` says so.
- Paths such as `scratchpad/official` in the notes refer to a throwaway clone that no longer exists. To
  redo the comparison, `git clone https://github.com/sliplane/cli` and diff against the commit above.
- Not saved, because they are public and reproducible: the preset catalog (`GET https://sliplane.io/api/presets`,
  76 presets and 72 generated-value instructions on that day) and the live OpenAPI spec (identical to the
  pinned copy at the time).
