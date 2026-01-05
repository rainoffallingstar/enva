#!/bin/bash
# Multi-platform release build script for enva

set -e

echo "🚀 Starting enva multi-platform release build..."

# Create release directory
RELEASE_DIR="release-$(date +%Y%m%d-%H%M%S)"
mkdir -p "$RELEASE_DIR"

# Build for current platform (Windows)
echo "📦 Building for Windows x86_64..."
cargo build --release
cp target/release/enva.exe "$RELEASE_DIR/enva-windows-x86_64.exe"
echo "✅ Windows build completed"

# Note: Cross-compilation requires additional setup
# For Linux x86_64 (static binary):
# rustup target add x86_64-unknown-linux-musl
# cargo build --release --target x86_64-unknown-linux-musl

# For macOS Intel:
# rustup target add x86_64-apple-darwin
# cargo build --release --target x86_64-apple-darwin

# For macOS Apple Silicon:
# rustup target add aarch64-apple-darwin
# cargo build --release --target aarch64-apple-darwin

echo ""
echo "📊 Release Information:"
echo "======================"
echo "Version: 0.1.0"
echo "Build Date: $(date)"
echo "Platform: Windows x86_64"
echo "Binary Size: $(ls -lh "$RELEASE_DIR/enva-windows-x86_64.exe" | awk '{print $5}')"
echo ""
echo "📁 Release files created in: $RELEASE_DIR/"
ls -lh "$RELEASE_DIR/"
echo ""
echo "🎉 Release build completed successfully!"
