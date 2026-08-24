---
title: "How does XERJ handle authorization?"
evidence:
  - claim: "GET /_security/roles self-reports enforced:false and full superuser access"
    source: "docs/SECURITY_MODEL.md"
  - claim: "authorize_index confines a scoped API key to its named indices"
    source: "docs/SECURITY_MODEL.md"
---

# How does XERJ handle authorization?

XERJ authorizes with API-key principal scoping. A scoped key is confined to the
indices you name when you mint it, and the `.xerj-memory-*` namespace is
reserved so one agent cannot read another's memory.

XERJ does not implement RBAC. The seeded roles are stored but not enforced, and
the node says so itself: `GET /_security/roles` returns `"enforced": false` and
warns that every authenticated caller has full superuser access regardless of
any role assignment. There is no SSO, SAML, OIDC or LDAP either. Terminate
identity at your proxy on the single-node deployment and pass a scoped key
through.
