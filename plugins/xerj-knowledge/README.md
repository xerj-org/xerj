# XERJ Knowledge: local document search for Claude Cowork and Claude Code

Ask Claude about your own files and get answers that say which file, and which
page, each fact came from. XERJ indexes a folder on your computer: PDFs, Word,
PowerPoint and Excel files, EPUB books, notes, and mail or mbox exports. **Your
files stay on your machine.** Nothing is uploaded; the index is local too.

This plugin is for documents. For code search use the `xerj` plugin from the same
marketplace. Both use the same `xerj mcp` server and can be installed together.

## Install

1. Install the `xerj` program (one time): https://xerj.org/docs/install
2. Start it, and leave it running:

   ```bash
   xerj --insecure --data-dir ~/xerj-data
   ```

3. Add the plugin:

   ```bash
   claude plugin marketplace add xerj-org/xerj
   claude plugin install xerj-knowledge@xerj
   ```

## Use

- `/xerj-knowledge:index ~/Documents/Contracts contracts` indexes a folder under
  the name `contracts`. Run it again any time to pick up changed files.
- `/xerj-knowledge:find when does the office lease renew?` searches and answers
  with citations.
- Or just ask in plain words ("find the invoice from Acme in my mail export");
  the included skill tells Claude when to search your documents.

## What is stored, and how to remove it

The index lives in the data folder you started XERJ with (`~/xerj-data` above).
To remove one folder's index: `xerj autoindex remove --prefix <name>`. To remove
everything, stop XERJ and delete that data folder.

## Status

- **Untested in Claude Cowork.** Cowork uses the same plugin format as Claude
  Code, and this plugin uses only the same `xerj mcp` server as the `xerj`
  plugin plus a skill and two commands, but nobody has run it in Cowork yet.
  Reports are welcome in https://github.com/xerj-org/xerj/issues.
- `XERJ_URL` defaults to `http://localhost:9200`. For a node with auth on, set
  `XERJ_AUTH` (for example `ApiKey <key>`); on a local node the key is read from
  `<data-dir>/admin.key`, so you can leave it unset.
