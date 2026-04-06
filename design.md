# SpotMe: Spotify CLI TUI Design Document

## 1. Goal
To build a CLI Text User Interface (TUI) for Spotify in Rust using the official Web API. 
Phase 1: Authenticate the user, fetch user info, and display their playlists.

## 2. Library Suggestions

### TUI & Terminal Handling
* **`ratatui`**: The de facto standard library for building TUIs in Rust (the actively maintained successor to `tui-rs`).
* **`crossterm`**: A robust, cross-platform terminal library that integrates perfectly with `ratatui` for handling keyboard events, rendering, and terminal raw mode.

### Spotify Web API Client
**We have decided to use `rspotify`.**

* **`rspotify`**: A mature, community-driven Rust crate for the Spotify API.
  * Handles OAuth2 flows seamlessly, including token caching and refreshing.
  * *HTTPS Callback Challenge*: To comply with Spotify's 2026 policy of enforcing HTTPS for callbacks, we cannot use `localhost` directly. However, Spotify explicitly permits **http** if we use the exact IP `127.0.0.1`!
    * We will register `http://127.0.0.1:8480/callback` in the Spotify Dashboard.
    * We will NOT actually run an HTTPS server.
    * When the user authenticates, the browser will redirect to `https://localhost:8480...` which will show a "Site cannot be reached" error.
    * The user simply copies the URL from their address bar and pastes it into our CLI prompt. `rspotify` will parse the token from the pasted URL. This avoids the headache of generating local SSL certificates!

### Async Runtime & Utilities
* **`tokio`**: The standard async runtime for Rust, essential for our HTTP requests and async TUI event loops.
* **`serde` & `serde_json`**: For serializing and deserializing JSON payloads.
* **`dotenvy`**: For loading `CLIENT_ID`, `CLIENT_SECRET`, and `REDIRECT_URI` from the `.env` file I see in your workspace.
* **`anyhow`**: To handle errors cleanly without needing deep nested `Result` mapping early on.

## 3. Initial Implementation Steps (Phase 1)
1. **Setup**: Initialize the Rust binary (`cargo init`) and add dependencies. Set up the `.env` file credentials.
2. **Authentication**: Perform OAuth authorization so we can access `user-read-private` and `playlist-read-private` scopes.
3. **TUI Layout**: 
    * A top banner displaying the current User's Info (Display Name).
    * A main block containing a list (or table) rendering the User's Playlists.
4. **Data Binding**: Wire up the API responses to the UI state.

## 4. Next Steps
1. Add `https://localhost:8480/callback` to your Spotify Developer Dashboard App.
2. Initialize the Rust project and start coding Phase 1!
