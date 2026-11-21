# rustls-pki-types patch

This is `rustls-pki-types` 1.16.0 from crates.io, copied so Litz can run TLS
in the browser.

Upstream hides `UnixTime::now` on `wasm32-unknown-unknown` unless the `web`
feature is on, and that feature reads the clock through `web-time`, which
links JavaScript imports. The simulator is instantiated with an empty import
object (`WebAssembly.instantiate(bytes, {})`), so those imports are not
available. rustls only calls `now` to fill its default `TimeProvider`; Litz
installs a fixed `TimeProvider` on both ends, so the value is never read.

The patch adds a third `cfg` branch to `UnixTime::now` for
`wasm32-unknown-unknown` without `web`, returning the Unix epoch. Nothing else
changes. Delete this directory and the `[patch.crates-io]` entry once upstream
offers a wasm clock that does not need a JavaScript host, or once rustls stops
compiling `UnixTime::now` into builds that supply their own time provider.
