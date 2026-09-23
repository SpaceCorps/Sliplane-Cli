# Authentication Guide for Sliplane CLI

This document outlines authentication methods, credential storage, and error handling for developers and AI agents using the Sliplane CLI.

## Methods of Authentication

### 1. Interactive Browser Flow (`sliplane login`)
Run:
```bash
sliplane login [account_name]
```
- Opens `https://sliplane.io/app/team/api` in your default browser.
- Prompts for your Sliplane API key with hidden input.
- Validates the token against the Sliplane API (`GET /v0/me`).
- Securely stores the key in the native OS credential store (macOS Keychain, Linux Secret Service / Keyutils).

### 2. Headless / Non-Interactive Authentication
For automated scripts, CI/CD pipelines, and agent loops:
```bash
echo "$SLIPLANE_API_KEY" | sliplane login [account_name] --api-key-stdin
```
Or pass the key as a flag:
```bash
sliplane login [account_name] --api-key "$SLIPLANE_API_KEY"
```

### 3. Environment Variable Fallback
Set `SLIPLANE_API_KEY`:
```bash
export SLIPLANE_API_KEY="your-api-key"
```

## Multi-Account Management
Switch or verify accounts using:
```bash
sliplane accounts list --check
sliplane accounts test [account_name]
```

## Structured Error Codes
When authentication fails, commands exit with non-zero exit codes and output JSON errors:
- `auth_required`: No token provided or token expired.
- `no_account`: Specified account does not exist.
- `invalid_input`: Key format rejected.
