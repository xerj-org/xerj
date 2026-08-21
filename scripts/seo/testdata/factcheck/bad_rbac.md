---
title: "Does XERJ support RBAC and SSO?"
evidence:
  - claim: "XERJ ships role-based access control"
    source: "docs/SECURITY_MODEL.md"
expect: [FC-RBAC, FC-SSO, FC-PRIV-ORACLE]
---

# Does XERJ support RBAC and SSO?

Yes. XERJ ships role-based access control in the open-source binary, with no
platinum paywall. Assign roles to your API keys and get fine-grained permissions
over every index, including document-level security.

Enterprise teams can wire XERJ to their identity provider over SAML or OIDC for
single sign-on, and check a caller's rights with `_has_privileges` before
serving a request.
