# Security Model

## Threats

- API-key theft from repository, logs, process arguments, or backups.
- Unauthorized use of the gateway and inference spend.
- Prompt or tool-content exfiltration through logs.
- SSRF through user-controlled upstream URLs.
- Malicious tool schemas or oversized requests causing resource exhaustion.
- Dependency vulnerabilities and compromised build artifacts.
- Deployment-key compromise through CI.

## Controls

- Secrets are supplied through GitHub Secrets and `/etc/miser/miser.env`; `.env` is ignored.
- The gateway runs as the non-root `miser` user with `NoNewPrivileges`, `PrivateTmp`, `ProtectSystem=strict`, and `ProtectHome`.
- Upstream URL is configuration-only; clients cannot choose arbitrary destinations.
- Response headers are allowlisted.
- Prompt bodies, authorization values, cookies, and tool arguments must not be logged.
- CI runs Cargo audit, cargo-deny, Gitleaks, and Trivy.
- CI uses least-privilege `contents: read` and deploys only after verification/security jobs pass.
- SSH host keys are recorded in the workflow before connecting.
- Service environment files are mode 600.

## Required production controls

- Put TLS and authentication at a trusted reverse proxy or configure TLS serving.
- Configure a gateway bearer key distinct from the OpenRouter provider key.
- Restrict VPS firewall ingress to the reverse proxy or trusted client networks.
- Rotate OpenRouter, SSH, and GitHub Actions secrets regularly.
- Use a GitHub production environment with required reviewers.
- Add rate limits, body limits, quotas, and circuit breakers before exposing the gateway broadly.
- Enable dependency update automation and review security alerts.

## Incident response

1. Revoke exposed provider or SSH credentials immediately.
2. Disable the deployment environment if CI is suspected.
3. Stop the gateway or firewall the endpoint if spend abuse is active.
4. Inspect sanitized service logs and OpenRouter activity.
5. Rotate credentials and redeploy from a known-good commit.
6. Record timeline, affected requests, cost, and corrective controls.

No secrets should be pasted into issues, benchmark output, commit messages, or chat logs.
