# Vendored xterm.js

Terminal emulator for the exec (shell) windows. Bundled so the app works offline.

| File | Package | Integrity (npm `dist.integrity`, verified on download) |
|---|---|---|
| `xterm.js`, `xterm.css` | `@xterm/xterm@5.5.0` (`lib/`, `css/`) | `sha512` from `https://registry.npmjs.org/@xterm/xterm/5.5.0` |
| `addon-fit.js` | `@xterm/addon-fit@0.10.0` (`lib/`) | `sha512` from `https://registry.npmjs.org/@xterm/addon-fit/0.10.0` |

Only change: the trailing `//# sourceMappingURL=` comment was removed (maps aren't bundled).
Both are MIT licensed; see `LICENSE`. To update, download the new tarballs, check their
integrity against the registry, and replace these files.
