# SpotMe Security & Code Audit

**Date**: 2026-04-08  
**Scope**: Full codebase review (~3,650 LOC Rust), dependency audit, CI/CD review  
**Tools**: Manual code review, `cargo clippy`, `cargo test`, `cargo audit`

---

## Summary

| Severity | Count |
|----------|-------|
| Critical | 1     |
| High     | 4     |
| Medium   | 5     |
| Low      | 4     |

**Tests**: 9/9 passing  
**Clippy**: Clean (0 warnings)  
**Dependency vulns**: 1 vulnerability, 2 warnings

---

## Critical

### C1. Access token stored in plaintext in `AppState` and cloned freely

**Location**: `state.rs:93`, `main.rs:110`, `events.rs` (throughout)

The Spotify access token is stored as a plain `String` in `AppState` and cloned into dozens of fire-and-forget `tokio::spawn` closures. Any panic or core dump would expose the token. The token is also passed by value into long-lived background pollers (`main.rs:110`) that never refresh it — after ~1 hour the token expires and the poller silently fails.

**Recommendation**: Wrap the token in `Arc<RwLock<String>>` so it can be refreshed in-place, and avoid scattering copies across spawned tasks.

---

## High

### H1. Background poller never refreshes expired token

**Location**: `main.rs:110-179`

The player-state polling loop clones `access_token` once at startup and uses it forever. After the token's 1-hour TTL expires, every poll silently returns 401 and the player state goes stale with no user-visible indication.

**Recommendation**: Share token via `Arc<RwLock<String>>`, refresh on 401, or restart the poller with a fresh token.

### H2. Spawned tasks silently swallow all errors

**Location**: `events.rs:58-59`, `events.rs:67-69`, `events.rs:107-109`, and ~15 more `tokio::spawn` sites

All spawned playback commands (`play_track`, `pause_playback`, `seek_playback`, `next_track`, etc.) discard errors with `let _ =`. If a network call fails — e.g., token expired, rate-limited, or device gone — the user sees no feedback. The UI optimistically updates state (e.g., `player.is_playing = !is_playing` at `events.rs:94`) even when the API call fails.

**Recommendation**: Route errors back through the `AppMessage` channel and display transient status messages in the TUI.

### H3. Log file written without restricted permissions

**Location**: `main.rs:81-88`

`app_log()` opens the log file with `OpenOptions::new().create(true).append(true)` but sets no file permissions. Logs contain API paths, playlist IDs, payload sizes, and status codes. On multi-user systems the default umask may leave logs world-readable.

