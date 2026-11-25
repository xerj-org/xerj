# The local node: background start and auto-start

Last updated 2026-11-22 (#3267). If this page and `xerj --help` /
`xerj mcp --help` disagree, the help text wins.

Agents call `xerj mcp`, `xerj search`, `xerj def`, `xerj code` and
`xerj autoindex map` against a node on this machine. If the node has stopped,
these commands start it again themselves. `xerj --detach` starts a node that
keeps running after the terminal closes. The flag names are unchanged:
`--detach`, `--no-autostart` and `XERJ_NO_AUTOSTART`.

## Start a node that outlives the terminal

```sh
xerj --detach --insecure --data-dir ~/xerj-data
```

Verify (expected: `"status":"green"` or `"yellow"`):

```sh
curl -s localhost:9200/_cluster/health
```

`--detach` starts the same command without `--detach` as a background child.
It waits until the node answers, prints its pid, URL, data dir and log, and
then returns. The node writes its output to `<data_dir>/node.log`. Stop it
with `kill <pid>` (Linux/macOS) or `taskkill /PID <pid>` (Windows).

## Auto-start

When a client's URL is loopback (`localhost`, `127.0.0.1`, `::1`) and nothing
accepts there, the client:

1. Finds the data dir that last served that port, from
   `~/.xerj/by-port/<port>.data-dir`.
2. Starts that node again with the flags it recorded in
   `<data_dir>/node.launch.json`.
3. Waits until `/_cluster/health` answers. The default wait is 120 s; set
   `XERJ_AUTOSTART_TIMEOUT_SECS` to change it.

Then the client runs the call. The MCP tool result gets one extra text item
that says the node was started, with its pid, data dir and log. If a node is
already starting on that data dir, the client waits for it and does not start
a second one. A node that is shutting down closes its port first and then
flushes for up to ~10 s. If the client is waiting on such a node, it starts a
fresh node as soon as the old one exits; it does not wait out the timeout.

Auto-start never runs for a URL that is not loopback. Turn it off with
`XERJ_NO_AUTOSTART=1` or `xerj mcp --no-autostart`. With auto-start off, the
error gives the exact start command.

If neither `--url` nor `XERJ_URL` is set, the port is read from what local
nodes recorded, so a node on `:9280` is found. The hub's own pack node is
never picked.

## How it is detached

| | What the child gets | What no longer reaches it |
|---|---|---|
| Linux, macOS | `setsid()`: a new session and process group (sid = pgid = its pid) and no controlling terminal. stdin is `/dev/null`; stdout and stderr go to the log. | SIGHUP when the terminal closes, a shell's `kill -- -<pgid>` of its job, Ctrl-C and Ctrl-Z in that terminal, and SIGPIPE/EIO from a closed pipe or terminal. |
| Windows | `DETACHED_PROCESS \| CREATE_NEW_PROCESS_GROUP`, plus `CREATE_BREAKAWAY_FROM_JOB`. If the job forbids breakaway, the spawn is retried without it. stdin is closed; output goes to the log. | The console closing, and Ctrl-C/Ctrl-Break in it. Leaving the job also means the job closing does not end the node, so `cmd start /B` dying with its shell is fixed. |

Every node a client starts is detached this way: `xerj --detach`, MCP/CLI
auto-start, the hub's pack node, background `xerj hub pull`s, and the server
`xerj brain` boots. The Unix behaviour is covered by tests in
`engine/crates/xerj-common/src/localnode.rs`. One test stops a stand-in
terminal session (SIGSTOP), then hangs it up and kills it (SIGHUP, SIGTERM,
SIGKILL to its process group). A child started the normal way stops and dies
with it. The detached child keeps running.

## Stopping it

One SIGINT (Ctrl-C) or SIGTERM starts a graceful stop: new requests get 503,
in-flight requests and gRPC calls get up to a third of the budget (at most
10 s), memtables are flushed to segments, then the ports close. The whole
stop takes at most `--shutdown-timeout` seconds (env
`XERJ_SHUTDOWN_TIMEOUT_SECS`, default 30, range 1-3600). When the budget runs
out the node exits anyway. A second signal exits at once. Both forced exits
fsync the WAL first, so acknowledged writes replay on the next start; the exit
code is 130 for SIGINT and 143 for SIGTERM. A signal sent while the node is
still starting is acted on too, also within the budget (#3418).

## Limits

- **Logout and reboot.** Being detached from a terminal does not protect the
  node from a whole-login kill (systemd `KillUserProcesses=yes`, a container
  or CI job tearing down its cgroup) or from a reboot. A service manager is
  the answer there: `xerj service install` writes a systemd `--user` unit
  (Linux) or a launchd agent (macOS) and starts it; on Windows it prints the
  Task Scheduler command instead. See `xerj service --help` and
  [the CLI reference](https://xerj.org/docs/cli#service).
- **Windows**: the spawn code compiles for `x86_64-pc-windows-msvc`, but no
  test runs it on Windows.
- **A node stopped with SIGSTOP or Ctrl-Z** in its own terminal still
  accepts TCP connections, so clients do not start a second one. Their calls
  time out instead (30 s per MCP call). If it was stopped before it opened
  its port, the client reports the stop at once with `kill -CONT <pid>`
  instead of waiting. Resume it with `kill -CONT <pid>`, or use `fg` in that
  terminal.
