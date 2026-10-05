Test-only TLS identity for the remote front tests: `ca.pem` signs `server.pem`
(CN and SAN `localhost`, `127.0.0.1`, valid 100 years); `server.key` is its
private key. The CA key was discarded. Never use these outside tests.
