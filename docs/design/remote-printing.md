# Remote printing design note (backlog item 4)

Status: draft for review. Depends on backlog item 3 (network printer helper), which owns the IPP server itself.

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
   emitting `b._dns-sd._udp`, `lb._dns-sd._udp`, `_ipps._tcp` PTR, the SRV record and the TXT record (`rp=ipp/print`, `pdl=...`, `TLS=1.2`, `air=username,password`, `UUID=...`), as a zone-file snippet and as JSON. The user serves them from any DNS server their tailnet uses (Tailscale split DNS to a resolver they run, or the WireGuard peer's resolver). anytopdf runs no DNS server.
5. **Manual fallback.** `anytopdf print url` prints the exact `ipps://host:port/ipp/print` URL plus a QR code (text) for clients that cannot browse unicast DNS-SD (Windows "add printer by URL", Android Mopria "add printer").
6. **Receipts.** Each remote job's provenance records the peer address, authenticated user name, job id and time. It is written only into the visible provenance/manifest page under the `archive` profile; the `share` profile drops peer address and user name. Nothing goes into the invisible text layer (existing rule).
7. **Untrusted input.** Remote jobs go through the same caps item 3 and the security backlog define (size and page caps, per-sender budgets keyed by authenticated user, sandboxed conversion with no network). Item 4 only adds the per-user budget key.
8. **doctor.** `anytopdf doctor` reports the print helper, whether remote mode is configured, cert expiry, and whether `tailscale` is on PATH (for `tailscale cert` and to show the tailnet address). Informational only.

## Config shape (proposed)

```toml
[print]
listen = "127.0.0.1:8631"          # item 3 default

[print.remote]                      # item 4; absent means remote mode off
listen = "100.101.102.103:8631"
tls_cert = "/path/printer.crt"
tls_key = "/path/printer.key"
users = { adeel = "$argon2id$..." } # set with `anytopdf print passwd <user>`
allow = ["100.64.0.0/10", "fd7a:115c:a1e0::/48"]
dns_sd_domain = "home.example"      # only used by `print dns-sd`
```

Port 8631 instead of 631 so the helper never needs root.

## Interface needed from item 3

To build this I need, from the print-server thread:

- how the helper is launched (binary name, args or config file) and how it receives listen address, so the Rust front can point it at loopback;
- whether TLS and Basic auth live in the helper or can be left to the front (preferred: front does both, helper stays loopback-only and plaintext);
- the job hand-off (how a received job reaches `convert`, and where the job metadata lives) so receipts can add peer and user fields.

## Plan

1. This note (review).
2. Without waiting on item 3: config parsing and the non-loopback guard, CIDR allowlist, `print dns-sd` record generator, `print url`, password hashing, with unit tests. Pure Rust, no network in tests, no new C dependencies.
3. Once item 3's helper exists: the rustls front that splices to it, receipts, doctor lines, an integration test with a loopback client.
4. Docs: README section and SECURITY notes for remote mode.

## Open questions for the owner

- Is unicast DNS-SD plus a manual URL enough, or must discovery work with zero DNS setup? Zero setup across a tunnel would need an mDNS reflector on the remote side, which this note does not propose.
- New dependencies for step 2/3: `rustls` (+ `rustls-pemfile`), `argon2`, `ipnet`. All MIT/Apache-2.0. OK to add?

## Unverified assumptions

- Apple clients browse unicast DNS-SD from `b._dns-sd._udp.<search domain>`; confirmed by RFC 6763 and Apple behaviour, not tested here against Tailscale MagicDNS search domains.
- Tailscale's admin DNS settings can split-DNS a domain to a user resolver but cannot host SRV/PTR records directly (inferred, not checked).
- Windows and Android do not browse unicast DNS-SD by default (inferred); they use the manual URL path.
