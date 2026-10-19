# POM - Ghostly

POM plugin that brings [Ghostly](https://github.com/MiguelMedeiros/ghostly), the end-to-end encrypted, peer-to-peer messenger with wallets, into the POM with its own web app as the screen, and puts every node of the network in one Ghostly group, without modifying Ghostly.

- **Runtime** (`src/runtime.rs`): a pinned Node.js release, verified by SHA-256, and the Ghostly CLI and web app built on the node from the latest release tag of the original repository (`npm ci`, `npm run build` of both workspaces, the web app with `--base=/api/ui/plugins/ghostly/proxy/web/`, `npm pack`, `npm install -g --prefix`). Every start checks for a newer release.
- **Node member** (`src/chat.rs`): keeps `ghostly daemon` running on the node's own profile, makes its `did:key` identity proof with the node's Ed25519 key, creates the network's community on the principal and joins it everywhere else.
- **Node API** (`src/node_api.rs`): the POM's loopback API for plugins, handed over in `host.configure` as `host_api`: `GET /node`, `GET /peers`, `POST /peer` (this plugin's `peer.<op>` on another node, over the signed peer call) and `POST /sign` (the node key signs only an identity statement for its own `did:key`).
- **Screen** (`ui/src/screens/Ghostly.tsx`): Ghostly's own web app in a frame (`GET /web/…` of the plugin's upstream, `src/upstream.rs`, through the POM proxy), its profile and wallets in the browser, under a POM bar that reads `GET /state`, opens the network group's join link in the app, and signs the browser profile's `did:key` statement with the node key (`POST /sign`).

Peer operations the plugin answers: `peer.group_link` (the group and its link) and `peer.whoami` (this node's member key).

See [docs/README.md](docs/README.md) for what happens at start, the requirements and the data layout.

## Build and verify

Requirements: Rust stable, Node.js 20 or newer, and npm.

```sh
cargo test
cargo fmt --check
scripts/build-ui.sh
scripts/build.sh --platform linux-x86_64
scripts/package.sh --platform linux-x86_64 --version 0.1.0
```

Supported package platforms: `linux-x86_64` and `macos-aarch64` (the runtime also supports Linux arm64). The manual `Publish plugin release` workflow attaches the packages and their `pom-plugin-<os>-<arch>.json` manifests to a GitHub release, for POM's **Add from GitHub** flow.
