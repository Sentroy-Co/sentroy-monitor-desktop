# Development

The desktop agent is Tauri 2 and Rust, with a plain HTML/JS interface in `ui/`.

```bash
npm install
npm run dev     # run the app
npm test        # Rust tests + UI tests
```

To work against a local server, start it on port 3014 and enter `http://localhost:3014` on the
sign-in screen. Linux needs WebKitGTK, appindicator and the X11/PipeWire headers listed in
`.github/workflows/release.yml`.

- Self-hosting the server: [server/README.md](../server/README.md)
- What the agent records and sends: [PRIVACY.md](PRIVACY.md)
- Releases and auto-update signing: [RELEASING.md](RELEASING.md)
