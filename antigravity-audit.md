# SpotMe Enterprise Readiness Audit

**Auditor:** Antigravity (Enterprise Systems & Security Division)
**Target:** `spotme` (Rust TUI Application)
**Date:** April 2026
**Result:** **FAILED (CRITICAL FINDINGS)**

---

## Executive Summary
While the SpotMe project exhibits a promising structure and resolves several basic requirements for a terminal user interface application, it fundamentally falls short of enterprise deployment standards. The application suffers from critical supply chain vulnerabilities, naive concurrency management, lacking thread-safety, and rudimentary error handling. Below is a comprehensive audit of areas requiring immediate remediation.

---

## 1. Supply Chain & Dependency Security (CRITICAL)

The application fails basic security pipelines. A standard `cargo audit` execution yields active vulnerabilities:

*   **RUSTSEC-2023-0071 (Marvin Attack - Medium/High):** The `rsa v0.9.10` crate is vulnerable to timing side-channels allowing potential key recovery. This is introduced transitively via `librespot`. Since no direct fix exists for this branch of `librespot`, the application must investigate isolating this dependency, forking `librespot`, or upgrading to an immune version stream.
*   **RUSTSEC-2024-0436 (paste crate unmaintained):** Transitive dependency through `image -> ravif -> rav1e`. The project depends on unmaintained software in its imaging pipeline.

**Auditor Notes:** An enterprise product cannot ship with known cryptographic CVEs, regardless of the perceived "local-only" context of the attack.

---

## 2. Concurrency & Async Architecture

The codebase violates fundamental asynchronous programming principles:

*   **Blocking I/O in Async Contexts:** The `app_log` function performs synchronous disk I/O (`std::fs::OpenOptions`, `writeln!`, `fs::rename`) and is invoked directly within Tokio's async worker threads (e.g., inside `tokio::spawn` loops and playback functions). This can stall the Tokio reactor thread pool, leading to application hangs.
    *   *Requirement:* Transition to a non-blocking logging framework like `tracing` or `log` combined with `tracing-appender`.
*   **Detached Tokio Tasks:** Background pollers and image fetching loops (e.g., `main.rs` lines 123, 182) are spawned using `tokio::spawn` but their `JoinHandle`s are never tracked. This results in resource leaks, inability to gracefully shut down the application, and zombie tasks if the main routine needs to exit or soft-reboot. 

---

## 3. Network & API Hygiene

*   **Aggressive Polling & Rate Limiting Risks:** The application uses a hardcoded `tokio::time::sleep(tokio::time::Duration::from_millis(1000))` loop to continuously hammer the Spotify API `/v1/me/player`. There is no exponential backoff or dynamic polling interval based on playback state or HTTP `429 Too Many Requests` headers. This is a severe API abuse vector.
*   **Missing CSRF Protection in OAuth:** The OAuth authorization flow (`api::endpoints::get_or_refresh_token`) omits the required `state` parameter. The local HTTP listener binds to port 8480 and accepts the first incoming connection containing a `code=` parameter. This exposes the user to Cross-Site Request Forgery (CSRF) via loopback if they navigate to a malicious local or external site executing local requests.

---

## 4. Error Handling & Data Integrity

The application avoids `unwrap()` which is a positive sign, but relies heavily on silent degradation which makes debugging impossible in a production setting:

*   **Silent Data Coercion:** Handlers frequently use `.unwrap_or()` or `.unwrap_or_default()` when parsing complex JSON (e.g., `progress_ms`, `duration_ms`, `volume_percent`). When the Spotify API deviates from assumptions, the state silently falls back to `0` or `false` rather than bubbling up a typed parsing error.
*   **`anyhow` as a Catch-All:** The use of `anyhow::Result` across library functions (`api/endpoints.rs`) prevents calling code from making deterministic, programmatic decisions about failure states (e.g., differentiating between a `NetworkError`, `RateLimitError`, and `ParseError`).
    *   *Requirement:* Implement a strictly typed error domain using the `thiserror` crate.
*   **Token Storage:** While the configuration enforces an `0o600` file permission mask for `.spotme_cache.json` and `.spotify_token_cache.json`, maintaining OAuth tokens as plain-text JSON on disk does not meet enterprise credential storage requirements. The data should be stored securely using OS-native secure enclaves (e.g., macOS Keychain or Linux Secret Service via the `keyring` crate).

---

## 5. Architectural Cohesion & State Management

*   **Monolithic Elements via Main:** `main.rs` currently spans over 700 lines, blending API polling, file synchronization, JSON manipulation, and event loop logic. State mutation handlers are directly embedded within the main `mpsc` receiver loop. This tight coupling makes the `app_state` practically untestable without spawning the entire event loop and mocking the GUI constraints.
*   **Data Consistency:** Operations like `app_cache.playlists.sort_by` manipulate the cache data struct directly before saving to disk. However, race conditions are possible if separate async tasks fire concurrent writes via `save_cache()`, which synchronously writes content without file-level transactional locking or atomic temporary file swapping.

## Conclusion
The `spotme` codebase demonstrates prototype-level maturity. Immediate architectural shifts are necessary to address cryptography vulnerabilities, synchronous blocking violations within Tokio, silent failure logic, and OAuth security oversights before the software can be considered stable or secure.
