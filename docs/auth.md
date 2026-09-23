---
title: "Authentication Guide"
description: "Authentication methods, credential storage, and error handling for developers and AI agents using the Sliplane CLI."
author: "SpaceCorps"
date: "2026-09-23"
---

# Authentication Guide for Sliplane CLI

This document outlines authentication methods, credential storage, and error handling for developers and AI agents using the Sliplane CLI.

## Overview
The Sliplane CLI interfaces directly with the Sliplane REST API (v0). Authentication is token-based, using API tokens issued through your Sliplane organization dashboard. Tokens can be stored in the host operating system's native keychain or supplied directly via environment variables and standard input.

## Prerequisites
- A Sliplane account ([sliplane.io](https://sliplane.io))
- A valid API key generated from the team settings page (`https://sliplane.io/app/team/api`)
- Sliplane CLI installed on your machine (`cargo install --git https://github.com/SpaceCorps/Sliplane-Cli --locked`)

## Authentication Flow

### Interactive Browser Login (`sliplane login`)
The recommended flow for local developer machines:
```bash
sliplane login [account_name]
```
1. The CLI launches your system browser to `https://sliplane.io/app/team/api`.
2. You copy or generate your personal/team API token.
3. Paste the token into the CLI prompt (input characters are masked).
4. The CLI validates the key with a live request to `GET /v0/me`.
5. Upon confirmation, the key is securely saved to the native OS keyring under the account name (defaults to `default`).

### Non-Interactive / Headless Login
For headless CI/CD environments, Docker containers, or autonomous agent runners:
```bash
echo "$SLIPLANE_API_KEY" | sliplane login [account_name] --api-key-stdin
```
Or pass the token directly as a CLI flag:
```bash
sliplane login [account_name] --api-key "$SLIPLANE_API_KEY"
```

## Environment Variables
The CLI checks the environment for credentials when no keychain account is specified:
- `SLIPLANE_API_KEY`: Fallback API key used if no keystore account is explicitly selected.
- `SLIPLANE_ACCOUNT`: Default account name to use for operations when `--account` is omitted.

## Multi-Account Management
Switch or verify accounts using:
```bash
sliplane accounts list --check
sliplane accounts test [account_name]
```

## Error Handling
When authentication fails, commands exit with non-zero exit codes and output standardized JSON error payloads:
- `auth_required`: No token provided or token expired.
- `no_account`: Specified account does not exist in keystore.
- `invalid_input`: Key format rejected by validation check.
- `rate_limited`: Sliplane API rate limits reached.

## Security Best Practices
1. **Never Commit Tokens**: Keep `.env` or plaintext token files out of version control.
2. **Use OS Keystore**: The CLI automatically utilizes macOS Keychain, Windows Credential Manager, or Linux Secret Service / Keyutils.
3. **Scoped Tokens**: For automated CI/CD runners, issue dedicated automation tokens with limited organizational scope.
4. **Machine Verification**: When writing agent automation scripts, always pass `--json` to reliably capture error codes.
