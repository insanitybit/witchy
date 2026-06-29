---
name: curl-security
description: Security requirements for curl commands. Use when writing or reviewing curl commands in shell scripts, documentation, CI/CD configs, or any code that executes curl.
user-invocable: false
---

# Curl Security Requirements

When writing or suggesting `curl` commands, always include these security flags:

- `--proto '=https'` - Restrict to HTTPS only (prevents accidental HTTP requests)
- `--tlsv1.2` - Require TLS 1.2 as the minimum version

Example:

```bash
curl --proto '=https' --tlsv1.2 https://example.com/api/endpoint
```

This applies to:

- Shell scripts
- Documentation examples
- CI/CD configurations
- Any code that executes curl commands programmatically
