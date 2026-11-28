# OpacusR iOS app shell

An unsigned iPhone app bundle built by CI: the Rust static library
(`crates/opacusr-ios` → `libopacusr_ios.a`) linked with the Swift UIKit
entry into a real Mach-O (`OpacusR.app`), packaged as
`opacusr-<version>-ios-unsigned.ipa`.

## What is verified in CI

`cargo check --target aarch64-apple-ios` only type-checks. This shell
does the real thing on `macos-latest`:

1. `cargo rustc -p opacusr-ios -- --print native-static-libs` builds the
   static library and prints the canonical framework/library list needed
   to link against it (Metal, UIKit, Security, CoreGraphics, ...) — that
   list is passed to the linker verbatim, so it tracks every
   wgpu/winit/objc2 bump automatically.
2. `swiftc -target arm64-apple-ios13.0` compiles `App/AppDelegate.swift`
   (with the C bridging header) and links `libopacusr_ios.a` + the
   native flags into the bundle executable.
3. `plutil -lint` the Info.plist, `file`/`otool`/`nm` sanity: arm64
   Mach-O, iOS SDK linkage, `_opacusr_ios_run` entry symbol present.
4. Package `Payload/OpacusR.app` → unsigned IPA.

## Why unsigned

There is no Apple Developer certificate in CI (a paid account is
required for code signing; TestFlight/App Store later). An unsigned IPA
cannot be installed as-is — self-sign it first:

- **Xcode (free Personal Team):** `iOS App Signer`-style tools aside,
  the clean way is: open `Payload/OpacusR.app` in Xcode via an empty
  project, or use `codesign` with your identity and an updated
  `CFBundleIdentifier` (Personal Teams may not use `io.opacusr.*`).
- **AltStore / Sideloadly / TrollStore:** point them at the IPA; they
  re-sign automatically with your Apple ID.

## Files

| Path | Purpose |
| --- | --- |
| `App/AppDelegate.swift` | UIKit entry: hands the main thread to `opacusr_ios_run()` via `DispatchQueue.main.async` after launch (blocking `didFinishLaunchingWithOptions` would deadlock — winit needs the run loop to start first). |
| `App/bridging.h` | C declaration of `opacusr_ios_run` for Swift (`-import-objc-header`). |
| `App/Info.plist.in` | Bundle manifest template; `@VERSION@` comes from `Cargo.toml` (or `$VERSION`). Minimum iOS 13.0, no storyboard — winit owns the window. |
| `scripts/build_ios_app.sh` | The whole pipeline above; used by CI (`ci.yml` debug, `release.yml` release). |

## Roadmap

- TestFlight / App Store build: needs an Apple Developer account, a
  signing identity + provisioning profile in CI secrets, a real
  `.xcodeproj` (or `xcodegen`) and app icons.
- ReplayKit broadcast capture (phone-as-host) is a separate item — the
  viewer works without it.
