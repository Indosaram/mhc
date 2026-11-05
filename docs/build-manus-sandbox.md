# Building on a Manus sandbox

A Manus free-plan task sandbox is a real Ubuntu 24.04 VM that can be joined to the tailnet over
Tailscale and reached with `ssh`. It is a fallback for when this Mac and `nbox` are both busy and a
Linux build cannot wait for GitHub Actions. It is not a permanent host: the sandbox suspends when
the Manus task goes idle, which kills running builds.

Verified end to end on 2026-11-05: `cargo build --offline -p maho-cli --bin mhc -j6` finished in
6m 52s and the binary reported `mhc 0.0.0`.

## Machine

| Item | Value |
| --- | --- |
| CPU | 6 vCPU — Intel Xeon @ 2.50 GHz, 1 socket / 3 cores / 2 threads per core, KVM full virt |
| RAM | 24 GB (`MemTotal` 25,004,324 kB) |
| Swap | 8 GB `/swapfile` — added by hand, not in the base image and not in `/etc/fstab` |
| Disk | 48 GB virtio (`/dev/vda`), single ext4 root |
| OS | Ubuntu 24.04.5 LTS, kernel 6.18.38+, x86_64 |
| GPU | none |
| Network | Tailscale userspace (`<private-macos>`), DERP relay only, no public IP |
| Access | `ssh manus-sbx` (openssh-server; ed25519 key in `~/.ssh/authorized_keys`) |

## Why the build is offline

The sandbox link to GitHub is fine (6.4 MB/s measured), but `static.crates.io` served 15–24 KB/s.
The mirrors tried did no better — `rsproxy` 20 KB/s, and `ustc` / `tuna` / `sjtu` did not respond.
Assume the registry is unreachable and vendor on the Mac.

## 1. Prepare the box

```bash
sudo apt-get update
sudo apt-get install -y build-essential curl git unzip pkg-config libssl-dev cmake clang \
  libclang-dev libsqlite3-dev protobuf-compiler rsync

curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain 1.95.0 \
  --component clippy --component rustfmt --component rust-src

sudo fallocate -l 8G /swapfile && sudo chmod 600 /swapfile
sudo mkswap /swapfile && sudo swapon /swapfile
```

Rust 1.95.0 matches `rust-toolchain.toml`, so the pinned toolchain is already installed when cargo
runs. `protoc` is required by `crates/maho-ai/build.rs` (it compiles `crates/maho-ai/proto`);
the distro `libprotoc 3.21.12` is enough. The swap file is not added to `/etc/fstab`, so a reboot
clears it — re-run those three commands after any restart.

## 2. Vendor on the Mac and transfer

Vendor from **the same commit the box checks out**. A newer local `HEAD` produces a lock that pins
crates the older checkout's `Cargo.lock` does not mention; the offline build then fails on version
selection rather than on the network.

```bash
# on the Mac, in a clean clone at the revision the box will build
git clone --depth 1 https://github.com/Indosaram/mhc.git /tmp/mhc-src
cd /tmp/mhc-src && cargo vendor --locked /tmp/vendor-mhc
tar --no-xattrs --no-mac-metadata -czf /tmp/vendor-mhc.tgz -C /tmp vendor-mhc

# macOS ships openrsync, which has no --append-verify; --append works
rsync -a --partial --append -e "ssh -o BatchMode=yes -o ServerAliveInterval=15" \
  /tmp/vendor-mhc.tgz manus-sbx:/tmp/
```

`--no-xattrs --no-mac-metadata` matters: without them the archive carries `._*` AppleDouble files
and drops some `.cargo-checksum.json` entries, and cargo rejects the vendored tree.

```bash
# on the box
tar xzf /tmp/vendor-mhc.tgz -C ~/vendor
mkdir -p ~/src && cd ~/src
git clone --depth 1 https://github.com/Indosaram/mhc.git
cat > ~/src/mhc/.cargo/config.toml <<'EOF'
[source.crates-io]
replace-with = "vendored-sources"

[source.vendored-sources]
directory = "/home/ubuntu/vendor/vendor-mhc"

[net]
offline = true
EOF
```

This replaces the repository's own `.cargo/config.toml`, which intentionally sets no
`build.target-dir`. Do not copy the box's config back into the checkout.

## 3. Build

```bash
cd ~/src/mhc
CARGO_PROFILE_DEV_DEBUG=0 cargo build --offline -p maho-cli --bin mhc -j6
```

The resulting binary is `target/debug/mhc` (461 MB in this run). `CARGO_PROFILE_DEV_DEBUG=0` is
not required, but full debuginfo roughly doubles the 18 GB `target` and the box only has 48 GB.

For the full artifact set use `cargo build --offline --workspace --bins --release` instead, then
package with `tools/package-native.mjs` as described in the README. That path needs `OMO_SRC` and
much more disk than a debug binary.

## 4. Verify

```bash
./target/debug/mhc --version     # mhc 0.0.0
```

## Disk budget

| Artifact | Size |
| --- | --- |
| `target/` after a debug build | ~18 GB (10 GB of it `incremental`) |
| `vendor/vendor-mhc` | 1.5 GB |
| `target/debug/mhc` | 461 MB |

48 GB holds one repository at a time, not two. After building, copy the binary somewhere durable
and delete the parts that regenerate:

```bash
mkdir -p ~/bin && cp target/debug/mhc ~/bin/
rm -rf target/debug/incremental      # 10 GB, regenerated on the next build
rm -rf target                         # 18 GB, rebuilt from the vendored sources
```

The vendor directory is reusable and worth keeping while the box lives; it is what makes a rebuild
possible at all.

## Sleeping and waking

When the Manus task goes idle the sandbox suspends and `ssh` times out. Memory and disk survive;
processes do not, so a suspend kills a running build. Wake it by sending any message to the task in
the Manus web app, then reconnect once the node is back online. Run long builds under `nohup` with
a log file so a suspend costs the run but not the evidence.
