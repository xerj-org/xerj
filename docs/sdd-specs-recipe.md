# Spec-Driven Development (OpenSpec & github/spec-kit) Search Recipe

When `xerj autoindex` runs on a repository that uses [OpenSpec](https://github.com/Fission-AI/OpenSpec) (`openspec/`) or [github/spec-kit](https://github.com/github/spec-kit) (`.specify/` and `specs/<feature>/spec.md`), it automatically indexes the project's specifications, constitutions, proposals, and architecture decision records into a dedicated `<prefix>-specs` corpus alongside `<prefix>-docs`.

## Included SDD Paths

When an OpenSpec or `github/spec-kit` layout is detected in a repository scope, the following paths are routed into `<prefix>-specs` by default:

- `openspec/**` (`openspec/project.md`, `openspec/specs/**`, `openspec/changes/**`)
- `.specify/**` (`.specify/memory/constitution.md`, `.specify/specs/**`, `.specify/templates/**`; any dotfile inside `.specify/`, such as `.specify/.env`, remains excluded)
- `specs/**` (`specs/<feature>/spec.md`, `plan.md`, `tasks.md`, `data-model.md`, `contracts/**`)
- `adr/**`, `adrs/**`, `docs/adr/**`, `docs/adrs/**`, `docs/decisions/**`, `docs/specs/**`, `rfcs/**`

## Coding-Agent Recipe

1. **Index the repository once**:
   ```bash
   xerj autoindex . --prefix myproj --no-graph
   ```
   This creates `myproj-specs` (requirements, proposals, constitutions, ADRs) and `myproj-docs` (source code and general documentation).

2. **Search requirements and ADRs with citations before editing code**:
   ```bash
   xerj search --prefix myproj-specs "session token expiration requirement"
   ```

3. **Search implementation symbols and code in the companion corpus**:
   ```bash
   xerj search --prefix myproj-docs "verify_session_token"
   ```
