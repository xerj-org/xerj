# Security Policy

We take the security of XERJ seriously. Thank you for helping keep XERJ and its
users safe.

## Supported Versions

XERJ is currently in the release-candidate phase leading up to 1.0.0. Security
fixes are provided for the latest release-candidate line.

| Version        | Supported          |
| -------------- | ------------------ |
| 1.0.0-rc.x     | :white_check_mark: |
| < 1.0.0-rc.1   | :x:                |

Once 1.0.0 is released, this table will be updated to reflect the supported
stable release line. We recommend always running the most recent release.

## Reporting a Vulnerability

**Please do not report security vulnerabilities through public GitHub issues,
discussions, or pull requests.** Public disclosure before a fix is available
puts all users at risk.

Instead, report vulnerabilities privately via email:

> **security@xerj.org**

Alternatively, you may use GitHub's private vulnerability reporting for the
`xerj-org/xerj` repository (Security → Report a vulnerability), which routes the
report to the maintainers confidentially.

To help us triage and resolve the issue quickly, please include as much of the
following as you can:

- A description of the vulnerability and its potential impact
- The affected component, version, or commit (e.g. `1.0.0-rc.1`)
- Step-by-step instructions to reproduce the issue
- A proof-of-concept, if available
- Any known mitigations or workarounds
- How you would like to be credited in the advisory (optional)

## Our Commitment

When you report a vulnerability to us, we will:

- **Acknowledge** receipt of your report within **72 hours**.
- Provide an **initial assessment** and triage, typically within **7 days**.
- Keep you informed of our progress toward a fix and a remediation timeline.
- Notify you when the vulnerability has been resolved.
- Credit you in the security advisory for your responsible disclosure, unless
  you prefer to remain anonymous.

Please make a good-faith effort to avoid privacy violations, data destruction,
and service disruption while investigating, and only interact with systems and
accounts you own or for which you have explicit permission to test.

## Disclosure Policy

We follow a **coordinated disclosure** process:

1. You report the vulnerability privately using one of the channels above.
2. We confirm the issue, determine the affected versions, and develop a fix.
3. We prepare a release containing the fix and, where appropriate, a published
   security advisory (via GitHub Security Advisories / CVE).
4. We publicly disclose the vulnerability after a fix is available, coordinating
   the timing with you where possible.

We ask that reporters give us a reasonable opportunity to remediate an issue
before any public disclosure. We aim to resolve critical issues promptly and
will work with you to agree on a disclosure timeline.

## Scope

This policy applies to the XERJ engine and its official crates in the
`xerj-org/xerj` repository. Vulnerabilities in third-party dependencies should be
reported to the respective upstream projects; if a dependency issue affects
XERJ, we welcome a report so we can update or mitigate accordingly.

Thank you for helping to keep XERJ secure.
