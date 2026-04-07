# SpotMe Security & Code Audit

**Date**: 2026-04-08
**Status**: Post-Remediation Verification

## Remediation Success Verification

All of the critical and standard issues identified in the previous code audit have been **successfully resolved**:

- **Token cache file permissions**: `0o600` restricted file permissions are strictly enforced for both `spotme_cache.json` (`main.rs:52`) and `spotify_token_cache.json` (`endpoints.rs:55, 183`) upon creation.
- **Sensitive data in logs**: Log redaction is successfully implemented. Payload operations, searching, and network APIs only log metadata (e.g. `[REDACTED]`) without leaking specific identifiers or authentication tokens.
- **Log rotation**: File size-based log rotation (~1 MB threshold) is correctly implemented within `main.rs:77`, preventing `spotme.log` from growing unbounded.
- **Search input bounds**: UI-based search inputs strictly max out logic lengths to `< 200` characters (`events.rs:406`), isolating the system from DoS/memory panic queries.
- **URI format validation**: `Track::parse_track` successfully tests string validity against `spotify:track:` base prefixes prior to allocating network calls (`models.rs:55`).
- **Third-party lyrics API**: A rigorous 5-second `.timeout()` acts as a solid isolation layer, guaranteeing the application UI main thread never blocks permanently on an external LRC fetch (`endpoints.rs:705`).
- **LRC timestamp parsing**: Synchronous timecode allocations strictly fall back if bounds are illegal (`secs < 60 && mins < 600`), stopping logic bombs (`endpoints.rs:753`).
- **CI/CD Pipeline**: GitHub Actions established (`ci.yml`), integrating `clippy`, `fmt`, `tests`, and automated `rustsec/audit-check`.

---

## New Audit Findings (Dependency Layer)

The source core code tests perfectly, compiling free of `cargo clippy` warnings and safely passing logic validation. However, a fresh diagnostic of `cargo audit` highlights upstream dependencies requiring attention:

### High

| # | Issue | Location | Resolution Path |
|---|-------|----------|-----------------|
| 1 | **Marvin Attack (RUSTSEC-2023-0071)** — Potential key recovery through timing sidechannels in the `rsa` crate. | `rsa` v0.9.10 (via `librespot` dependencies) | **No fixed upgrade is currently available.** This will cause the newly implemented GitHub Actions CI `security_audit` job to **FAIL** automatically. |

### Warnings

| # | Issue | Location | Resolution Path |
|---|-------|----------|-----------------|
| 2 | **Unmaintained Crate (RUSTSEC-2024-0436)** — `paste` crate is no longer maintained. | `paste` v1.0.15 (via `ratatui-image` chain) | Non-critical workflow block unless `ratatui-image` upgrades it manually upstream. |
| 3 | **Yanked Crate** — `fastrand` v2.4.0 was yanked. | `fastrand` v2.4.0 (via `tempfile`) | Non-critical workflow block. Awaiting patch in `librespot`. |

## Actionable Recommendations
1. **GitHub Actions / CI Remediation**: Since the `RUSTSEC-2023-0071` vulnerability does not have an available upgrade and is deeply embedded in the `librespot` upstream tree, the CI checks will chronically fail. Consider temporarily ignoring `RUSTSEC-2023-0071` via the RustSec config file (`audit.toml`) or GitHub actions parameter so CI builds pass while retaining awareness.
