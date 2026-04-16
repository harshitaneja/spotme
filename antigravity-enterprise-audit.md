# SpotMe Enterprise Readiness Audit
**Auditor:** Antigravity (Enterprise Systems & Security Division)
**Target:** `spotme` (Rust TUI Application)
**Date:** April 8, 2026
**Result:** **FAILED (CRITICAL REMEDIATION REQUIRED)**

## Executive Summary
While the SpotMe project has resolved basic functional requirements and addressed low-hanging fruit from previous audits (like simple runtime panics), it remains severely deficient when evaluated against enterprise-grade software engineering standards. The architecture betrays a dangerous "happy path" design philosophy: it lacks memory guardrails, parses TCP streams improperly, and stores sensitive credentials in plaintext log/cache files. Below is a comprehensive audit of areas requiring immediate remediation.

---

## 1. Denial of Service (DoS) & Memory Starvation (CRITICAL)
The application fundamentally assumes that external networks and APIs will return trusted, bounded data. This creates multiple Out-Of-Memory (OOM) attack vectors:

### 1.1 Unbounded Pagination Loops
In `endpoints.rs:534-566` (`fetqch_tracks`), the code iterates over Spotify's pagination (`json["next"]`) within an infinite `loop {}`. 
- **The Flaw:** A maliciously constructed playlist, a DNS interception, or a proxy attack serving an endless stream of `next` pages will cause the application to accumulate `Track` objects infinitely until the OS OOM-killer forcibly terminates the process.
- **Enterprise Standard:** Impose strict iteration limits (e.g. max limits on vector length) and enforce global allocation circuit breakers.

### 1.2 Unbounded Payload Deserialization
In `endpoints.rs:775-864` (`fetch_lyrics_api`), the fetch from `lrclib.net` reads the entire response body directly into heap memory via `res.text().await?` before parsing.
- **The Flaw:** If `lrclib.net` is compromised, unavailable, or hijacked, it could stream an artificially bloated payload (e.g., a 5GB dummy string). `reqwest` will faithfully download and allocate this into RAM, bypassing UI timeout guarantees and crashing the client.
- **Enterprise Standard:** Enforce strict byte-size payload limits using `.content_length()` validation (when available) and bounded stream processing wrappers (e.g., pulling a capped byte chunk stream).

### 1.3 Image "Pixel Flood" Bomb Vulnerability
In `main.rs:134` and `main.rs:257`, album art fetching blindly reads an arbitrary binary blob (`ares.bytes().await`) and passes it nakedly into `image::load_from_memory`.
- **The Flaw:** "Pixel flood" and decompression bomb (tarbomb) image payloads can easily allocate excessive gigabytes of RAM when a highly-compressed, seemingly small byte array is expanded into RAW bitmaps in memory.
- **Enterprise Standard:** Limit image binary fetches to safe caps (e.g., <2MB bytes) and impose deterministic memory constraints before initiating any complex decoder.

---

## 2. Insecure Network & Authentication Hygiene (HIGH)

### 2.1 Fragile TCP Fragment Parsing in OAuth Flow
In `endpoints.rs:204`, the local OAuth listener relies on a single raw socket buffer read:
```rust
let mut buf = [0; 4096];
let n = socket.read(&mut buf).await.unwrap_or(0);
let request = String::from_utf8_lossy(&buf[..n]);
```
- **The Flaw:** TCP is a continuous stream protocol. There is absolutely *zero guarantee* that an HTTP GET request header will arrive in a single packet. If the browser's HTTP headers are fragmented, `socket.read` will return a partial string lacking the `code=` parameter, silently failing authentication for users on strange network topologies.
- **Enterprise Standard:** Implement a robust state machine loop reading until `\r\n\r\n` is encountered, or simply mandate a lightweight HTTP server library parser (like `axum` or `hyper`) instead of rolling non-compliant HTTP logic.

### 2.2 Plaintext Credential Storage on Disk
The `.spotify_token_cache.json` containing the application's long-lived Refresh Token is written to disk in unencrypted plaintext (`endpoints.rs:51-62`).
- **The Flaw:** While POSIX `0o600` permissions mask casual observation, storing valid OAuth refresh tokens in unencrypted filesystem JSON is inadequate. Process-memory dumps, LFI attacks, malware scripts, or untrusted user space contexts can aggressively scrape and exfiltrate these files.
- **Enterprise Standard:** Utilize OS-native secure enclaves (e.g., macOS Keychain, Secret Service on Linux, Windows Credential Manager) via the `keyring` crate to encrypt stored secrets dynamically to the hardware identity context.

### 2.3 Hardcoded Port Contention / Single Points of Failure
The OAuth redirect listener blindly attempts to bind `127.0.0.1:8480`. If the port is in use, the application outright panics/fails natively.
- **Enterprise Standard:** Implement graceful bind fallbacks to dynamically negotiate an available Ephemeral Port and direct the authentication engine correspondingly.

---

## 3. Concurrency Thread Starvation (MEDIUM)

### 3.1 Synchronous Mutex Contention inside Tokio Workers
In `main.rs`, the application uses `init_logger()` to pass `String` messages from the active app across the standard library's `std::sync::mpsc::channel` to an isolated blocking OS thread.
- **The Flaw:** `app_log()` operates via standard library blocking mechanisms from directly inside the Tokio reactor thread pool scope (`tokio::spawn` loops). Because it takes a synchronous lock under the hood, under tight tracing loads (or UI key-slam cascades), Tokio workers will unnecessarily stall natively against the OS locking primitives.
- **Enterprise Standard:** Never mix `std::sync` primitives across asynchronous scopes. Switch strictly to `tokio::sync::mpsc` for non-blocking message passing, or adopt a dedicated async tracing crate (`tracing-appender`).

---

## 4. Silent State Degradation (LOW / MAINTAINABILITY)
The codebase heavily abuses `.unwrap_or()` and `.unwrap_or_default()` in deserializing potentially flawed data (e.g., `main.rs:212` `progress_ms`, `volume_percent`).
- **The Flaw:** When the Spotify API schema mutates, fails dynamically, or introduces a bug, SpotMe forces coerced values (like `0` or `false`) into the State Engine without surfacing an error. This creates untraceable logical state mismatches, wiping out metric telemetry and radically expanding MTTR (Mean Time to Resolution) when hunting complex bugs.
- **Enterprise Standard:** Employ absolute strict deserialization. If a core field fails, it must explicitly break parsing and bubble up a strict contextual `Result<Err>` via `thiserror` highlighting the schema fault.
