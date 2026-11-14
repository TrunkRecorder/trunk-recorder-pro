# Test fixtures

Keys and certificates made for the tests, and used by nothing else. Their
private keys are public (they're in this repository), so never deploy them,
trust them, or point `server.tls` at them.

- `tls/a.crt`, `tls/a.key`, `tls/b.crt`, `tls/b.key`: self-signed certificates
  for `localhost` and `127.0.0.1`, and their keys (`tls.rs`, `server.rs` tests).
- `cloudflare/a.pk8`, `cloudflare/b.pk8`: RSA keys that stand in for a
  Cloudflare Access team's, to sign test tokens (`access/cloudflare.rs` tests).

They're read from disk by `#[cfg(test)]` code only; the binary holds none of them.
