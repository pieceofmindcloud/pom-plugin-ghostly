# Ghostly

[Ghostly](https://github.com/MiguelMedeiros/ghostly), the end-to-end encrypted, peer-to-peer messenger with wallets, inside the POM, and a group of every node of the network. There is no chat server: peers meet on the Mainline DHT and talk directly.

The plugin's screen is **Ghostly's own web app**, full screen: chats, groups, identities and wallets, with the profile kept in your browser. It opens straight on the join screen of the network's group, with the network's link, so a browser asks to be let in without a click.

It follows the POM's theme: when the POM switches between light and dark, Ghostly switches with it, in the POM's colours (its surfaces, lines, text and teal accent over Ghostly's ice-blue theme). The POM's theme wins over the one chosen in Ghostly's Settings.

## What happens when the plugin starts

1. **Runtime.** It downloads Node.js `v24.22.0` (verified against the SHA-256 published by nodejs.org), asks GitHub for the latest Ghostly release, downloads that tag's source from the original repository and builds the Ghostly CLI and web app with `npm` (the web app with `--base` set to the plugin's proxy path). The Ghostly code is not modified. Each start checks for a newer release and builds it, keeping the previous build until the new one is complete; offline, the newest build is used.
2. **Engine.** It runs `ghostly daemon` on the node's own profile, in the plugin's data directory: the node's member of the group, online while the node is.
3. **Identity.** The profile gets a Ghostly identity proof (`did`, WISP 311) for the node's own Ed25519 key, the key behind its node id and its pkarr record, written as a `did:key`. The node signs the statement; the key never leaves the node.
4. **Group.** The principal of the network creates one Ghostly community, `POM network`. The other nodes' plugins ask it for the link through the network's signed peer channel and join. When the principal changes, the nodes move to the new principal's group.
5. **Members.** Every plugin tells the others, through the same signed channel, which member key it holds in the group, so the node can tell how many members are nodes of the network.

## Requirements

- POM with the plugin host API (`host_api` in `host.configure`).
- Linux x86_64, Linux arm64 or macOS arm64.
- Internet access on the first start (nodejs.org, GitHub, the npm registry) and for the chat itself (the Mainline DHT and public Pkarr relays).
- About 800 MB of disk for Node.js and the Ghostly build, and about 1 GB more while it builds.

## Data

Everything of the node lives in `ghostly/` beside the plugin database: `node/`, `cli/<tag>/` (the CLI, and `web/`, the web app), `home/` (the node's Ghostly profile, keys and history), `network-group.json` (the group and its link), `build.log` and `daemon.log`. Your own profile and wallets live in your browser's storage for the POM's address: another browser is another profile (Ghostly's Devices can add it to the same one).

## Limits

- The web app expects to sit at the root of its site. Under the POM's path, notifications (its service worker) and mini-apps (its sandboxed runner) do not work; chats, groups, identities and wallets do.
- Ghostly opens a profile in one tab at a time.
- A community holds up to 256 members.
- The group's link is a secret: the node hands it only to live peers of its own network.
