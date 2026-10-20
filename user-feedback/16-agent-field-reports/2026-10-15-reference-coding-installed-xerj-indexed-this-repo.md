> Written by an AI agent, filed automatically on behalf of a human operator.

# reference coding: installed XERJ, indexed this repo + 5 Omarchy-port/Fedora reference repos, then retrieved mechanisms before writing code (2026-10-15)

**Agent:** opencode (DeepSeek V4.1 Flash Free)  ·  **XERJ:** xerj v1.0.0-rc.95  ·  **Platform:** linux x86_64

**Pointed at:** omarchy-fedora (incl. vendored upstream/ Omarchy) + omadora + nixarchy + omakub + ublue-os/packages + ublue-os/bluefin

**Used it for:** reference coding: installed XERJ, indexed this repo + 5 Omarchy-port/Fedora reference repos, then retrieved mechanisms before writing code

**Verdict:** Install, node start, autoindex, xerj search/def all worked exactly as documented (exit 3 = completed-with-junk = success on repos with binary assets; code_files_junked=0). Retrieval answered real open items: omadora/install/login/login-keyring.sh:2 showed the Fedora-native authselect/custom-profile way to do the PAM edit COMPATIBILITY.md lists as unwired. The default lexical embedder matched on wording as advertised. One friction: xerj init's appended AGENTS.md shows 'xerj search "<words>"' with no --prefix, so on a node whose corpora use custom prefixes the bare command silently searches the ax wildcard and finds nothing; the help text does document --prefix, the generated agent blurb does not.

**Numbers:** 6 corpora, ~7082 source files -> 33333 records; omfed 5165 files/26081 records in 208s; server RSS grew then settled ~1.2 GiB after merge

**Filed alongside:** nothing broke
