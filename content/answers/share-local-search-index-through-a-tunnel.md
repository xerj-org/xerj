---
title: "Share a local search index through a tunnel"
h1: "How do I let someone outside my network search an index on my laptop without opening a port?"
description: "xerj share --tunnel starts your own cloudflared, prints a public guest link, and closes the tunnel and revokes the share on Ctrl-C. What the trade is."
slug: "share-local-search-index-through-a-tunnel"
cluster: "Sharing: read-only guest access"
question: "How can I give a remote person temporary search access to files on my laptop without port forwarding or a VPN?"
intent: "how-to"
published: "2026-09-18"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt. Confirm cloudflared is installed, then run xerj share on the user's indexed folder with --tunnel in the background, wait for the line that says the tunnel is up, and give the user the https link and the passcode separately. Tell them the link stops working when the command stops, and that stopping it revokes the share."
commands:
  - cmd: "xerj share ./casefiles --label 'for Dana' --tunnel"
    note: "Open a temporary public address and print the full guest link. Runs until Ctrl-C, then closes the tunnel and revokes the share."
  - cmd: "xerj share ./casefiles --tunnel --keep"
    note: "Same, but leave the share active after Ctrl-C. The link stops working anyway, because the hostname is gone."
  - cmd: "xerj share ./casefiles --public-url https://search.example.org"
    note: "For a node you already publish at your own hostname: print the link for that address and open no tunnel."
links_out:
  - "share-folder-read-only-search-without-uploading"
  - "what-a-share-link-guest-can-reach"
  - "search-engine-without-docker"
  - "/docs/security"
evidence:
  - claim: "xerj share --tunnel runs cloudflared tunnel --url against 127.0.0.1 and reads the trycloudflare.com hostname from its output."
    source: "engine/crates/xerj-server/src/share.rs"
  - claim: "A real quick tunnel was exercised on 2026-09-18: page 200, wrong passcode 401, claim 200, guest _cat 403, and after SIGINT the guest key answered 401 with 1 key invalidated."
    source: "docs/usecases/share-links/VERIFICATION.md"
  - claim: "A known share is limited to 30 passcode attempts an hour from anywhere, so the lockout does not depend on the source address."
    source: "engine/crates/xerj-api/src/share.rs"
  - claim: "Forwarded headers are read only when the peer is listed in server.trusted_proxies."
    source: "engine/crates/xerj-console-api/src/client_ip.rs"
faq:
  - q: "How can I give a remote person temporary search access to files on my laptop without port forwarding or a VPN?"
    a: "Run `xerj share` on the indexed folder with `--tunnel`. XERJ starts your own `cloudflared`, prints a public HTTPS link, and revokes the share when you stop the command."
  - q: "What happens when I press Ctrl-C?"
    a: "The tunnel is closed and the share is revoked, which invalidates every guest key it minted. Add `--keep` to leave the share active. The link still stops working, because the hostname is gone."
  - q: "Do I need a Cloudflare account?"
    a: "Not for a quick tunnel. You do need the `cloudflared` binary. If it is missing, `xerj share` prints the install steps for your platform and the local link, and does not fail."
  - q: "Can Cloudflare read what my guest searches for?"
    a: "It terminates the HTTPS connection, so in principle yes. The folder is not uploaded there, but everything between the guest's browser and your node crosses its network: the passcode, the guest key, the searches and the documents the guest opens. The command prints this with the link, and the guest page shows it. Use your own hostname and certificate with `--public-url` when that matters."
  - q: "Why does the guest get a passcode and not a passkey?"
    a: "A passkey is bound to a hostname, and a quick tunnel gets a new random hostname on every start. A passkey made through one tunnel would not work through the next."
  - q: "Every guest arrives from 127.0.0.1 through a tunnel. Does that break the passcode lockout?"
    a: "No. The lockout is charged to the share, not to the address: 30 attempts an hour from anywhere. Junk ids are charged to the source address, where one shared bucket only makes the bound tighter."
---

**TL;DR** — `xerj share` with `--tunnel` starts your own `cloudflared`, prints a public HTTPS guest link, and runs until you press Ctrl-C. Stopping it closes the tunnel and revokes the share. No port is opened on your router. The trade: the guest's traffic passes through Cloudflare, which terminates the TLS connection.

