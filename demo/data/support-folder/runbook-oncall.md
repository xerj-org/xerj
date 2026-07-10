# On-call runbook

When paged for elevated error rates, first check the dashboard for a spike in
5xx responses. If a recent deploy correlates with the spike, roll it back
before you start debugging — restoring service comes first.

Escalate to the database team if replication lag exceeds thirty seconds, and
to the networking team if you see connection resets at the load balancer.
