# xerj-security-audit — copyable Claude Code skill

`SKILL.md` is a self-contained Claude Code skill that encodes the whole
coverage-guaranteed whitebox audit workflow (setup → map → AST census → prove
0 gaps → index → AI-enrich → reason).

## Install
```bash
mkdir -p ~/.claude/skills/xerj-security-audit         # or <repo>/.claude/skills/...
cp SKILL.md ~/.claude/skills/xerj-security-audit/SKILL.md
cp -r ../sink-census ~/.claude/skills/xerj-security-audit/   # the scripts + catalog
```
Then invoke it in Claude Code (`/xerj-security-audit`) or just ask to
"run the XERJ security audit" on a target codebase. Retarget another stack by
extending `php_dangerous_functions.json` (the taint model is data, not code).
