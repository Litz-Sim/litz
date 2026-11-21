# Simulated TLS certificates

These are the router's TLS identity, not a secret. The private key is in the
repository because every copy of the simulator must answer the same handshake;
it protects nothing.

- `ca.pem` / `ca.der` — the simulated CA, `CN=Litz Simulated CA`, valid
  2000-01-01 through 2100-01-01.
- `server.pem` / `server.der` — `api.litz.dev`, signed by the CA, with
  `litz.local`, `localhost` and `10.0.2.3` as subject alternative names.
- `server.key.der` — the server's Ed25519 key, PKCS#8.

A firmware trusts the server by loading `ca.pem` into its trust store. The
validity window covers both a guest whose clock starts at zero and one that has
synced over SNTP.

Regenerate with OpenSSL 3.6 or later (it can backdate the validity):

```sh
openssl genpkey -algorithm ed25519 -out ca.key
openssl req -x509 -new -key ca.key -out ca.pem -subj "/CN=Litz Simulated CA/O=Litz" \
  -not_before 20000101000000Z -not_after 21000101000000Z \
  -addext "basicConstraints=critical,CA:TRUE" \
  -addext "keyUsage=critical,keyCertSign,cRLSign"
openssl genpkey -algorithm ed25519 -out server.key
openssl req -new -key server.key -out server.csr -subj "/CN=api.litz.dev"
printf "subjectAltName=DNS:api.litz.dev,DNS:litz.local,DNS:localhost,IP:10.0.2.3\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=serverAuth\n" > server.ext
openssl x509 -req -in server.csr -CA ca.pem -CAkey ca.key -CAcreateserial -out server.pem \
  -not_before 20000101000000Z -not_after 21000101000000Z -extfile server.ext
openssl x509 -in ca.pem -outform DER -out ca.der
openssl x509 -in server.pem -outform DER -out server.der
openssl pkcs8 -topk8 -nocrypt -in server.key -outform DER -out server.key.der
```
