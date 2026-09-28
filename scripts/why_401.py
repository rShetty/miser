#!/usr/bin/env python3
"""Explain a `401 invalid API key` from the gateway, rather than guessing.

The gateway's 401 is deliberately undifferentiated: a missing key, an unknown
key, a revoked key and a key read from the *wrong store* all produce the same
`{"error":{"message":"invalid API key"}}`. That makes it indistinguishable from
a bad secret, which is why it is easy to spend an hour blaming the key.

This reproduces the decision locally instead. It never prints a secret, and it
does not talk to the network.

    python3 scripts/why_401.py
"""

import hashlib
import json
import os
import subprocess
import sys

GATEWAY = "http://127.0.0.1:8787"


def sha(text: str) -> str:
    return hashlib.sha256(text.encode()).hexdigest()


def load_store(path: str):
    """Return `(keys, None)` on success or `(None, error)` on failure."""
    try:
        with open(os.path.expanduser(path)) as handle:
            return json.load(handle).get("keys", []), None
    except Exception as error:  # noqa: BLE001 - diagnostic tool
        return None, error


def classify(candidate: str | None, keys: list) -> tuple[str, str]:
    """Reproduce the gateway's `validate` against a candidate secret."""
    if candidate is None:
        return "absent", "no key to test"

    hashes = {key.get("key_hash"): key for key in keys}
    # The gateway strips a `miser_` prefix before hashing, so both forms are
    # tried -- sending the wrong one is an easy mistake and looks identical.
    for label, secret in (
        ("as-is", candidate),
        ("miser_ prefix stripped", candidate[len("miser_"):] if candidate.startswith("miser_") else "miser_" + candidate),
    ):
        match = hashes.get(sha(secret))
        if match is None:
            continue
        if not match.get("active", True):
            return "revoked", f"{match['id']} matches but active=false -> 403"
        expires = match.get("expires_at")
        if expires:
            import time

            if expires < int(time.time()):
                return "expired", f"{match['id']} expired at {expires} -> 403"
        return "valid", f"{label} matches {match['id']} (owner={match.get('owner')!r})"
    return "unknown", "matches no key_hash in the store"


def read_env_value(name: str) -> tuple[str | None, str]:
    """Where is `name` defined, if anywhere? Never prints the value."""
    found = []
    for candidate in ("~/.env", ".env"):
        path = os.path.expanduser(candidate)
        if not os.path.isfile(path):
            continue
        for line in open(path):
            if line.strip().startswith(f"{name}="):
                found.append((candidate, line.split("=", 1)[1].strip()))
    if not found:
        return None, ""
    return found[0][1], ", ".join(where for where, _ in found)


def main() -> int:
    store_path = os.environ.get("MISER_KEYS_FILE")
    if not store_path:
        for candidate in ("~/.env", ".env"):
            path = os.path.expanduser(candidate)
            if os.path.isfile(path):
                for line in open(path):
                    if line.strip().startswith("MISER_KEYS_FILE="):
                        store_path = line.split("=", 1)[1].strip()
                        break
            if store_path:
                break

    print("1. which store would the gateway read?")
    if not store_path:
        print("   MISER_KEYS_FILE is unset -> the gateway falls back to")
        print("   /etc/miser/keys.json. If that is empty or absent, EVERY key")
        print("   is rejected with exactly this 401, and it looks like a bad key.")
        print("   THIS IS THE MOST COMMON CAUSE.")
        return 1
    print(f"   {os.path.expanduser(store_path)}")

    keys, error = load_store(store_path)
    if keys is None:
        print(f"   cannot read it: {error}")
        return 1
    print(f"   {len(keys)} key(s)")
    for key in keys:
        print(
            f"     {key.get('id')}  owner={key.get('owner')!r} client={key.get('client')!r} "
            f"active={key.get('active')} expires_at={key.get('expires_at')}"
        )

    print()
    print("2. does the client's key authenticate against THAT store?")
    candidate, where = read_env_value("MISER_API_KEY")
    if candidate is None:
        print("   MISER_API_KEY is not defined in ~/.env or .env.")
        print("   The client then sends an empty or literal value, which the")
        print("   gateway rejects with the same 401. Check which .env the")
        print("   client actually loads -- there are two.")
    else:
        verdict, detail = classify(candidate, keys)
        print(f"   defined in: {where}")
        print(f"   verdict:    {verdict} -- {detail}")

    print()
    print("3. is the gateway even up, and which process is it?")
    try:
        out = subprocess.run(
            ["pgrep", "-af", "miser-gateway"], capture_output=True, text=True, timeout=10
        ).stdout.strip()
        print("   " + (out.replace("\n", "\n   ") if out else "not running"))
        if out:
            binary = out.split()[1]
            mtime = subprocess.run(
                ["stat", "-c", "%y", binary], capture_output=True, text=True, timeout=10
            ).stdout.strip()
            print(f"   binary: {binary}")
            print(f"   built:  {mtime}")
            print("   (a binary older than your source is running the old code)")
    except Exception as error:  # noqa: BLE001
        print(f"   could not check: {error}")

    print()
    print("4. live check with the key the gateway will actually see")
    try:
        out = subprocess.run(
            [
                "curl", "-sS", "-o", "/dev/null", "-w", "%{http_code}",
                "-X", "POST", f"{GATEWAY}/v1/chat/completions",
                "-H", f"Authorization: Bearer {candidate}",
                "-H", "Content-Type: application/json",
                "-d", '{"model":"auto","messages":[{"role":"user","content":"hi"}]}',
            ],
            capture_output=True, text=True, timeout=60,
        ).stdout.strip()
        print(f"   HTTP {out}  (200 = the key is fine and the fault is elsewhere)")
    except Exception as error:  # noqa: BLE001
        print(f"   could not reach {GATEWAY}: {error}")

    return 0


if __name__ == "__main__":
    sys.exit(main())
