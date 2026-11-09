#!/usr/bin/env bash
# Build and sign the package-install probe TPK.
#
# The probe reads a raw JSON request from /tmp/tvauth-probe.request, sends it
# to /run/tv-auth/broker.sock, and writes the response line to
# /tmp/tvauth-probe.response. Launch it with:
#   /usr/bin/app_launcher -s org.tvauth.pkgprobe
#
# The request file must be readable by the app; on the TV:
#   chown owner:users /tmp/tvauth-probe.request
#   chsmack -a User::App::Shared /tmp/tvauth-probe.request
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
cd "$SCRIPT_DIR"
rm -rf out
mkdir -p out/pkgroot/bin out/pkgroot/shared/res

docker run --rm --platform linux/amd64 -v "$PWD:/src" -w /src \
    mcr.microsoft.com/dotnet/sdk:3.1 sh -eu -c '
        dotnet publish -c Release -o /src/out/publish
        cp /src/out/publish/PkgProbe.dll \
           /src/out/publish/PkgProbe.runtimeconfig.json \
           /src/out/publish/PkgProbe.deps.json /src/out/pkgroot/bin/
    '

cp tizen-manifest.xml out/pkgroot/
cp icon.png out/pkgroot/shared/res/
( cd out/pkgroot && zip -q -X -r ../org.tvauth.pkgprobe-0.1.0.tpk tizen-manifest.xml bin shared )

if [ -n "${TIZEN_PROFILE:-}" ]; then
    "$HOME/tizen-studio/tools/ide/bin/tizen" package -t tpk \
        -s "$TIZEN_PROFILE" -- out/org.tvauth.pkgprobe-0.1.0.tpk
else
    echo "unsigned: out/org.tvauth.pkgprobe-0.1.0.tpk"
    echo "set TIZEN_PROFILE (for example tvauth-partner) to sign it"
fi
