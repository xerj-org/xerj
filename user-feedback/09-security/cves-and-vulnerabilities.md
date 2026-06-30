# CVEs and Security Vulnerabilities

## Severity: CRITICAL | Frequency: Ongoing

---

## Critical Vulnerabilities (CVSS 9.0+)

### CVE-2021-44228 -- Log4Shell (CVSS 10.0)
- The infamous Log4j2 RCE vulnerability
- Affected ES 5.5.3 through 6.7.0+
- HTTP body, headers, and URL all attack vectors
- Required JVM flag changes or physical class removal to mitigate
- Elasticsearch inherited this from its Java dependency

### CVE-2015-1427 (CVSS 9.8)
- Groovy scripting engine sandbox bypass
- Remote attackers could execute arbitrary shell commands
- Affected ES before 1.3.8 and 1.4.x before 1.4.3

### CVE-2015-5377 (CVSS 9.8)
- Remote code execution via transport protocol
- Affected ES before 1.6.1

### CVE-2025-25015 (CVSS 9.9)
- Kibana prototype pollution
- Arbitrary code execution
- 2025 vulnerability

---

## High Vulnerabilities (CVSS 7.0-8.9)

### CVE-2024-43709 (CVSS 7.5)
- Specially crafted SQL query causes OutOfMemoryError and node crash
- No resource allocation limits
- Disclosed January 2025

### CVE-2023-31419 (CVSS 7.5)
- Stack overflow DoS via crafted query strings to search API

### CVE-2023-31418 (CVSS 7.5)
- Unauthenticated users force OutOfMemory via malformed HTTP requests
- Crashes nodes without authentication

### CVE-2021-37937 (CVSS 8.8)
- Fleet-Server API keys could escalate to super-user privileges

### CVE-2018-3831 (CVSS 8.8)
- Passwords and tokens exposed in plain text via API when secrets configured through ES API

---

## Pattern: Java Ecosystem Vulnerabilities
- Log4Shell was not an ES bug -- it was a Java dependency vulnerability
- ES inherited the risk by using the JVM ecosystem
- **Rust has no equivalent attack surface**: no Log4j, no Groovy sandbox, no Java serialization exploits

---

## XERJ.ai Response
- **No JVM = no Java supply chain vulnerabilities**
- No Log4j, no Groovy, no Java deserialization
- Rust memory safety eliminates entire classes of CVEs (buffer overflows, use-after-free)
- Minimal dependency tree: Rust crates are statically linked, auditable
- Single binary: smaller attack surface than ES + Kibana + Logstash + plugins
- `cargo-audit` for dependency vulnerability scanning
- Fuzz testing (cargo-fuzz) on all input parsing paths

## Sources
- Elastic Security Advisory ESA-2021-31
- stack.watch/product/elastic/elasticsearch
- NVD (National Vulnerability Database)
