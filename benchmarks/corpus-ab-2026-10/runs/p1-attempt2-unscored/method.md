I read the router, auth, authz and share core directly and swept es_compat.rs (45k lines) for
unchecked slicing and unvalidated parsing. Four parallel subagents covered the xerj-wasm ingest
processors, xerj-autoindex hostile-file parsing, xerj-storage/xerj-engine persistence readers, and
the console/static surface, each reporting file:line with confirmed-versus-suspected labels.

I re-verified the load-bearing claims myself: booting the prebuilt release binary reproduced F1
exactly (one unauthenticated request, listeners permanently dead) and gave measured timings for F2
(17.9s at a 7KB pattern, 58.5s at 21KB). Storage-reader claims were reproduced by the subagent
against xerj-storage in a scratch crate.

Ranked by attacker reach: unauthenticated remote first, then malicious corpus, then data-dir
tampering. The console/static surface was audited and found genuinely clean apart from minor
cache-control and nosniff gaps that I did not spend a slot on.
