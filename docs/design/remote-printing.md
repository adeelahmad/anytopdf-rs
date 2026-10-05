# Remote printing design note (backlog item 4)

Status: implemented in `crates/anytopdf-print` and `anytopdf print` (front, users, guard, discovery). Wiring receipts and `doctor` waits on backlog item 3's helper, which owns the IPP server itself (a PAPPL helper on localhost that spools PWG/Apple Raster and runs `anytopdf convert`).

Decisions taken: discovery is multicast DNS on the LAN plus unicast DNS-SD records and a manual URL for remote clients (owner asked for IPP, AirPrint-style, DNS and mDNS); configuration is command-line flags plus a JSON users file instead of a TOML config; the CIDR allowlist is hand-rolled, so `ipnet` is not a dependency.

## Scope

The README backlog line is "Remote printing over Tailscale or WireGuard with DNS-based discovery". Read together with the item 3 lines ("IPP over TLS with a password, localhost by default, print receipts on the provenance page"), remote printing means:

- a phone or laptop that is not on the same LAN can print to anytopdf and get a searchable PDF out;
- transport is the user's existing private overlay network (Tailscale or plain WireGuard); anytopdf does not ship a VPN, a relay or a cloud service;
- clients find the printer by DNS, because multicast mDNS does not cross a WireGuard tunnel.

Out of scope: exposing the printer to the public internet, Microsoft Universal Print (separate backlog line), and the IPP server internals (item 3).

## What item 4 adds on top of item 3

Item 3 gives a helper process that speaks IPP Everywhere, bound to localhost by default, and hands each received job to the convert pipeline. Item 4 is a thin layer of policy around that helper:

1. **Remote listen mode.** A config/CLI switch that lets the helper bind a non-loopback address. Default stays `127.0.0.1`. The intended values are the host's tailnet address (Tailscale `100.x.y.z` / `fd7a:115c:a1e0::/48`) or the WireGuard interface address, not `0.0.0.0`.
2. **Hard guard on non-loopback binds.** anytopdf refuses to start the helper on any non-loopback address unless all of these hold:
   - TLS is configured (`ipps://`), with a cert and key file, or a Tailscale cert obtained by `tailscale cert`;
   - IPP authentication is on (HTTP Basic over TLS, password stored as an Argon2 hash in the config, never on the command line);
   - a peer allowlist is set (CIDRs). The default suggestion is the tailnet ranges `100.64.0.0/10` and `fd7a:115c:a1e0::/48`.
   Binding `0.0.0.0` or `::` additionally needs an explicit `--i-understand-public-bind` style flag. These are refusals with a named exit code, not warnings.
