# Sentroy Monitor — desktop agent

The macOS menu-bar agent for [Sentroy Monitor](https://monitor.sentroy.com): pick a task, start the
timer, and the agent records tracked time and an activity percentage against that task. Built with
**Tauri 2 + Rust**; the UI is plain HTML/JS in `ui/` (six languages: en, tr, ru, zh, es, id).

## What it records (and what it never does)

While **you** have started the timer, once a second the agent asks macOS two things:

| Signal | API | Stored |
|---|---|---|
| Seconds since the last keyboard/mouse event | `CGEventSourceSecondsSinceLastEventType` | active seconds (input within 1.5 s), idle seconds (no input ≥ 5 min) |
| Name of the frontmost app | `NSWorkspace.frontmostApplication.localizedName` | seconds per app name |

It never reads key codes, screen contents, window titles or URLs, so it needs **no** Accessibility,
Input Monitoring or Screen Recording permission. Every minute becomes one sample
(`{start, end, projectId, taskId, activeSeconds, idleSeconds, apps}`), queued on disk and uploaded every
30 s. The server de-duplicates by sample id, so retries never double count. After 30 minutes without
any input the timer stops by itself.

A first-run consent screen explains this before anything can be tracked; the company can see the data
on the web timeline.

## Sign-in

Device authorization flow (RFC 8628 style), no password ever enters the app:

1. `POST /api/agent/device/start` → a code such as `ABCD-EFGH`; the browser opens
   `monitor.sentroy.com/connect?code=…`.
2. The user signs in with their Sentroy account there, picks the company and approves.
3. The agent polls `POST /api/agent/device/token` and receives a device token, stored in the
   **macOS Keychain** (`com.sentroy.monitor`). Settings and the offline queue live in
   `~/Library/Application Support/com.sentroy.monitor/`.

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

## Release (signed + notarized DMG)

```bash
./build-mac.sh            # universal (arm64 + x64), signed, notarized, stapled
```

Needs `SIGN_ID`, `APPLE_ID`, `TEAM_ID`, `APP_PASSWORD` in the environment (the same variables
`sentroy-desktop/build-mac.sh` uses) and the `x86_64-apple-darwin` Rust target.
