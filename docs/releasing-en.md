# Release Process (Maintainer Guide)

This document covers maintainer-only release and packaging details: the Homebrew cask pipeline, code-signing rationale, and application icon generation. For everyday installation and development, see the README.

## Homebrew cask release

`scripts/release.sh` is the complete release pipeline. It runs `bundle.sh` to build the `.app`, `.dmg`, Sparkle `.zip`, and signatures, then generates `dist/oh-my-tab.rb`, a Homebrew cask containing the DMG `sha256`, the version read from `Cargo.toml`, and a `zap trash:` block that removes the icon cache, logs, and application data when the cask is uninstalled. Nothing is uploaded by default; R2 is used only when `--push` is explicitly supplied.

```sh
sh scripts/release.sh                    # build locally without R2 access
sh scripts/release.sh --push             # build, then upload ZIP, DMG, and appcast.xml
sh scripts/release.sh --push --dry-run   # inspect and print the upload plan
```

The development channel uses a separate bundle ID, feed, R2 prefix, and archive prefix, so it cannot mix with production updates:

```sh
sh scripts/release-dev.sh                 # build the development package only
sh scripts/release-dev.sh --push          # upload to dev_release and publish its appcast
sh scripts/release-dev.sh --push --dry-run
```

`release-dev.sh` still uses an optimized Release build, but enables the `dev-long-text` Cargo feature. The development package therefore includes a `[TEST] English x3` language option for checking long dropdown values, settings rows, and card layouts. The production `release.sh` and direct `bundle.sh` paths do not enable this feature.

With `--push`, the scripts always rebuild the `.app`, ZIP, and DMG from the current source tree before invoking the pinned `vendor/Sparkle/bin/generate_appcast` tool. If a local appcast exists, it is used first; on a clean checkout, the public feed is read to preserve historical entries. If the feed does not exist, a new one is created. The enclosure URL is generated from the final ZIP filename in the temporary directory so it matches the object subsequently uploaded by the R2 publisher.

By default, appcast signing reads the Ed25519 private key named `ed25519` from the macOS Keychain. `SPARKLE_ED_KEY_FILE` can point to an external private-key file; never commit that file. `--push --dry-run` only prints the upload plan and does not generate or upload files.

The cask hard-codes `depends_on macos: :ventura` and `depends_on arch: :arm64`, so it supports macOS 13+ on Apple Silicon only. Its URL points to `https://github.com/eacryo/oh-my-tab/releases/download/v#{version}/Oh-My-Tab.dmg`, so the DMG must be uploaded to a GitHub release tagged `v<version>`, matching the version in `Cargo.toml`.

Release a new version as follows:

