# mm-overlay: the hub through the Transferd overlay

ModelMistress's desktop hub stays on loopback. `mm-overlay` publishes it to paired devices through the Transferd overlay,
so a phone can use the desktop's models from any network without a LAN or Tailscale address, and without opening a
firewall port. The crate also holds the overlay node that the Android app runs (through `android/overlay-jni`).

- The hub's routes and its peer-key check are unchanged. A phone still sends its peer key as a bearer token.
- Only nodes paired through an invite can open the published service (`*paired`).
- The node dials out. It opens an inbound port only if you give `--listen`.

## Build

This crate is standalone (its own `[workspace]`, not part of the desktop workspace). It takes Transferd from GitHub at the
commit pinned in `Cargo.toml`, the same commit ComputeFabric uses (`docs/TRANSFERD-BINDING.md` in ComputeFabric).

```bash
cd overlay
cargo build --release
cargo test
```

The tests include a loopback run with two nodes in one process: a desktop publishes a fake hub, a phone pairs by invite and
reads the hub through the overlay, and an unpaired node is refused.

## Run on the desktop

1. Start the hub as usual. Its address is in `control.json` in the hub's home folder (`url`).
2. Make a peer key for the phone: `model-mistress keys create phone`. The key is shown once.
3. Start the overlay node, with a folder of its own for its identity and state:

```bash
mm-overlay serve --home <folder> --hub 127.0.0.1:<hub port> --name home --invite-ttl 900
```

It prints the node id and an invite. Enter the invite and the peer key in the app's Desktops panel, under "Add through the
Transferd overlay". An invite pairs one device, then it is spent. While `serve` runs, type `invite 900` (any number of
seconds) in its window for another invite, or `status`. Each invite is good only while this process runs.

Options:

- `--listen 127.0.0.1:<port>`: a direct TCP listener. It is published in the invite, so a phone that can reach that
  address can dial the desktop directly. Use a loopback address and a tunnel, or leave it out.
- `--relay <host:port>` (repeatable): a UDP relay to register on. Without a relay, a phone reaches the desktop only
  through a direct address. The relay must be reachable from both the phone and the desktop; see the README's known limits.
- `--direct <host:port>` (repeatable): an extra address to publish in invites.

`--hub` must be a loopback address. `mm-overlay id --home <folder>` prints the node id and creates the identity if needed.

## Where things are

- `src/lib.rs`: the node wrapper shared by the desktop tool and the Android bridge.
- `src/main.rs`: the `mm-overlay` command.
- `tests/loopback.rs`: the two-node loopback test.
- `../android/overlay-jni`: the JNI bridge the app calls. Gradle builds it with cargo-ndk (`:runtime:buildOverlayNative`).
