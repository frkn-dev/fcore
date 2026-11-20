#!/usr/bin/env python3
"""Live check of the peers flow against a running api (AGW envelope client).

Runs on the api host: reads the private key the api itself uses, wraps a
random AES key with it, POSTs an encrypted body and decrypts the answer.
Nothing secret is printed by default except the throwaway client key pair,
which the caller may keep to try a real handshake.

usage: peers-live-check.py <card_id|""> <subscription_id> <device_uid>
"""
import base64
import ipaddress
import json
import os
import sys
import urllib.request

from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import padding, x25519
from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes

API = "http://127.0.0.1:3000"
KEY_PATH = "/opt/api/agw_private.pem"


def post(path: str, payload: dict) -> dict:
    with open(KEY_PATH, "rb") as fh:
        priv = serialization.load_pem_private_key(fh.read(), password=None)
    pub = priv.public_key()

    aes_key = os.urandom(32)
    aes_iv = os.urandom(16)
    body = json.dumps(payload).encode()

    enc = Cipher(algorithms.AES(aes_key), modes.CBC(aes_iv)).encryptor()
    pad = 16 - len(body) % 16
    cipher = enc.update(body + bytes([pad]) * pad) + enc.finalize()

    key_json = json.dumps(
        {
            "aes_key": base64.b64encode(aes_key).decode(),
            "aes_iv": base64.b64encode(aes_iv).decode(),
        }
    ).encode()
    wrapped = pub.encrypt(key_json, padding.PKCS1v15())

    envelope = {
        "keyPayload": base64.b64encode(wrapped).decode(),
        "apiPayload": base64.b64encode(cipher).decode(),
    }

    req = urllib.request.Request(
        API + path,
        data=json.dumps(envelope).encode(),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=20) as resp:
        status = resp.status
        raw = resp.read()

    if raw[:1] == b"{":
        # Plain refusal (peers disabled / crypto off): already JSON.
        return {"status": status, "body": json.loads(raw)}

    dec = Cipher(algorithms.AES(aes_key), modes.CBC(aes_iv)).decryptor()
    plain = dec.update(raw) + dec.finalize()
    plain = plain[: -plain[-1]]
    return {"status": status, "body": json.loads(plain)}


def client_keypair():
    """A throwaway WG key pair; the api only needs the public half."""
    priv = x25519.X25519PrivateKey.generate()
    priv_raw = priv.private_bytes(
        encoding=serialization.Encoding.Raw,
        format=serialization.PrivateFormat.Raw,
        encryption_algorithm=serialization.NoEncryption(),
    )
    pub_raw = priv.public_key().public_bytes(
        encoding=serialization.Encoding.Raw,
        format=serialization.PublicFormat.Raw,
    )
    return base64.b64encode(priv_raw).decode(), base64.b64encode(pub_raw).decode()


def main():
    card_id = sys.argv[1] if sys.argv[1] else None
    subscription_id = sys.argv[2]
    device_uid = sys.argv[3]

    priv_b64, pub_b64 = client_keypair()

    payload = {
        "subscription_id": subscription_id,
        "device_uid": device_uid,
        "device_name": "live-check",
        "public_key": pub_b64,
    }
    if card_id:
        payload["card_id"] = card_id
    else:
        payload["env"] = "0001"
        payload["proto"] = "amnezia_peer"

    created = post("/v1/peers", payload)
    print(json.dumps(created, ensure_ascii=False, indent=2))

    peer = (created.get("body") or {}).get("peer") or {}
    addr = (peer.get("address") or "").split("/")[0]
    if addr:
        print("CLIENT_PRIVATE_KEY=" + priv_b64)
        print("CLIENT_PUBLIC_KEY=" + pub_b64)
        print("PEER_ADDRESS=" + addr)
        print("SHARD_GATEWAY=10.88.0.1")

    listed = post(
        "/v1/peers/list",
        {"subscription_id": subscription_id, "device_uid": device_uid},
    )
    print("LIST=" + json.dumps(listed, ensure_ascii=False))


if __name__ == "__main__":
    main()