1. Change the `version` in `Cargo.toml`.
2. Run `sh scripts/release.sh` to produce `dist/Oh-My-Tab.dmg` and `dist/oh-my-tab.rb`.
3. Create a GitHub release tagged `v<version>` and upload `dist/Oh-My-Tab.dmg`.
4. Copy `dist/oh-my-tab.rb` into the `Casks/` directory of the [homebrew-tap](https://github.com/eacryo/homebrew-tap) repository and push it.

After step 4, `brew install --cask eacryo/tap/oh-my-tab` (or `brew upgrade --cask`) can install the new version. `brew install --cask` reads the committed cask from the tap repository; `release.sh` only regenerates it locally for copying.

The `--push` flow uses `tools/r2-publisher` and reads credentials only from environment variables: `R2_ACCESS_KEY_ID`, `R2_SECRET_ACCESS_KEY`, `R2_BUCKET`, `R2_ENDPOINT`, or `R2_ACCOUNT_ID`. It uploads the ZIP and DMG first. If `R2_LATEST_DMG_KEY` is set, it also overwrites a latest-DMG alias with a short-cache policy, then uploads the newly generated appcast. Production objects default to `releases/` plus `appcast.xml`, with the root `Oh-My-Tab.dmg` as the latest-DMG alias. Development artifacts use `dev_release/` and the `Oh-My-Tab-Dev-...` archive prefix. These values can be overridden with environment variables including `R2_RELEASE_PREFIX`, `R2_APPCAST_KEY`, `R2_ARTIFACT_BASENAME`, and `R2_LATEST_DMG_KEY`.

Upload targets and download URLs are separate. Uploads always use the S3 endpoint from `R2_ENDPOINT` (or the endpoint derived from `R2_ACCOUNT_ID`) and `R2_BUCKET`; `https://download.oh-my-tab.app` is used only by clients to access appcasts and archives. `R2_PUBLIC_BASE_URL` changes only the public URL shown in the release plan.

## Sparkle update materials

Regular `cargo build` and `cargo test` do not require Sparkle. The repository includes a pinned Sparkle 2 framework at `vendor/Sparkle.framework`, so the default packaging path can build an `.app` with update support. It also includes the pinned Sparkle 2.9.6 `vendor/Sparkle/bin/generate_keys` and `vendor/Sparkle/bin/generate_appcast` tools for generating keys and appcasts; both are macOS universal binaries. See `vendor/Sparkle/LICENSE` for the relevant license.

At runtime, the updater loads `Contents/Frameworks/Sparkle.framework`. Place the Sparkle 2 framework at `vendor/Sparkle.framework`, or set `SPARKLE_FRAMEWORK_PATH`; `bundle.sh` and `dev-restart.sh` copy it automatically. Production packages use `https://download.oh-my-tab.app/appcast.xml` as `SUFeedURL`, while development restart and release scripts use `https://download.oh-my-tab.app/dev_release/appcast.xml`. `SPARKLE_FEED_URL` can override the feed URL.

The publisher uploads `appcast.xml` and update archives to R2. Appcasts are signed with Sparkle's Ed25519 private key; packaging only injects the corresponding public key through `SPARKLE_PUBLIC_ED_KEY`. Sparkle compares `CFBundleVersion` (the build number); scripts use a UTC timestamp by default, while reproducible tests can set `SPARKLE_BUILD_VERSION`. `CFBundleShortVersionString` remains the user-visible version. Never commit the private key, place it in the app bundle, or upload it to R2.

Release notes must be placed in the version-specific directories: production builds use `release_doc/<version>.md`, and development builds use `release_doc_dev/<version>.md`. For version `0.2.0`, both `release_doc/0.2.0.md` and `release_doc_dev/0.2.0.md` must exist; the corresponding build fails before compilation if either file is missing. The release scripts embed the complete Markdown in the new appcast item.

A single Markdown file may contain multiple language blocks. Start blocks with `<!-- locale: en -->` or `<!-- locale: zh-Hans -->` and close them with `<!-- /locale -->`. The app reads the single Sparkle `<description>` and selects the block matching the current UI locale; if no translation is available, it falls back to English and then to the first block in the file. Older single-language Markdown files remain compatible.

## Code signing: why a self-signed certificate stabilizes permissions

`bundle.sh` prefers the **`oh-my-tab-sign`** self-signed identity and falls back to ad-hoc signing (`codesign -s -`) if the certificate is missing or signing fails.

**Why:** an ad-hoc signed app's designated requirement is only its raw CDHash, which changes on every rebuild. macOS TCC records Accessibility permission against that CDHash, so every rebuild invalidates the grant. A self-signed certificate makes the designated requirement certificate-based, so it stays stable across rebuilds.

Create the certificate once in Keychain Access:

1. *Keychain Access → Certificate Assistant → Create a Certificate...*
2. Name it `oh-my-tab-sign`; choose **Self Signed Root** and **Code Signing**.
3. Create it. The first `bundle.sh` run may ask for keychain access; choose *Always Allow*.

Then rebuild, reinstall, and grant Accessibility permission once. Future rebuilds use the same signing identity and do not require reauthorization. If a grant is stale, for example because of old ad-hoc installations, clear it with:

```sh
tccutil reset Accessibility com.eacryo.oh-my-tab
```

**Note:** a self-signed certificate stabilizes TCC identity but does **not** satisfy Gatekeeper for distribution. Other users may still see an unidentified-developer warning. Proper distribution requires a paid Apple **Developer ID Application** certificate; set `SIGN_IDENTITY` in `scripts/bundle.sh` to that identity.

## Application icon

The application icon (`AppIcon.icns`) is generated from `assets/Icon-Default-1024x1024@1x.png` and packaged into `Contents/Resources/`. `assets/AppIcon.icns` is committed, so contributors do not need extra tools to build the `.app`.

After replacing the source PNG, regenerate the icon:

```sh
./scripts/build-icon-from-png.sh   # 1024x1024 PNG -> 10 .iconset sizes (sips) -> assets/AppIcon.icns
```

Only `iconutil` (Xcode Command Line Tools) is required; `sips` is built into macOS. Commit the regenerated `assets/AppIcon.icns` together with changes to `assets/Icon-Default-1024x1024@1x.png`.

If `assets/AppIcon.icon` exists as a directory, `bundle.sh` also copies it into `Contents/Resources/` for the macOS 26+ Liquid Glass icon format. The system prefers it over `.icns`.
