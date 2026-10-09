# Security Policy

Toktol is a local-first application. **This page covers more ground than most
projects** — for Toktol, the privacy promises and product-level boundary
violations are treated as security issues on equal footing with technical
vulnerabilities.

## Supported versions

Toktol is at `0.1.0`. During 0.x we only
maintain the latest patch release; security fixes ship in new versions, with
no backports to older ones.

## What counts as a security issue

Conventional technical vulnerabilities do: remote code execution, path
traversal, injection, unauthorized reading of local files, dependency-chain
attacks, and so on.

**Beyond that, the following are security incidents for this product**, because
the README and CONTRIBUTING state them as explicit constraints:

- Sending data off the machine without an explicit user action (telemetry,
  usage reporting, crash uploads, etc.)
- Reading API keys, tokens, or other credentials from environment variables,
  config files, or the system keychain
- Writing outside `~/.toktol/`, or reading directories unrelated to AI tool
  usage
- Modifying the original session logs of AI tools (read-only is a promise)
- Hard-deleting sessions instead of going through the OS trash
- Inventing estimated costs for models with no known price

Any one of these is a violation of this project's privacy red lines even when
"technically harmless" — please report it through the channel below.

## Out of scope

Please use a regular issue instead of the security channel for the following,
so that real incidents are not diluted:

- Attacks that require physical access to the device or an already-compromised
  local account
- Known vulnerabilities in the dependencies themselves (we track them through
  routine dependency upgrades)
- The currently logged-in local user reading their own `~/.toktol/` data —
  that is by design

## How to report

**Please do not describe security issues in public issues.** Use GitHub's
private Security Advisories, including:

- The affected version or commit
- Reproduction steps
- The observed behavior versus the expected behavior
- (If relevant) the operating system and version

## Response expectations

This project is currently maintained by one person, and **we do not commit to
a fixed response time**. Reports are taken seriously; once an issue is
confirmed, a fix ships as soon as possible. If we cannot reproduce a report,
we will come back to you for details.

If you ask for credit in your report, we will include it.

## Related documents

- Privacy red lines and contribution rules: [CONTRIBUTING.md](CONTRIBUTING.md) (Chinese)
- Product promises: [README.md](README.md)
