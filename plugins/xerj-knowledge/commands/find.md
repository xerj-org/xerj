---
description: Search your indexed local documents and answer with the file and page each fact came from
argument-hint: <question>
---

Answer this from the user's indexed local documents: $ARGUMENTS

1. Search with the `xerj_search` MCP tool (or `xerj search --prefix <prefix>
   "<plain words>"`), using the words the documents would use. If you know the
   prefix the folder was indexed under, pass it. Try two or three phrasings if the
   first one misses.
2. Answer only from what the hits say. Cite each fact with the file path and the
   page, slide, sheet or message the hit names.
3. If nothing relevant is found, say so and say which prefix was searched. Do not
   answer from memory.
