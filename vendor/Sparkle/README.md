# Sparkle release tools

This directory contains the Sparkle 2.9.6 macOS universal binaries needed to
generate and sign appcast files locally:

- `bin/generate_appcast` generates and signs an appcast from update archives.
- `bin/generate_keys` creates an Ed25519 key pair when setting up a release
  channel for the first time.

The application runtime framework remains in `vendor/Sparkle.framework`.
These tools are macOS-only and require the Sparkle private key from the
Keychain or an external secret; private keys must not be committed here.

The included binaries are distributed under the Sparkle license. See
[`LICENSE`](LICENSE).
