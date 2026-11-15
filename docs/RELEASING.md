# Releasing

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

## Auto-update

The agent checks
`https://github.com/Sentroy-Co/sentroy-monitor-desktop/releases/latest/download/latest.json` on start
and every 6 hours (release builds only) and shows an *Install & restart* banner. Installing stops the
timer and delivers queued data first; it never restarts by itself while the timer runs. Updates are
verified with the public key in `src-tauri/tauri.conf.json` (`plugins.updater.pubkey`); the private
key is `~/.tauri/sentroy-monitor.key` locally and the `TAURI_SIGNING_PRIVATE_KEY` /
`TAURI_SIGNING_PRIVATE_KEY_PASSWORD` secrets in CI.
