# ccplan Cockpit

A native desktop app for ccplan, built with [Tauri](https://tauri.app) (Rust backend +
HTML/CSS/JS frontend in the system WebView). It is a real GUI — click to mark blocks
done, snooze, approve runs, and add blocks through a form — not a terminal in a window.

The backend is deliberately thin: reads go through the pure `ccplan::gui::model` view-model
builders, and every mutation is funnelled through `ccplan::run` (the exact entrypoint the
`ccplan` CLI uses), so all domain invariants are reused rather than re-implemented.

## Layout

- `src-tauri/` — the Tauri Rust crate (`cockpit` binary). Its own workspace; depends on
  `ccplan` by path. Not part of ccplan's `--workspace` test/coverage gate.
- `dist/` — the static frontend shipped into the WebView (`index.html`, `styles.css`, `app.js`).
- `preview.html` — a browser-openable harness with mock data for iterating on the UI without
  a build (`xdg-open cockpit/preview.html`).

## Build & run

System prerequisites (Linux only — macOS/Windows use the system WebView):

```sh
sudo apt install libwebkit2gtk-4.1-dev build-essential curl wget file \
  libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev
cargo install tauri-cli --version "^2.0" --locked
```

Then, from `cockpit/src-tauri`:

```sh
cargo tauri dev      # run in development
cargo tauri build    # produce a signed-able bundle (.app/.dmg, .msi, AppImage/.deb)
```

`ccplan gui` launches the `cockpit` binary when it sits next to the `ccplan` executable.
