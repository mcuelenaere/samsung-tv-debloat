#!/usr/bin/env bash
# Build the unsigned M4 filesystem and UI-impersonation probe TPKs.
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
IMAGE=mcr.microsoft.com/dotnet/sdk:3.1

docker run --rm --platform linux/amd64 \
    -v "$SCRIPT_DIR:/src" -w /src "$IMAGE" sh -eu -c '
        for item in fsprobe:FsProbe uiprobe:UiProbe; do
            package=${item%%:*}
            project=${item##*:}
            output="/src/out/dotnet-$package"
            pkgroot="/src/out/$package/pkgroot"
            rm -rf "$output" "$pkgroot"
            dotnet publish -c Release -o "$output" "/src/$package/$project.csproj"
            mkdir -p "$pkgroot/bin" "$pkgroot/shared/res"
            cp "$output/$project.dll" \
               "$output/$project.runtimeconfig.json" \
               "$output/$project.deps.json" \
               "$pkgroot/bin/"
            cp "/src/$package/tizen-manifest.xml" "$pkgroot/tizen-manifest.xml"
            cp /src/icon.png "$pkgroot/shared/res/org.tvauth.spike.$package.png"
        done
    '

for package in fsprobe uiprobe; do
    pkgroot="$SCRIPT_DIR/out/$package/pkgroot"
    tpk="$SCRIPT_DIR/out/$package/org.tvauth.spike.$package-0.1.0.tpk"
    rm -f "$tpk"
    ( cd "$pkgroot" && zip -q -X -r "$tpk" tizen-manifest.xml bin shared )
    ls -l "$pkgroot/bin" "$tpk"
    shasum -a 256 "$tpk"
done
