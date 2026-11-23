#!/bin/bash
# Linux x64 で、libdbgshim.so から行ブレークポイントの停止までを一本通す。
set -euo pipefail
cd "$(dirname "$0")"
ROOT="$(pwd)"

VER=10.0.750501
SHA=661698f7be43162f50b465576e059585713f6e715c6f2d5b9ba5e255e9a8deba
SHIM="$ROOT/.tools/libdbgshim.so"
if [[ ! -f "$SHIM" ]]; then
  mkdir -p "$ROOT/.tools"
  TMP="$(mktemp -d)"
  curl -fsSL -o "$TMP/pkg.nupkg" \
    "https://api.nuget.org/v3-flatcontainer/microsoft.diagnostics.dbgshim.linux-x64/${VER}/microsoft.diagnostics.dbgshim.linux-x64.${VER}.nupkg"
  python3 - "$TMP/pkg.nupkg" "$TMP" <<'PY'
import sys, zipfile
zipfile.ZipFile(sys.argv[1]).extract("runtimes/linux-x64/native/libdbgshim.so", sys.argv[2])
PY
  cp "$TMP/runtimes/linux-x64/native/libdbgshim.so" "$SHIM"
  rm -rf "$TMP"
fi
echo "$SHA  $SHIM" | sha256sum -c -

export DOTNET_CLI_TELEMETRY_OPTOUT=1
export DOTNET_NOLOGO=1
# libicu の無い環境でもホストが起動できるようにする。
export DOTNET_SYSTEM_GLOBALIZATION_INVARIANT=1
cargo test --manifest-path "$ROOT/Cargo.toml"
dotnet build "$ROOT/app/Spike.csproj" -c Release --nologo
DLL="$ROOT/app/bin/Release/net8.0/Spike.dll"
cargo build --release --manifest-path "$ROOT/Cargo.toml"
"$ROOT/target/release/spike" --shim "$SHIM" --dotnet "$(command -v dotnet)" --dll "$DLL"
