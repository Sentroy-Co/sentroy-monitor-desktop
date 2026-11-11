# Sentroy Monitor — desktop agent

The menu-bar / tray agent for [Sentroy Monitor](https://monitor.sentroy.com) on **macOS, Windows and
Linux**: pick a task, start the timer, and the agent records tracked time and an activity percentage
against that task. Built with **Tauri 2 + Rust**; the UI is plain HTML/JS in `ui/` (six languages: en,
tr, ru, zh, es, id).

## What it records (and what it never does)

While **you** have started the timer, once a second the agent asks the OS two things (`src-tauri/src/os/`):

| Signal | macOS | Windows | Linux (X11) | Stored |
|---|---|---|---|---|
| Seconds since the last keyboard/mouse event | `CGEventSourceSecondsSinceLastEventType` | `GetLastInputInfo` | MIT-SCREEN-SAVER | active seconds (input within 1.5 s), idle seconds (no input ≥ 5 min) |
| Frontmost app | `NSWorkspace` app name | foreground process exe name | `_NET_ACTIVE_WINDOW` → process name | seconds per app name |

It never reads key codes, window titles or URLs, so it needs **no** Accessibility or Input Monitoring
permission. Every minute becomes one sample (`{start, end, projectId, taskId, activeSeconds,
idleSeconds, apps}`), queued on disk and uploaded every 30 s. The server de-duplicates by sample id,
so retries never double count. After the company's idle limit (`policy.idleStopMinutes`, default 30
minutes) without any input the timer stops by itself.

On a **Wayland** session an X11 client cannot see native Wayland input or windows: the agent still
tracks time, but reports input as unknown (no activity percentage, no idle auto-stop), no app names,
and takes no screenshots.

### Screenshots (only when the company turns them on)

The company decides in Sentroy Monitor whether this agent takes screenshots (`policy` in
`GET /api/agent/me`, re-read on start, on refresh and every 15 minutes). When they are **off** (the
default) the agent never captures the screen. When they are **on**:

- only while the timer runs, `perTenMinutes` (1–3) screenshots of the main display at random moments
  of each ten minutes of tracking;
- downscaled to at most 1600 px wide; if the company asked for **blur**, the image is blurred on this
  computer **before** it is written to disk or sent — the sharp image never leaves memory;
- a visible notice ("Screenshot taken") every time, plus a system notification when the window is not
  in front;
- queued as JPEG in `screenshots/` in the app data folder (at most 200, oldest dropped) and uploaded to
  `POST /api/agent/screenshots`; deleted after upload, and deleted on sign-out;
- on macOS this needs the **Screen Recording** permission. The agent shows a permissions sheet on start
  (and after sign-in / policy changes) and will not start the timer until it is granted; macOS applies
  the grant after a restart (the sheet has a *Restart app* button). Windows and Linux need no permission.

A first-run consent screen explains all this before anything can be tracked; the company can see the
data on the web timeline.

## Sign-in

Device authorization flow (RFC 8628 style), no password ever enters the app:

1. `POST /api/agent/device/start` → a code such as `ABCD-EFGH`; the browser opens
   `monitor.sentroy.com/connect?code=…`.
2. The user signs in with their Sentroy account there, picks the company and approves.
3. The agent polls `POST /api/agent/device/token` and receives a device token, stored in the OS
   credential store under `com.sentroy.monitor` (macOS Keychain, Windows Credential Manager, Secret
   Service on Linux). Settings and the offline queues live in the app data folder
   (`~/Library/Application Support/com.sentroy.monitor/`, `%APPDATA%\com.sentroy.monitor\`,
   `~/.local/share/com.sentroy.monitor/`).

Devices can be disconnected from the web (**Desktop app** page) or with *Sign out* in the agent.

The server is `https://monitor.sentroy.com` by default; the sign-in screen has a *Server* field for
other instances (`https://…`, or `http://localhost…` for development).

## Develop

```bash
npm install
npm run dev     # tauri dev
npm test        # cargo tests + UI tests (i18n parity, helpers)
```

To run against a local `apps/monitor` (monorepo), start it on port 3014 and set the server to
`http://localhost:3014` on the sign-in screen.

Linux builds need the WebKitGTK/appindicator packages plus `libxcb*`, `libdbus-1-dev`,
`libpipewire-0.3-dev`, `libgbm-dev`, `libegl-dev` and `libclang-dev` (see
`.github/workflows/release.yml` for the full list).

## Auto-update

The agent checks
`https://github.com/Sentroy-Co/sentroy-monitor-desktop/releases/latest/download/latest.json` on start
and every 6 hours (release builds only) and shows an *Install & restart* banner. Installing stops the
timer and delivers queued data first; it never restarts by itself while the timer runs. Updates are
verified with the public key in `src-tauri/tauri.conf.json` (`plugins.updater.pubkey`); the private
key is `~/.tauri/sentroy-monitor.key` locally and the `TAURI_SIGNING_PRIVATE_KEY` /
`TAURI_SIGNING_PRIVATE_KEY_PASSWORD` secrets in CI.

## Release

1. Bump the version in `package.json`, `src-tauri/Cargo.toml` and `src-tauri/tauri.conf.json`, commit,
   tag `vX.Y.Z` and push the tag.
2. **CI** (`.github/workflows/release.yml`) builds Windows (NSIS + MSI) and Linux (deb + AppImage),
   signs the updater artifacts, and creates/updates the GitHub release for the tag with the installers
   and a `latest.json` containing the `windows-x86_64` and `linux-x86_64` platforms (tauri-action merges
   into an existing `latest.json`; the two jobs run one after the other so they cannot race).
3. **macOS** (locally, after CI finished):

   ```bash
   ./build-mac.sh            # universal (arm64 + x64), signed, notarized, stapled
   ```

   Needs `SIGN_ID`, `APPLE_ID`, `TEAM_ID`, `APP_PASSWORD` in the environment (the same variables
   `sentroy-desktop/build-mac.sh` uses), the `x86_64-apple-darwin` Rust target and the updater key
   `~/.tauri/sentroy-monitor.key`. Output in `src-tauri/target/universal-apple-darwin/release/bundle/`:
   the DMG, `Sentroy-Monitor-X.Y.Z-mac-universal.app.tar.gz` + `.sig`, and `latest-mac.json`
   (`darwin-aarch64` + `darwin-x86_64`).
4. Merge the mac platforms into the release's manifest and upload:

   ```bash
   V=X.Y.Z; B=src-tauri/target/universal-apple-darwin/release/bundle
   gh release download v$V --pattern latest.json --output /tmp/latest-ci.json --clobber || true
   node scripts/merge-latest-json.mjs /tmp/latest-ci.json $B/latest-mac.json -o $B/latest.json
   gh release upload v$V $B/Sentroy-Monitor-$V-mac-universal.dmg \
     $B/Sentroy-Monitor-$V-mac-universal.app.tar.gz $B/Sentroy-Monitor-$V-mac-universal.app.tar.gz.sig \
     $B/latest.json --clobber
   ```

   `merge-latest-json.mjs` keeps every platform already in the release and refuses to merge different
   versions, so the order of steps 2 and 3 does not matter as long as each upload merges.
