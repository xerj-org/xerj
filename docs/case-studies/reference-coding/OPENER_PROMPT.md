Set me up for **reference-coding with XERJ** and then use it automatically for the
rest of our work, so I stop burning output tokens re-deriving APIs I could just
look up.

1. Make sure a local XERJ instance is running and the `xerj-code` skill is
   available. If XERJ isn't installed, install it from https://xerj.org/get
   (`curl -fsSL https://xerj.org/get | sh`) and start it (`xerj -d ./.xerj-data
   --insecure`).
2. Look at this project — its dependencies, the domains its code touches — and
   decide which well-known open-source libraries already solve those problems.
   Clone them into a reference corpus with `xerj corpus add <name> <git-url>...`,
   grouped by problem domain (async, parsers, storage, …), not by language.
3. Index that corpus with `xerj corpus index <name>` (it runs `xerj autoindex`).
4. From now on, for **every** coding task this session: before you write
   non-trivial code against an unfamiliar, external, or internal-but-uncommon
   API, first run `xerj code <corpus> "<what you need>"` to retrieve the real
   implementation, read the returned definition, and adapt it — cite `file:line`
   when you rely on it, and check the licence before copying. If retrieval
   returns nothing relevant, say so and fall back to normal work. If you've looped
   twice on the same error, retrieve before trying a third time.
5. Re-index whenever the references change; refuse a stale index.

Do this proactively for the rest of the session without me asking again.