## What the command does

It checks that the node is enforcing authentication, then starts `cloudflared tunnel --url http://127.0.0.1:<port>` as a child process and reads the `trycloudflare.com` hostname from its output. The tunnel comes up before the share is created, so a link is never printed for an address that is not there.

Then it creates the share and prints the full link, the passcode, the expiry and the revoke command. It keeps running while your guest reads.

On Ctrl-C, on SIGTERM, or when the terminal closes, it terminates the tunnel and revokes the share. With `--keep` the share stays active on the node until it expires.

If `cloudflared` is not installed, the command prints the install steps for your platform and the local link. It does not fail.

## What was verified, and what was not

On 2026-09-18 the command was run against a real quick tunnel from a Linux host and the public hostname was exercised over IPv4:

| Request through the tunnel | Result |
| --- | --- |
| guest page | 200, with the full security header set |
| claim with a wrong passcode | 401 |
| claim with the right passcode | 200 |
| guest search on the shared index | 200 |
| guest `_cat/indices` | 403 |

After SIGINT the `cloudflared` process was gone, the share listed as `revoked` with 1 key invalidated, and the guest key answered 401.

That run cannot be repeated in CI, because it needs the public internet and a third party. It also predates the review fixes: the claim then carried the share id in its path, and the command did not yet print the Cloudflare notice. CI covers the hostname parser, the missing-`cloudflared` fallback, and a stub `cloudflared` that checks the notice and what happens when the tunnel dies. A browser was not driven through a real tunnel. The browser test serves the page under a `trycloudflare.com` name that points at the local node, and checks the page's notice there.

## Three properties of a quick tunnel

**The hostname is random and new on every start.** A link stops working when the command stops. That is the intended lifetime.

**Cloudflare terminates TLS.** The corpus is not uploaded there. Everything between the guest's browser and your node does cross Cloudflare's network, and the operator of a TLS endpoint can read what passes through it. That includes the passcode, the guest's API key, every search and every document the guest opens. `xerj share --tunnel` prints this with the link. The guest page shows it on a `trycloudflare.com` address before the passcode is typed. When that is not acceptable, publish the node at your own hostname behind your own certificate and pass it as `--public-url`.

**The share id is never in a URL the page requests.** The id is in the link's fragment, which a browser does not send. The page then sends it in the body of `POST /_share/claim`. So the id is not in the request line that a proxy or a tunnel logs. Cloudflare still carries that body and can read it, together with the passcode.

**When the tunnel drops on its own, the command says why.** It prints the last lines `cloudflared` wrote, closes the tunnel and revokes the share.

**It is temporary by design.** Cloudflare offers quick tunnels without an account and makes no promise about how long one stays up. For a standing arrangement use a named tunnel on your own domain, or your own reverse proxy.

## Why guests do not get passkeys

The XERJ Console signs its owner in with a passkey. A passkey is bound to the hostname it was created on, which is what makes it resistant to phishing. A quick tunnel gets a new random hostname each time it starts, so a passkey enrolled through one tunnel is unusable through the next.

That is why a share is a link plus a passcode. The route to guest passkeys is a stable hostname: a named Cloudflare Tunnel on your own domain, optionally with Cloudflare Access in front, or any reverse proxy with your own certificate. Guest passkeys are not built.

## The rate limiter behind a tunnel

Through a tunnel every request reaches the node from `127.0.0.1`. A limiter keyed on the source address first would put every guest in one bucket, and a few junk requests would lock the real guest out.

XERJ does not fix that by trusting `X-Forwarded-For`, because a direct client can write that header itself. A claim against a real share is charged to the share: 30 attempts an hour, from anywhere. Junk ids are charged to the source address. The two are not stacked.

To log each guest's real address in the audit log, list loopback under `trusted_proxies` in the `[server]` section of the node's config and restart it. Only then is the forwarded header read. The command tells you which mode your node is in.

## What this does not do

`--tunnel` fronts a plain-http node on this machine only. For a node with TLS on, run your own tunnel or proxy and use `--public-url`.

It does not work on a node started with `--insecure`. The command refuses before it opens a tunnel.

XERJ is single-node. The share is reachable only while that one host and that one command are running.