3. **Peer allowlist enforcement.** Connections from outside the allowlist are closed before any IPP parsing. Since item 3's helper may be PAPPL (C, out of process), the allowlist is enforced either by the helper's own listener options or by a small Rust TCP front (accept, check peer, splice to the loopback helper). The Rust front is the default plan because it works the same for PAPPL, ippeveprinter or a Rust IPP server, and keeps TLS termination in our code (rustls).
4. **DNS-based discovery (unicast DNS-SD, RFC 6763).** A command prints the records a user adds to their DNS so Apple clients can browse the printer across the tunnel:
   ```
   anytopdf print dns-sd --domain home.example --host printer.home.example --port 8631
   ```
   emitting `b._dns-sd._udp`, `lb._dns-sd._udp`, `_ipps._tcp` PTR, the SRV record and the TXT record (`rp=ipp/print/anytopdf`, `pdl=...`, `TLS=1.2`, `air=username,password`, `UUID=...`), as a zone-file snippet and as JSON. The user serves them from any DNS server their tailnet uses (Tailscale split DNS to a resolver they run, or the WireGuard peer's resolver). anytopdf runs no DNS server.
5. **Manual fallback.** `anytopdf print url` prints the exact `ipps://host:port/ipp/print/anytopdf` URL for clients that cannot browse unicast DNS-SD (Windows "add printer by URL", Android Mopria "add printer").
6. **Receipts.** Each remote job's provenance records the peer address, authenticated user name, job id and time. It is written only into the visible provenance/manifest page under the `archive` profile; the `share` profile drops peer address and user name. Nothing goes into the invisible text layer (existing rule).
7. **Untrusted input.** Remote jobs go through the same caps item 3 and the security backlog define (size and page caps, per-sender budgets keyed by authenticated user, sandboxed conversion with no network). Item 4 only adds the per-user budget key.
8. **doctor.** `anytopdf doctor` reports the print helper, whether remote mode is configured, cert expiry, and whether `tailscale` is on PATH (for `tailscale cert` and to show the tailnet address). Informational only.

## Commands

```
anytopdf print passwd <user> --users users.json          # password on stdin, Argon2id hash, file mode 0600
anytopdf print remote --listen 100.x.y.z:8631 --allow-tailnet \
    --tls-cert host.crt --tls-key host.key --users users.json [--upstream 127.0.0.1:8631]
anytopdf print dns-sd --domain home.example --host printer.home.example [--address IP] [--json]
anytopdf print advertise [--port 8631] [--host anytopdf.local]   # mDNS on the LAN
anytopdf print url --host printer.home.example
```

Port 8631 instead of 631 so neither process needs root. The front and the helper can both use 8631 because the front binds the tailnet address and the helper binds loopback. Guard refusals exit 2 (usage).

mDNS: mdns-sd answers one subtype per instance, so the LAN record is `_ipps._tcp` with the AirPrint `_universal` subtype; the unicast zone also lists the IPP Everywhere `_print` subtype.

## Interface with item 3

The helper (`helpers/anytopdf-printer`, PR #14) listens on loopback in plaintext and does no TLS or authentication; its address comes from `ANYTOPDF_PRINTER_LISTEN`/`ANYTOPDF_PRINTER_PORT` (default `localhost:8631`) and its printer path is `/ipp/print/anytopdf`, which the DNS-SD `rp` key and `print url` advertise. The front points `--upstream` at it. PAPPL does not pass the peer address to jobs, so receipts will come from the front's log or a wrapper set as `ANYTOPDF_BIN`. Jobs reach the pipeline through the helper's spool and `anytopdf convert`, so remote printing adds no importer. The integration test uses a stand-in helper on loopback.

## Dependencies added

`rustls` 0.23 (ring provider), `rustls-pki-types` (PEM parsing), `argon2` 0.5, `base64ct`, `mdns-sd` 0.21; all MIT or Apache-2.0, all build on Rust 1.88.

## Receipts and doctor

- The front parses each request's HTTP framing (Content-Length or chunked, answering `Expect: 100-continue` itself) and the IPP operation attributes only, and replaces `requesting-user-name` with the signed-in user, so PAPPL's `job-originating-user-name` (and `ANYTOPDF_PRINT_USER`) is the authenticated user. Document bytes pass through untouched. The front also rewrites `Host` to the helper's loopback address, because PAPPL answers 400 to a host name it does not know (found by the print-server thread against PAPPL 1.3.1); PAPPL builds `printer-uri-supported` from its own host name, not from `Host`. A connection that signed in once keeps its user for later requests; a new `Authorization` header is re-verified.
- `--receipts FILE` appends `anytopdf.print-receipt/1` JSON lines (unix time, peer address, user, operation, request id, job name) for Print-Job, Print-URI, Create-Job, Send-Document and Send-URI.
- `convert` turns `ANYTOPDF_PRINT_JOB_ID/JOB_NAME/USER/FORMAT` into `print.*` source metadata, which the manifest carries and the provenance page shows as "Print job", "Printed by" and "Print format". The `share` profile drops them.
- `doctor` lists `remote-front`, `anytopdf-printer` and `tailscale` under "Printing" (`printing` in `--json`).

## End-to-end test

`helpers/anytopdf-printer/remote-smoke.sh` (`make printer-remote-smoke`, run by the Linux "Print server" CI job) starts the real helper and the front, checks that an unauthenticated Print-Job gets 401, prints a page over TLS as `adeel` while the client claims `mallory`, and checks `ANYTOPDF_PRINT_USER=adeel`, `print.user` in the PDF's manifest and the receipt's peer address.

## Remaining follow-ups

- Older iOS releases that browse only `_ipp._tcp` will not see an `_ipps`-only advertisement.
