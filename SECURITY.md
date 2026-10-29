# Security Policy

We take the security of XERJ seriously. Thank you for helping keep XERJ and its users safe.

---

## Supported Versions

XERJ is currently in the Release Candidate (RC) phase leading up to `1.0.0`. Security fixes are provided exclusively for the latest active RC release line.

| Version | Status |
| :--- | :---: |
| **`1.0.0-rc.x`** | :white_check_mark: Supported |
| **`< 1.0.0-rc.1`** | :x: Unsupported |

*Note: Once version `1.0.0` is released, this table will be updated to reflect the supported stable release line. We always recommend running the most recent release.*

---

## Reporting a Vulnerability

> **Important:** Please do not report security vulnerabilities through public GitHub issues, discussions, or pull requests. Public disclosure before a fix is available puts all users at risk.

If you have discovered a vulnerability, please report it privately through one of the following channels:

1. **Official Security Email:** Send a report to **`security@xerj.org`**.
2. **GitHub Private Vulnerability Reporting:** Submit a report directly via the [`xerj-org/xerj`](https://github.com/xerj-org/xerj) repository under **Security** → **Report a vulnerability**.

### Reporting Guidelines
To help us triage and resolve the issue quickly, please include as much of the following details as possible:

* A clear description of the vulnerability and its potential impact.
* The affected component, version, or commit (e.g., `1.0.0-rc.1`).
* Step-by-step instructions or scripts to reproduce the issue.
* A Proof-of-Concept (PoC), if available.
* Any known mitigations or workarounds.
* How you would like to be credited in the advisory (optional).

---

## Our Commitment

When you report a vulnerability to us, we commit to:

* **Acknowledge receipt** of your report within **72 hours**.
* Provide an **initial assessment** and triage, typically within **7 days**.
* Keep you informed of our progress toward a fix and remediation timeline.
* Notify you when the vulnerability has been resolved.
* Credit you in the security advisory for your responsible disclosure, unless you prefer to remain anonymous.

Please make a good-faith effort to avoid privacy violations, data destruction, and service disruption during your investigation. Only interact with systems and accounts you own or for which you have explicit permission to test.

---

## Coordinated Disclosure Policy

We follow a **coordinated disclosure** process:

1. **Private Report:** You submit the vulnerability privately through one of the designated channels.
2. **Triaging & Fix:** We confirm the issue, assess affected versions, and develop a fix.
3. **Release Preparation:** We prepare a release containing the fix and, where appropriate, a public security advisory (via GitHub Security Advisories / CVE).
4. **Public Disclosure:** We publicly disclose the vulnerability once the fix is available, coordinating the timing with you whenever possible.

We ask that reporters give us a reasonable opportunity to remediate an issue prior to any public disclosure. We aim to resolve critical issues promptly and will work with you to agree on a disclosure timeline.

---

## Automated CI/CD Security Checks

The following automated security checks run in [`.github/workflows/ci.yml`](.github/workflows/ci.yml) on pushes to `main` and pull requests that change code. The workflow skips these jobs for changes classified as documentation-only. If you notice a gap between this document and the actual workflow file, that itself is a bug worth reporting.

* **`security-audit`:** Runs [`cargo-audit`](https://github.com/rustsec/rustsec) against `engine/Cargo.lock`.
  * RUSTSEC *vulnerability* advisories will fail the build.
  * *Informational* advisories (unmaintained, unsound, or yanked crates) are logged in the output but will not fail the pipeline, as they typically involve transitive dependencies we cannot directly upgrade.
* **`fuzz`:** Uses [`cargo-fuzz`](https://github.com/rust-fuzz/cargo-fuzz) harnesses in [`engine/fuzz/`](engine/fuzz/) to test parsers exposed to unauthenticated requests:
  * Elasticsearch query DSL and Lucene `query_string` grammar.
  * Date math, date-format patterns, and index-name date math.
  * SQL queries and `xerj_engine::painless` scripts.
  * Script tokenizers in the aggregation engine (`scripted_metric`, `bucket_script`, and `bucket_selector`).

Each fuzz target replays a checked-in seed corpus and then fuzzes for a bounded budget. This list describes the covered parsers; it does not cover every parser in the engine.

> *Note: Automated checks are not a replacement for security reports. Fuzzing uncovers crashes and hangs rather than logic flaws, and advisory databases only list previously published vulnerabilities.*

---

## Scope

This policy applies to the **XERJ** engine and its official crates within the `xerj-org/xerj` repository.

Vulnerabilities in third-party dependencies should be reported to their respective upstream projects. However, if a third-party dependency bug impacts XERJ directly, we welcome a report so we can update or mitigate the component accordingly.

Thank you for helping keep XERJ secure.
