# SpotMe Security & Code Audit

**Date**: 2026-04-08

## High

| # | Issue | Location |
|---|-------|----------|
| 1 | **Token cache file permissions inconsistent** — `0o600` set in `endpoints.rs` but not enforced everywhere cache is written | `endpoints.rs`, `config.rs` |
| 2 | **Sensitive data in logs** — full API URLs, device IDs, playlist IDs, and payloads logged in plaintext | `endpoints.rs` (log calls throughout) |
| 3 | **No log rotation** — `spotme.log` grows unbounded | `main.rs:55-65` |
| 4 | **Unbounded search input** — no length limit on query string in TUI search | `events.rs:573-605` |

## Medium

| # | Issue | Location |
|---|-------|----------|
| 5 | **No URI format validation** — Spotify track URIs accepted without format checks before API use | `models.rs:40-97` |
| 6 | **Third-party lyrics API (lrclib.net)** — no fallback or timeout isolation; failure blocks UI | `endpoints.rs` |
| 7 | **No CI/CD pipeline** — no automated tests, linting, or `cargo audit` on push | (missing) |

## Low

| # | Issue | Location |
|---|-------|----------|
| 8 | **Error messages expose API internals** — raw API response details surfaced to users | various |
| 9 | **LRC timestamp parsing** — no validation of timestamp ranges in synced lyrics | `endpoints.rs` |

## Positive Findings

- PKCE OAuth2 properly implemented (SHA-256 challenge, random verifier)
- All external API calls over HTTPS with Bearer token auth
- `.env` and token cache correctly `.gitignore`d
- Default client ID shipped for frictionless setup (standard practice for open-source Spotify tools)
- Well-structured modular codebase with async/await patterns
- 28 dependencies, all actively maintained — no abandoned or EOL packages
- 8 unit tests with mockito HTTP mocking

## Recommendations

1. **Enforce `0o600` permissions** on all cache/token file writes consistently
2. **Scrub logs** — redact tokens, device IDs, and full URLs; add log rotation or size cap
3. **Add `cargo audit`** to a CI pipeline (even a basic GitHub Actions workflow)
4. **Cap search input length** to a reasonable limit (e.g., 200 chars)
5. **Validate Spotify URIs** against `spotify:track:<base62>` format before use