**Recommendation**: Set `mode(0o600)` on the log file open (matching what's already done for cache/token files).

### H4. Librespot error handler overwrites entire log file

**Location**: `main.rs:570-573`

```rust
let _ = std::fs::write(&config::paths().log_file, format!("Librespot error: {}\n", e));
```

Uses `fs::write` which **truncates** the file, destroying all prior log entries. Also lacks `0o600` permissions.

**Recommendation**: Use `app_log()` instead of `fs::write` for consistency and to preserve log history.

---

## Medium

### M1. `set_env_var` in tests is unsound

**Location**: `endpoints.rs:811`

```rust
std::env::set_var("SPOTIFY_API_BASE_URL", server.url());
```

`std::env::set_var` is unsound in multi-threaded programs (UB per Rust docs since 1.81). Since tests run in parallel under tokio, this can cause data races.

**Recommendation**: Use `unsafe { std::env::set_var(...) }` to acknowledge the risk (required since Rust 1.83), or refactor to pass base URL as a parameter rather than using env vars in tests.

### M2. Dependency vulnerability: `rsa` crate (RUSTSEC-2023-0071)

**Severity**: 5.9 (medium) — Marvin Attack timing side-channel  
**Source**: `librespot-core 0.8.0 -> rsa 0.9.10`  
**Status**: No fixed upstream version available

This is a transitive dependency from librespot. The risk is limited since SpotMe doesn't perform RSA operations directly, but librespot uses it for Spotify authentication.

**Recommendation**: Monitor for librespot updates. Consider `[advisories.ignore]` in `audit.toml` with a comment explaining the accepted risk.

### M3. Dependency warning: `paste` crate unmaintained (RUSTSEC-2024-0436)

**Source**: `image -> rav1e -> paste 1.0.15`

Transitive dependency through the image processing chain. No security impact currently but signals maintenance risk.

### M4. Dependency warning: `fastrand 2.4.0` yanked

**Source**: `tempfile -> fastrand 2.4.0` (via librespot chain)

The installed version has been yanked from crates.io. `cargo update` should resolve this.

**Recommendation**: Run `cargo update` to pick up the replacement version.

### M5. No input sanitization on `playlist_id` / `album_id` in URL construction

**Location**: `endpoints.rs:428-429`, `endpoints.rs:543-546`, `endpoints.rs:611`

```rust
let url = format!("{}/v1/playlists/{}/items", crate::api::api_base_url(), playlist_id);
```

While these IDs originate from Spotify's API (not direct user input), a malicious or corrupted cache could inject path traversal or query parameters into the URL. The `track_uri` field has validation (`models.rs:55-61`) but playlist/album IDs do not.

**Recommendation**: Validate that IDs are alphanumeric before interpolation, consistent with the track URI validation already in `models.rs`.

---

## Low

### L1. Unbounded `app_cache.playlists` growth from featured playlists

**Location**: `main.rs:454`

```rust
app_state.app_cache.playlists.extend(lists.clone());
```

Each press of `b` (featured playlists) appends up to 50 playlists to the cache without deduplication. Over time this bloats the cache file and slows serialization.

**Recommendation**: Deduplicate by playlist ID before extending, or cap cache size.

### L2. Log rotation creates only one `.log.old` backup

**Location**: `main.rs:76-79`

When the log exceeds 1MB, it's renamed to `.log.old`, silently overwriting any previous backup. This provides minimal history.

**Recommendation**: Acceptable for a TUI app, but consider numbered rotation (`.log.1`, `.log.2`) if debugging requires historical logs.

### L3. CI runs only on `workflow_dispatch` — no automatic triggers

**Location**: `.github/workflows/ci.yml:4`

CI doesn't run on `push` or `pull_request` events. This means PRs can be merged without passing checks.

**Recommendation**: Add `on: [push, pull_request]` triggers.

### L4. Error messages in UI expose raw API details

**Location**: `endpoints.rs:529`, `endpoints.rs:604`

```rust
return Err(format!("Bad payload: no items array. {}", json));
```

Full JSON payloads are included in error messages that may be displayed in the TUI. While this is useful for debugging, it could expose unexpected data.

**Recommendation**: Log the full response but show a user-friendly message in the TUI.

---

## Positive Findings

- **PKCE OAuth2 correctly implemented** — 32-byte random verifier, SHA-256 challenge, no client secret required (`endpoints.rs:72-87`)
- **All Spotify API calls over HTTPS** with Bearer token auth
- **Track URI validation** is thorough — checks prefix, length, and character set (`models.rs:55-61`)
- **Token and cache files use `0o600` permissions** on Unix (`main.rs:46-57`, `endpoints.rs:52-59`)
- **`.env` and sensitive files correctly `.gitignore`d**
- **Shared HTTP client** via `OnceLock` with 30s timeout (`api/mod.rs:6-16`)
- **Lyrics API has dedicated 5s timeout** to avoid blocking the UI (`endpoints.rs:708`)
- **Search input capped at 200 characters** (`events.rs:406`, `events.rs:616`)
- **Debounce logic** prevents UI state from being overwritten by stale API responses (`state.rs:112-145`)
- **Clean clippy** — zero warnings
- **9 unit tests passing** with mockito HTTP mocking
- **Well-structured async architecture** with channel-based message passing
- **Log size capped at 1MB** with basic rotation (`main.rs:76-79`)

---

## Resolved Since Last Audit

| Prior Issue | Status |
|---|---|
| Token cache file permissions inconsistent | **Resolved** — `0o600` now enforced in both `main.rs:save_cache()` and `endpoints.rs` token writes |
| Unbounded search input | **Resolved** — 200-char cap added in `events.rs:406` and `events.rs:616` |
| No URI format validation | **Resolved** — `models.rs:55-61` validates `spotify:track:` prefix, length, and alphanumeric ID |
| No CI/CD pipeline | **Partially resolved** — CI exists with fmt/clippy/test/audit, but only triggers on `workflow_dispatch` |
| Third-party lyrics API timeout | **Resolved** — 5s timeout added at `endpoints.rs:708` |
