# YTM Glass

YTM Glass is an unofficial desktop shell for YouTube Music on Linux. The official YouTube Music web player, loaded in a hidden window, plays the audio. A native Rust core keeps the application state, serves the library and search views, publishes MPRIS for desktop media controls, and runs a local MCP server through which an AI agent can browse, queue music and act as a DJ.

## Disclaimer

YTM Glass is not affiliated with, endorsed by or sponsored by Google or YouTube. "YouTube" and "YouTube Music" are trademarks of Google LLC.

- Playback happens in the official web player at `music.youtube.com`. YTM Glass does not download, extract, rewrite or store media streams, and has no offline audio.
- You sign in yourself, on the official Google sign-in page shown inside the app. YTM Glass never asks for, records or automates your credentials, and does not import cookies from other browsers.
- Use it in line with the YouTube Terms of Service and your YouTube Music subscription.

## Features

- A native window with a Svelte interface: home, search, library, playlists, albums, artists, the queue and synchronized lyrics (official lyrics or LRCLIB).
- A refractive "liquid glass" material for the sidebar, player bar and popovers, with reduced-transparency, reduced-motion and forced-colors fallbacks. See [docs/design/liquid-glass.md](docs/design/liquid-glass.md).
- English interface by default, with a Chinese (zh-CN) translation selectable in Settings.
- One command path for the interface, the tray, MPRIS and the MCP server, so they all see the same state. Manual input always wins over an agent.
- KDE media keys and the Plasma media applet through MPRIS (`org.mpris.MediaPlayer2.ytm_glass`), a tray icon, and optional track-change notifications (off by default).
- Playback state, queue position and settings survive restarts. Playback resumes paused by default.
- An MCP adapter, `ytm-glass-mcp`, with 39 tools and an AI DJ loop. See [docs/mcp.md](docs/mcp.md) and [docs/ai-dj.md](docs/ai-dj.md).

## Platform status

YTM Glass is developed and tested on an Arch-based distribution (CachyOS) with KDE Plasma 6 on native Wayland, on x86_64 with an NVIDIA GPU. Other distributions, desktops, X11 sessions and GPUs are untested. The packaging targets Arch (`makepkg`). Known limits are listed in [docs/known-issues.md](docs/known-issues.md).

## Build and install

You need Rust (version pinned in [rust-toolchain.toml](rust-toolchain.toml)), Node.js and pnpm (versions pinned in [package.json](package.json)), [`just`](https://github.com/casey/just), and `base-devel` for packaging. Details are in [docs/building.md](docs/building.md).

```sh
pnpm install --frozen-lockfile
just test            # offline Rust and TypeScript tests
just lint            # rustfmt, clippy and type checks
just dev-mock        # the interface with synthetic data, no account needed
just dev             # a development build against the official web player
```

To build and install the Arch package from a clean, committed tree:

```sh
just package                                   # writes packaging/out/ytm-glass-<version>-x86_64.pkg.tar.zst
sudo pacman -U packaging/out/ytm-glass-*.pkg.tar.zst
```

`just package` runs [packaging/PKGBUILD](packaging/PKGBUILD) with `makepkg --cleanbuild` in a temporary directory and refuses a dirty working tree. `sudo pacman -R ytm-glass` removes the package and keeps your profile, settings and cache.

## First run

1. Start **YTM Glass** from the application launcher, or run `ytm-glass`.
2. While you are signed out, a banner offers to open the official player page. Complete the Google sign-in there; the session is kept in the app's own browser profile.
3. Open the library or search and pick something to play. Use the player bar, the tray icon or your media keys.

If the official player page disconnects, a banner offers to reopen it. Features the page does not offer at the moment are shown as unavailable rather than faked.

## AI DJ quick start

The MCP adapter talks to the running player over a private socket that only your user can open.

```sh
# Register a client. The account defaults to the one signed in to the player.
ytm-glass-mcp register --principal claude-dj --grant dj
# The command prints the credential file path. Restart YTM Glass once after the first registration.

# Add the server to Claude Code (replace the path with the printed one):
claude mcp add ytm -s user -- /usr/bin/ytm-glass-mcp \
  --credential-file ~/.config/ytm-glass/control-credentials/claude-dj.json --allow-background-start
```

Other MCP clients use the same stdio command and arguments. Then ask the agent to be your DJ: it calls `ytm_wait_dj_prompt` in a loop and acts on each prompt. `--grant dj` covers playback, the queue, device settings and taste notes; ratings, library saves and playlist edits need their own grants and have daily limits. See [docs/mcp.md](docs/mcp.md).

## Project layout

| Path | Contents |
| --- | --- |
| `apps/desktop/` | Electron host: windows, sessions, tray, the daemon process and IPC guards |
| `bridge/` | The page shim and isolated bridge injected into the official web player |
| `crates/core/` | Engine actor, command lifecycle, the control service for MCP clients and the DJ prompts |
| `crates/daemon/` | `ytm-daemon`: wires the core to stdio, SQLite, catalog, lyrics, MPRIS and the control socket |
| `crates/ytm-data/` | YouTube Music catalog reads and writes |
| `crates/storage/` | SQLite schema, migrations, operation journal and cover cache |
| `crates/lyrics/`, `crates/mpris/` | Lyrics sources and LRC parsing; the MPRIS service |
| `crates/protocol/`, `crates/control-api/` | Host wire protocol v1 and the local control protocol |
| `crates/mcp/` | `ytm-glass-mcp`: the MCP stdio adapter and the `register` command |
| `ui/` | The Svelte interface and the liquid glass material |
| `packages/protocol/` | Generated JSON Schema and TypeScript types |
| `packaging/` | PKGBUILD, launcher, desktop entry, icon and license notices |
| `tools/` | MCP live checks, QA and probe tools, packaging and release scripts |
| `tests/`, `fixtures/` | Process, desktop and end-to-end tests; synthetic fixtures |

## Documentation

- [Architecture](docs/architecture.md): processes, channels, the page bridge, the Rust core and the interface.
- [MCP server](docs/mcp.md): registration, grants, account limits, results and the tool list.
- [AI DJ](docs/ai-dj.md): the prompt loop, track-ending prompts, check-ins and taste notes.
- [Design goals](docs/design-goals.md): the principles behind the MCP and DJ design and their acceptance criteria.
- [Building and testing](docs/building.md): toolchain, `just` recipes, packaging, live checks, troubleshooting and the public export.
- [Known issues](docs/known-issues.md)
- [Liquid glass design](docs/design/liquid-glass.md)
- [Changelog](CHANGELOG.md)

## License

YTM Glass is released under the [MIT License](LICENSE). The package installs third-party notices for Electron, Chromium, the Inter font and the Rust and npm dependencies under `/usr/share/licenses/ytm-glass/`; they are generated at build time by [tools/package/gen-notices.mjs](tools/package/gen-notices.mjs).
