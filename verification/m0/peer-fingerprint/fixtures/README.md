# Synthetic DTLS certificate fixtures

Everything in this directory is public test data. The private keys are
deliberately known examples and must never be deployed or reused for real
devices or clients.

- `certificate_a.rtc.pem`: test key A and certificate A.
- `certificate_b.rtc.pem`: independent test key B and certificate B.
- `certificate_a_renewed.rtc.pem`: key A reused in a renewed certificate with
  a different serial, validity period, and DER encoding.
- `manifest.json`: SHA-256 hashes of the DER certificates and public keys.

The combined PEM format starts with the WebRTC-RS `EXPIRES` block and then
contains the X.509 certificate and PKCS#8 private key. The accompanying
`generate.py` recreates all files with fixed private scalars, certificate
serials, and validity dates. ECDSA signatures may differ between generations;
use the committed manifest and fixture bytes for repeatable tests.
