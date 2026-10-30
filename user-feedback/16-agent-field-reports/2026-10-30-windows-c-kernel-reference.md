# Reference-coding an x86 C kernel on native Windows (2026-10-30)

**Pointed at:** `WuXiaoMuer/muos` — a 32-bit x86 hobby kernel written in C (84 files: `src/`, `include/`, `user/`, plus three Chinese-language docs describing the ELF loader and syscall ABI).

**Used it for:** `autoindex` + lexical BM25 search and `xerj def` symbol lookup, while mapping the Ring3 / ELF32 / `int 0x80` subsystem.

**Verdict:** The MSVC release ran with no extra setup: `autoindex` finished 84 files / 1750 records in 21.1 s and was immediately searchable, and `xerj def elf32_load_ex` returned the definition and the declaration with real line numbers — symbol lookup is the best part of the tool. Lexical search was weaker on this repo: for "where does the ELF32 loader map PT_LOAD segments" the top two hits were `src/syscall.c` (mmap sandbox) and `src/test.c`, not `src/elf.c`; only a literal `PT_LOAD p_filesz p_memsz` query surfaced the loader, and it surfaced through CODE_LOGIC.md rather than the source. Three prose docs plus Chinese comments appear to swamp code volume on shared tokens, and `def` on a guessed name (`elf_load`) returned no definition. I would reach for it again for symbol-level lookup, and would not rely on lexical mode alone for concept-level questions in a bilingual repository.

**Numbers:** `xerj autoindex e:\github\muos --prefix muos` -> ok=true, wall=21.1s, files=84, records=1750, code_files_indexed=67, junk_files=0. `xerj def elf32_load_ex` -> 2 exact hits (`src/elf.c:162`, `include/elf.h:117`). Not measured: per-query latency, resident memory.

**Filed alongside:** nothing broke; this report is the record.
