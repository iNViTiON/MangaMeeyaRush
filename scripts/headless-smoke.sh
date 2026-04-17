#!/usr/bin/env bash
# Launch mmce under xvfb for 3 seconds against a generated fixture to catch
# runtime crashes without needing a real display. Exit non-zero iff the
# process crashed; a 3-second timeout is treated as success.
set -uo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
fixture="$root/target/smoke-fixture"
mkdir -p "$fixture"

# Produce fixture PNGs via a tiny side crate (only the first run compiles).
gendir="$root/target/smoke-gen"
mkdir -p "$gendir/src"
cat > "$gendir/Cargo.toml" << EOF
[package]
name = "mmce-smoke-gen"
version = "0.0.1"
edition = "2021"
[dependencies]
image = "0.25"
[workspace]
EOF
cat > "$gendir/src/main.rs" << 'EOF'
use image::{ImageBuffer, Rgb};
fn main() {
    let out = std::env::args().nth(1).unwrap();
    for i in 1..=8u32 {
        let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(600, 900, |x, y| {
            Rgb([((x + i * 32) % 256) as u8, ((y + i * 17) % 256) as u8, (i as u8) * 31])
        });
        img.save(format!("{out}/page_{i:02}.png")).unwrap();
    }
}
EOF
(cd "$gendir" && cargo run --quiet --release -- "$fixture") >/dev/null 2>&1

# Run under Xvfb for 3 seconds; timeout-induced kill is success.
xvfb-run -a --server-args='-screen 0 1280x800x24' \
    timeout --preserve-status 3s "$root/target/release/mmce" "$fixture"
rc=$?
# timeout returns 124 or 143 when killed; treat both as healthy boot.
if [[ $rc -eq 124 || $rc -eq 143 || $rc -eq 0 ]]; then
    echo "smoke: OK (mmce ran $rc)"
    exit 0
fi
echo "smoke: FAIL (exit $rc)"
exit $rc
