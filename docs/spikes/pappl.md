# PAPPL feasibility spike (S2-12-T3, R24)

## Question

Can PAPPL (the C Printer Application framework) host anytopdf's network-printer feature (backlog item 3: IPP Everywhere / AirPrint / Mopria printer, IPP over TLS, localhost by default) and the remote-printing follow-up (backlog item 4)? Specifically: platform support (Windows in particular), build and runtime dependencies (CUPS libraries, DNS-SD, TLS), static linking per release target, licence fit with this project's MIT OR Apache-2.0, IPP Everywhere / AirPrint / Mopria coverage, and alternatives if PAPPL is unsuitable.

## Time box and method

- Date: 2026-10-04 (UTC). Time box: 20-25 minutes.
- Method: desk research only. No scratch experiment, no build, no measurement was performed; nothing was compiled or run. Sources are the PAPPL and libcups repositories (README, INSTALL.md, NOTICE, configure.ac, vcnet project files, source files read via the GitHub API on the master branch), PWG, Apple, Mopria and Microsoft pages. Any figure not in a source is labelled "estimate" with its basis.

## Evidence

| # | Claim | Source |
|---|-------|--------|
| E1 | PAPPL requires Windows 10+ or a POSIX host (Linux, macOS, QNX, VxWorks); Windows needs Visual Studio 2019+ and the provided project files | PAPPL README.md "Requirements"; INSTALL.md |
| E2 | Required libs: Avahi 0.8+ or mDNSResponder; libcups 3.0+ (or CUPS 2.5+ per README; INSTALL.md still says CUPS 2.2+); GnuTLS 3.0+, LibreSSL 3.0+ or OpenSSL 1.1+; zlib. Optional: libjpeg, libpng, libusb, libpam | PAPPL README.md, INSTALL.md |
| E3 | Windows build pulls libcups3, libjpeg-turbo, libpng, LibreSSL, zlib via NuGet; the vcxproj ConfigurationType is DynamicLibrary (DLL) | vcnet/packages.config, vcnet/libpappl2.vcxproj |
| E4 | Latest PAPPL release at research time: v1.4.12 (2026-08-20). Master is the v2.0 beta line that requires libcups 3 | GitHub releases API; CHANGES.md "Changes in v2.0b1" |
| E5 | libcups 3 selects DNS-SD at build time: mDNSResponder, the Windows DnsService* API (windns.h, dnsapi.lib), or Avahi | libcups cups/dnssd.c |
| E6 | libcups latest release v3.0.3 (2026-08-20) | GitHub releases API |
| E7 | configure.ac builds a static libpappl2.a by default (--disable-static turns it off); the shared library is the default product on Windows | PAPPL configure.ac; vcnet/libpappl2.vcxproj |
| E8 | PAPPL licence: Apache-2.0 plus an optional exception: embedded object-form portions may be redistributed without the 4(a), 4(b), 4(d) conditions, and a GPLv2 combination may waive patent/indemnity sections. libcups uses the same terms | PAPPL LICENSE, NOTICE; libcups NOTICE |
| E9 | Avahi is LGPL-2.1; mDNSResponder is Apache-2.0 (Apple) | GitHub licence metadata for avahi/avahi and apple-oss-distributions/mDNSResponder (the latter reported NOASSERTION by the API; Apache-2.0 is from the project's own README, estimate-level confidence) |
| E10 | PAPPL supports JPEG, PNG, PWG Raster, Apple Raster and raw printing; provides an embedded IPP Everywhere service; "helping" with AirPrint and Mopria (no claim of being certified) | PAPPL README.md |
| E11 | PAPPL v2 adds support for CUPS 3 `ipptransform` for PDF and plain-text printing | PAPPL CHANGES.md |
| E12 | IPP Everywhere (PWG 5100.14) requires PWG Raster and JPEG (colour); PDF is recommended; discovery is DNS-SD; a PWG self-certification programme exists | PWG IPP Everywhere page |
| E13 | AirPrint needs Bonjour (mDNS) discovery, IPP, URF/PDF/JPEG; Apple runs a licensing programme (contact Apple) | Wikipedia AirPrint summary, Apple developer forum thread |
| E14 | Mopria certification is member-only, supports PCLm, PWG Raster or PDF; Windows 10 21H2+ prints to Mopria devices through the inbox Microsoft IPP Class Driver | Mopria certification page, PWG/Mopria press release, Microsoft docs |
| E15 | CUPS ippeveprinter and OpenPrinting ippsample are Apache-2.0; PAPPL is "based loosely on ippeveprinter.c". Rust crate ippper.rs is BSD-3-Clause (IPP server library) | GitHub licence metadata; PAPPL README |

## Findings

1. Platforms (E1, E3, E5). Windows is supported, but only through the Visual Studio solution and NuGet-packaged dependencies; the GNU autotools build targets POSIX. DNS-SD on Windows goes through the OS DnsService* API inside libcups 3, so no Bonjour install is needed on Windows 10+ (our reading of libcups source; not run). macOS uses mDNSResponder from the OS. Linux needs Avahi (D-Bus daemon at runtime) or mDNSResponder.
2. Dependencies (E2). The hard dependency is libcups 3 (a second C library, same author group), plus a TLS library and zlib. The README and INSTALL.md disagree on the minimum CUPS version (2.5 vs 2.2); master requires libcups 3 (E4), so treat 3.0 as the floor. libcups 3 itself is a separate build on every target.
3. Static linking (E7, E3, E5). A static libpappl is feasible on POSIX with a static libcups, OpenSSL/LibreSSL and zlib. Per target (analysis from the build files, not tested):
   - x86_64/aarch64-unknown-linux-musl: static link of PAPPL, libcups, TLS and zlib looks plausible; the blocker is Avahi, which needs libdbus and a running avahi-daemon, so a fully static binary would still need the host daemon, or mDNSResponder embedded (more porting). Cross-compiling C for musl adds a toolchain step to the release workflow.
   - aarch64/x86_64-apple-darwin: dns_sd comes from libSystem and cannot be linked statically; the rest can. Acceptable, since libSystem is always present.
   - x86_64-pc-windows-msvc: the project files build a DLL, so a static lib would need a custom vcxproj or CMake wrapper; dnsapi.lib is a system DLL. LibreSSL/libcups3 NuGet packages are native packages with .redist DLLs, so a single self-contained exe would require our own static builds.
   - Rust integration: there is no maintained safe Rust binding that we found; we would write bindgen/cc build glue, which touches Cargo.toml and CI (out of scope for this spike).
4. Licence (E8, E9). Apache-2.0 is one-way compatible with MIT OR Apache-2.0 for our use: we may combine and redistribute, but the combined binary carries PAPPL/libcups Apache-2.0 terms, so release archives need their LICENSE and NOTICE files and attribution. Our own source stays MIT OR Apache-2.0. The optional exception only loosens obligations. Avahi is LGPL-2.1: dynamic linking to a system libavahi is fine; static linking would trigger relinking obligations (same pattern as an LGPL ffmpeg in the full-build spike). Using the Apple mDNSResponder (Apache-2.0) avoids that.
5. Protocol coverage (E10-E14). IPP Everywhere: PAPPL's embedded service is the closest fit and was written to support it; formal PWG self-certification is a separate, optional step. AirPrint: the technical requirements (Bonjour, IPP, URF) are covered by PAPPL's Apple Raster and DNS-SD support, but the AirPrint brand needs an Apple licence; unlicensed use is "works with iOS/macOS clients", not "AirPrint certified". Mopria: certification is member-only; practical Windows and Android interop comes through IPP Everywhere devices with PWG Raster and the Microsoft IPP Class Driver. For anytopdf this means PDF-to-print-job reception (the output we want is a PDF) depends on PAPPL's rasterised formats plus ipptransform (E11), so every job arrives as raster or JPEG, not PDF, and OCR quality depends on the client's raster resolution.
6. Alternatives (E15):
   - CUPS ippeveprinter (Apache-2.0, in libcups tooling): a sample IPP Everywhere printer with a command-per-job hook; simplest to wrap as an out-of-process helper but not a library.
   - OpenPrinting ippsample (Apache-2.0): reference IPP server; heavier, aimed at testing.
   - A Rust IPP server (ippper.rs, BSD-3-Clause) plus a pure-Rust mDNS crate: keeps the toolchain single-language and statically linkable, but we would own IPP Everywhere conformance, URF/PWG Raster decoding and TLS; estimate several weeks to reach interoperability with macOS, iOS and Windows clients (estimate, basis: protocol surface listed in PWG 5100.14, not measured).
   - CUPS itself (cups-browsed/queue sharing) on the host: zero code for us, but requires CUPS installed and is not a self-contained tool.

## Risks and unknowns

- Not tested: no build of PAPPL or libcups was attempted on any target; static-link feasibility is inferred from build files.
- PAPPL v2/libcups 3 are on a beta master line while v1.4.12 is the stable release; API stability of v2 is unconfirmed.
- No Rust binding exists to our knowledge; the FFI and C-toolchain cost in CI and release (five targets) is unquantified.
- Security: PAPPL accepts untrusted network jobs; backlog says sandboxed conversion without network, size and page caps. Process isolation is easier with a separate helper than with in-process FFI.
- Brand and certification: AirPrint licence and Mopria membership cost and eligibility are unknown; need contact with Apple and Mopria.
- Windows Firewall and DNS-SD behaviour for unprivileged listeners was not verified.
- Avahi dependence on Linux conflicts with a static, no-daemon release goal.

## Recommendation

Recommendation: Do not link PAPPL into the Rust binary; for backlog item 3, run it only as an optional separate helper process (slim build excludes it), defer AirPrint and Mopria branding, and re-spike with a real build before committing.

Rationale: Windows is supported but through a DLL-oriented VS build, the hard dependency chain (libcups 3, TLS, Avahi daemon on Linux) fights the static single-binary release model, and the Apache-2.0 licence is compatible but adds notice duties. Out-of-process isolation also matches the plugin boundary in AGENTS.md and the untrusted-input sandbox requirement.

## Backlog impact

- Backlog item 3 (network printer on PAPPL): proceed only as an out-of-process helper (or ippeveprinter wrapper) with a hands-on build spike on all five targets first; claim "IPP Everywhere compatible" only after PWG self-certification tooling passes; not "AirPrint" or "Mopria" without licences. Expect jobs as PWG/Apple Raster or JPEG, which feeds the image/OCR path rather than PDF pass-through.
- Backlog item 4 (remote printing over Tailscale/WireGuard, DNS-SD unicast): PAPPL's DNS-SD goes through libcups and the OS APIs; unicast DNS-SD over a VPN is unverified. Treat as unproven; the Rust-native alternative may be easier here.
- Backlog items 1 and 2 are unaffected. No change to sprint 2 code, Cargo.toml or CI.

## Sources

- https://github.com/michaelrsweet/pappl
- https://github.com/michaelrsweet/pappl/blob/master/INSTALL.md
- https://github.com/michaelrsweet/pappl/blob/master/NOTICE
- https://github.com/michaelrsweet/pappl/blob/master/CHANGES.md
- https://github.com/michaelrsweet/pappl/blob/master/configure.ac
- https://github.com/michaelrsweet/pappl/tree/master/vcnet
- https://github.com/OpenPrinting/libcups
- https://github.com/OpenPrinting/libcups/blob/master/cups/dnssd.c
- https://github.com/OpenPrinting/ippsample
- https://github.com/ArcticLampyrid/ippper.rs
- https://github.com/avahi/avahi
- https://www.pwg.org/ipp/everywhere.html
- https://en.wikipedia.org/wiki/AirPrint
- https://developer.apple.com/forums/thread/12359
- https://app.mopria.org/mopria-certification
- https://learn.microsoft.com/en-us/windows-hardware/drivers/print/printer-driver-overview
