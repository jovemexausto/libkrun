#!/bin/bash
# Capivara SP-2a: build do libkrun com feature gpu-gfxstream no macOS ARM64
#
# Uso:
#   GFXSTREAM_BUILD=/path/to/capivara-sp1/gfxstream-patched/build-macos/host \
#     ./build-sp2a-macos.sh

set -e
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

echo "╔══════════════════════════════════════════════╗"
echo "║  Capivara SP-2a: libkrun + gfxstream macOS   ║"
echo "╚══════════════════════════════════════════════╝"
echo ""

# ── GFXSTREAM_PATH ─────────────────────────────────
if [ -z "$GFXSTREAM_BUILD" ]; then
    echo "✗ GFXSTREAM_BUILD não definido."
    echo "  export GFXSTREAM_BUILD=/path/to/capivara-sp1/gfxstream-patched/build-macos/host"
    exit 1
fi

DYLIB="$GFXSTREAM_BUILD/libgfxstream_backend.0.dylib"
if [ ! -f "$DYLIB" ]; then
    echo "✗ libgfxstream_backend.0.dylib não encontrado em: $GFXSTREAM_BUILD"
    exit 1
fi

echo "→ Usando libgfxstream_backend em: $GFXSTREAM_BUILD"
echo "  $(ls -lh "$DYLIB" | awk '{print $5, $9}')"
echo ""

export GFXSTREAM_PATH="$GFXSTREAM_BUILD"

if ! command -v cargo &>/dev/null; then
    echo "✗ cargo não encontrado. Instale Rust: https://rustup.rs"; exit 1
fi
echo "→ $(cargo --version)"
echo ""

# ── Compilar do root da workspace ──────────────────
cd "$SCRIPT_DIR"

echo "→ Compilando krun-rutabaga-gfx com feature gfxstream..."
cargo build \
    -p krun-rutabaga-gfx \
    --features krun-rutabaga-gfx/gfxstream \
    2>&1 | tee /tmp/sp2a-rutabaga.log

if [ ${PIPESTATUS[0]} -ne 0 ]; then
    echo "✗ Build do krun-rutabaga-gfx falhou."
    grep "^error" /tmp/sp2a-rutabaga.log | head -20; exit 1
fi
echo "✓ krun-rutabaga-gfx com gfxstream OK"
echo ""

echo "→ Compilando libkrun com feature gpu-gfxstream..."
cargo build \
    -p libkrun \
    --features libkrun/gpu-gfxstream \
    2>&1 | tee /tmp/sp2a-libkrun.log

if [ ${PIPESTATUS[0]} -ne 0 ]; then
    echo "✗ Build do libkrun falhou."
    grep "^error" /tmp/sp2a-libkrun.log | head -20; exit 1
fi

echo ""
echo "╔══════════════════════════════════════════════╗"
echo "║  ✓  SP-2a CONCLUÍDO                          ║"
echo "╚══════════════════════════════════════════════╝"
echo ""
echo "Artefatos:"
find target/debug \( -name "libkrun*.dylib" -o -name "libkrun*.a" \) 2>/dev/null | \
    while read f; do echo "  $(du -sh "$f" | cut -f1)  $f"; done
echo ""
echo "Próximo passo: SP-2b — capivara-vm"
