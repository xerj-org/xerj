# Postmortem: checkout latency spike

On the evening of the release, our payment service began timing out. The
root cause was connection-pool exhaustion after a config change reduced the
maximum number of open database connections. We restored service by rolling
back the change and raising the pool ceiling.

Follow-ups: add a saturation alert on the pool, and load-test connection
limits before shipping config that touches them.
