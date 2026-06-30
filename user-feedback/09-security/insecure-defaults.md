# Security: Insecure Defaults and Breaches

## Severity: CRITICAL | Frequency: MODERATE

---

## Core Complaints

### Historical Default: No Authentication
- Port 9200 was historically exposed without ANY authentication
- "Default, simple install is almost instantly hackable" -- John B., Senior Network Engineer (Capterra)
- Free tier lacked built-in security in older versions
- Improved since ES 6.8/7.1 with free-tier security, but legacy clusters carry this debt
- ES 8.x enables security by default, but upgrade paths from older versions remain risky

### Major Data Breaches (All from Misconfigured ES)
- 1.2 billion user records exposed (2019)
- 184 million login credentials exposed (May 2025)
- Adobe Creative Cloud: 7.5 million accounts exposed
- Tens of millions of text messages with password info exposed (2018)
- 24 million sensitive financial documents exposed (2019)
- "60% of NoSQL data breaches are with Elasticsearch databases"

### Configuration Complexity
- Transport-layer TLS mandatory for production with security enabled
- Every node is both client and server: certificates need dual-purpose (clientAuth + serverAuth)
- Separate TLS config for REST layer (HTTP) and transport layer (inter-node)
- Certificate management across dozens of nodes is operationally burdensome
- A single expired certificate can take down the entire cluster
- Security touches: elasticsearch.yml, keystore, truststore, role mappings, realm chains, API keys

### Critical Vulnerabilities
- CVE-2025-25015 (CVSS 9.9): Kibana prototype pollution enabling arbitrary code execution

### Feature Gating
- Fine-grained RBAC and audit logging: paid tiers
- SSO/SAML: Enterprise only
- SOC2 compliance reports: Enterprise subscription required

---

## User Quotes

> "You will leak customer data and may end up with brand recognition that you don't want"
> -- John B., Senior Network Engineer, Oil & Energy (Capterra)

---

## XERJ.ai Response
- **Secure by default from day one**
- TLS enabled out of the box (single node = one certificate)
- API key authentication enabled by default
- No separate transport layer to secure (single-node M1)
- No paid tier for security features
- Minimal attack surface: single binary, no plugins, no Java deserialization risks

## Sources
- Capterra, G2 reviews
- Coralogix: 5 Common Elasticsearch Mistakes That Lead to Data Breaches
- Elastic Blog: Configure TLS/SSL & PKI
- CVE databases
