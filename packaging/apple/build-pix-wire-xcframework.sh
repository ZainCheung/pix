#!/bin/sh
set -eu

repository_root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
output_root=${1:-"$repository_root/build/apple"}
bindings_directory="$output_root/bindings"
headers_directory="$output_root/headers"
simulator_library="$output_root/libpix_wire_sim.a"
xcframework="$output_root/PixWireFFI.xcframework"

cd "$repository_root"

for target in aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios; do
    rustup target list --installed | grep -qx "$target" || {
        printf '%s\n' "Missing Rust target: $target" >&2
        printf '%s\n' "Install it with: rustup target add $target" >&2
        exit 1
    }
done

cargo build -p pix-wire --release --target aarch64-apple-ios
cargo build -p pix-wire --release --target aarch64-apple-ios-sim
cargo build -p pix-wire --release --target x86_64-apple-ios
cargo build -p pix-wire --release --features bindgen --bin uniffi-bindgen-swift

mkdir -p "$bindings_directory" "$headers_directory"
rm -f "$bindings_directory/PixWire.swift"
rm -f "$headers_directory/PixWireFFI.h" "$headers_directory/module.modulemap"

"$repository_root/target/release/uniffi-bindgen-swift" \
    "$repository_root/target/aarch64-apple-ios/release/libpix_wire.a" \
    "$bindings_directory" \
    --swift-sources

"$repository_root/target/release/uniffi-bindgen-swift" \
    "$repository_root/target/aarch64-apple-ios/release/libpix_wire.a" \
    "$headers_directory" \
    --headers \
    --modulemap \
    --module-name PixWireFFI \
    --modulemap-filename module.modulemap

lipo -create \
    "$repository_root/target/aarch64-apple-ios-sim/release/libpix_wire.a" \
    "$repository_root/target/x86_64-apple-ios/release/libpix_wire.a" \
    -output "$simulator_library"

if [ -e "$xcframework" ]; then
    rm -R "$xcframework"
fi

xcodebuild -create-xcframework \
    -library "$repository_root/target/aarch64-apple-ios/release/libpix_wire.a" \
    -headers "$headers_directory" \
    -library "$simulator_library" \
    -headers "$headers_directory" \
    -output "$xcframework"

simulator_sdk=$(xcrun --sdk iphonesimulator --show-sdk-path)
swiftc -typecheck \
    -target arm64-apple-ios18.0-simulator \
    -sdk "$simulator_sdk" \
    -I "$xcframework/ios-arm64_x86_64-simulator/Headers" \
    "$bindings_directory/PixWire.swift"

printf '%s\n' "Generated $xcframework"
printf '%s\n' "Generated $bindings_directory/PixWire.swift"
