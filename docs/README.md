# Miser Documentation

- [High-Level Design](./HLD.md)
- [Low-Level Design](./LLD.md)
- [Security Model](./SECURITY.md)
- [Operations Runbook](./OPERATIONS.md)
- [Evaluation Methodology](./EVALUATION.md)

Miser is an OpenAI-compatible gateway. The documentation describes the current Rust MVP and marks planned capabilities explicitly. The default classifier is **Jev** (TypeSafe System One evaluation model) — see [Evaluation Methodology](./EVALUATION.md) for the benchmark evidence and [Operations Runbook](./OPERATIONS.md) for key rotation and fallback detection. The `miser-setup` skill (`.omp/skills/miser-setup/`) installs and configures the whole stack on a coding-agent harness.
