# ADR 0002: API keys are stored as salted SHA-256, not Argon2

Status: accepted by the orchestrator 2026-10-17, pending product owner security review.
Story: [[ALK-US-0041]]. Requirement: [[ALK-SP-0006]] R3 ("stored only as salted hashes").

## Context

The backlog task for the key store suggested Argon2. Argon2 and other slow hashes exist to
resist offline guessing of low-entropy secrets such as passwords. Alkahest API keys are not
chosen by people: they are `alk_<8-char id>_<52-char secret>`, with the secret taken from
the OS CSPRNG (about 260 bits).

## Decision

Store each key as SHA-256 over a per-key random salt and the secret, compare in constant
time, and look keys up by their public id. No verification cache is needed.

## Consequences

- Offline brute force of a leaked store is infeasible because of the secret's entropy, not
  because of hash cost, so a slow hash adds no protection.
- Verification stays cheap on every request, and a caller who knows a key id cannot make
  the server burn CPU by presenting wrong secrets.
- If people-chosen keys or passphrases are ever accepted, this decision must be revisited.
- The pinned `argon2` workspace dependency is unused by this path.

Builds on [[plan/alkahest-v1-plan]]; details in
`docs/development/changes/2026-10-17-api-key-store.md`.
