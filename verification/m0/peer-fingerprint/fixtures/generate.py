#!/usr/bin/env python3
"""Regenerate public, test-only DTLS certificate fixtures."""

import base64
import hashlib
import json
from datetime import datetime, timezone
from pathlib import Path

from cryptography import x509
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import ec
from cryptography.x509.oid import NameOID

ROOT = Path(__file__).parent


def make_fixture(name, scalar, serial, not_before, not_after):
    key = ec.derive_private_key(scalar, ec.SECP256R1())
    subject = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, f"kvmctl {name} test-only")])
    before = datetime.fromisoformat(not_before).replace(tzinfo=timezone.utc)
    after = datetime.fromisoformat(not_after).replace(tzinfo=timezone.utc)
    cert = (
        x509.CertificateBuilder()
        .subject_name(subject)
        .issuer_name(subject)
        .public_key(key.public_key())
        .serial_number(serial)
        .not_valid_before(before)
        .not_valid_after(after)
        .sign(key, hashes.SHA256())
    )
    cert_der = cert.public_bytes(serialization.Encoding.DER)
    key_pem = key.private_bytes(
        serialization.Encoding.PEM,
        serialization.PrivateFormat.PKCS8,
        serialization.NoEncryption(),
    ).decode().replace("PRIVATE KEY", "PRIVATE_KEY").strip()
    cert_pem = cert.public_bytes(serialization.Encoding.PEM).decode().strip()
    expiry = int(after.timestamp()).to_bytes(8, "little")
    expiry_pem = "-----BEGIN EXPIRES-----\n" + base64.b64encode(expiry).decode() + "\n-----END EXPIRES-----"
    (ROOT / f"{name}.rtc.pem").write_text(
        f"{expiry_pem}\n\n{key_pem}\n\n{cert_pem}\n", encoding="ascii", newline="\n"
    )
    return {
        "certificate_der_sha256": hashlib.sha256(cert_der).hexdigest(),
        "public_key_der_sha256": hashlib.sha256(
            key.public_key().public_bytes(
                serialization.Encoding.DER,
                serialization.PublicFormat.SubjectPublicKeyInfo,
            )
        ).hexdigest(),
    }


manifest = {
    "warning": "PUBLIC TEST DATA ONLY; NEVER DEPLOY THESE CERTIFICATES OR KEYS",
    "fixtures": {
        "certificate_a": make_fixture("certificate_a", 1, 1001, "2025-01-01", "2035-01-01"),
        "certificate_b": make_fixture("certificate_b", 2, 2001, "2025-01-01", "2035-01-01"),
        "certificate_a_renewed": make_fixture(
            "certificate_a_renewed", 1, 1002, "2026-01-01", "2036-01-01"
        ),
    },
}
(ROOT / "manifest.json").write_text(
    json.dumps(manifest, indent=2) + "\n", encoding="ascii", newline="\n"
)
