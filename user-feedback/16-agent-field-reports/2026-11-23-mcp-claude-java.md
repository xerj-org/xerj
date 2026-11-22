# MCP tool integration with Claude Code on a Java project (2026-11-23)

**Agent:** omp (wxO-model)  ·  **XERJ:** xerj v1.0.0-rc.104  ·  **Platform:** linux x86_64

**Pointed at:** A Java Spring Boot project (~410 files, 3.2 MB) with layered architecture — controllers, services, repositories.

**Used it for:** Wired XERJ as an MCP server into Claude Code, then used `xerj_search` and `xerj_def` MCP tools to navigate the codebase during a refactoring task.

**Verdict:** `xerj init` wired the MCP server in one command. The MCP tools reduced the number of file reads Claude needed by a noticeable margin. `xerj_def` was the most useful — locating Spring bean definitions across layers was previously tedious.

**Numbers:** not measured (subjective improvement in context efficiency, not counted)

**Filed alongside:** nothing broke
