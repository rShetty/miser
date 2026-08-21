# Operations Runbook

## Service

```bash
systemctl status miser
journalctl -u miser -n 100 --no-pager
systemctl restart miser
curl -fsS http://127.0.0.1:8787/health/live
curl -fsS http://127.0.0.1:8787/health/ready
```

## API key store

Keys live in `/var/lib/miser/keys.json` as SHA-256 hashes. The hashing scheme changed from a legacy FNV-based digest to real SHA-256; **pre-existing `keys.json` entries are invalidated by this change** and must be regenerated:

1. Stop the gateway or pick a low-traffic window.
2. Back up `/var/lib/miser/keys.json` (mode 600, never to source control).
3. Remove the stale entries (`echo '{"keys":[]}' > /var/lib/miser/keys.json`).
4. Re-issue keys via `POST /admin/keys` and redistribute them to clients.

## Deployment

Push to `main` after CI passes. The GitHub Actions deployment builds on the VPS architecture, stages binaries/configuration, restarts systemd, and checks liveness.

Required GitHub secrets:

- `VPS_HOST`
- `VPS_USER`
- `VPS_SSH_KEY`
- `OPENROUTER_API_KEY`

## Failure diagnosis

### Service will not start

```bash
journalctl -u miser -n 50 --no-pager -o cat
/usr/local/bin/miser-gateway --config /etc/miser/miser.toml
```

Check TOML validity, required tier routes, environment-file permissions, and the binary architecture.

### Requests return 401

Verify the gateway bearer key expected by the client. Provider authentication is separate and is not the client gateway key.

### Requests return 502

Check OpenRouter availability, provider key validity, configured model slugs, provider allowlists, and upstream response errors. Do not print the provider key while debugging.

### Latency is high

Compare heuristic classification latency with local/cloud classifier latency. On a small CPU VPS, avoid a large generative local model in the synchronous path. Reduce classifier timeout, disable local LLM, or use a compact classifier service.

### Cost is high

Inspect selected tier/model headers and OpenRouter activity. Verify that heuristic confidence is not systematically low, route models are correctly priced, and provider `sort = price` is active. Add representative production prompts to the evaluation corpus before changing thresholds.

## Backups and rollback

Keep the last known-good binary, configuration, and commit reference. Roll back by installing the previous binary/config pair and restarting `miser`. Never back up environment files to source control or world-readable storage.

## Capacity

Monitor CPU, memory, open file descriptors, upstream latency, and active requests. Scale horizontally behind a TLS reverse proxy when one VPS is insufficient. Keep model selection and classifier policy identical across replicas.
