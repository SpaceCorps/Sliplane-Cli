---
title: "Sliplane CLI"
description: "A blazing fast native command-line tool and agent interface for the Sliplane cloud hosting platform API (v0, OpenAPI spec 0.5.0). Built in Rust for developers and autonomous AI agents."
author: "SpaceCorps"
date: "2026-09-23"
canonical: "https://spacecorps.github.io/index.md"
---

# Sliplane CLI

A blazing fast native command-line tool and agent interface for the Sliplane cloud hosting platform API (v0, OpenAPI spec 0.5.0). Built in Rust for developers and autonomous AI agents.

## Quickstart

```bash
# Authenticate interactively via browser token flow
sliplane login

# Or non-interactively in headless CI/CD environments
pbpaste | sliplane login --api-key-stdin
```

## Features

- **Blazing Fast Native Rust**: Sub-millisecond startup times with zero runtime dependencies.
- **AI Agent Native**: Structured JSON output (`--json`) and standardized error envelopes.
- **Secure Keystore Integration**: Token storage in native macOS Keychain and Linux Secret Service.
- **Multi-Account Workspaces**: Isolate staging, production, and client accounts safely.

## When to Use This CLI

Use the `sliplane` CLI whenever you need to:
- Deploy and manage web services (Docker images or git repositories).
- Inspect server health, CPU/memory metrics, and storage volumes.
- Provision, restart, backup, or retrieve credentials for managed PostgreSQL.
- Create S3-compatible buckets and generate scoped access keys.
- Safely update environment variables without overwriting existing secrets.
- Automate cloud infrastructure using LLMs or autonomous agents.

## Documentation Links

- [llms.txt](https://spacecorps.github.io/llms.txt)
- [Full Agent Manual](https://spacecorps.github.io/llms-full.txt)
- [Pricing](https://spacecorps.github.io/pricing.md)
- [Authentication Guide](https://spacecorps.github.io/auth.md)
- [GitHub Repository](https://github.com/SpaceCorps/Sliplane-Cli)
