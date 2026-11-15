# What the agent records

The full detail behind the summary in the [README](../README.md).

## Activity

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

## Screenshots (only when the company turns them on)

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

## Sign-in and local data

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
