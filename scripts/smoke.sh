#!/usr/bin/env bash
# Launch the viewer against a generated fixture folder.
# Uses the nix dev shell so runtime libs resolve.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
fixture="$root/target/smoke-fixture"
mkdir -p "$fixture"

cat > "$fixture/.genrs" << 'EOF'
use image::{ImageBuffer, Rgb};
fn main() {
    let out = std::env::args().nth(1).expect("out dir");
    for i in 1..=8 {
        let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(600, 900, |x, y| {
            let r = ((x + i * 32) % 256) as u8;
            let g = ((y + i * 17) % 256) as u8;
            let b = (i as u8) * 31;
            Rgb([r, g, b])
        });
        let name = format!("{}/page_{:02}.png", out, i);
        img.save(&name).unwrap();
    }
    println!("generated {} pages", 8);
}
EOF

# Small throwaway crate to produce the fixture PNGs.
gendir="$root/target/smoke-gen"
mkdir -p "$gendir/src"
cp "$fixture/.genrs" "$gendir/src/main.rs"
cat > "$gendir/Cargo.toml" << EOF
[package]
name = "mmce-smoke-gen"
version = "0.0.1"
edition = "2021"
[dependencies]
image = "0.25"
EOF

(cd "$gendir" && cargo run --quiet --release -- "$fixture") 1>&2

exec "$root/target/release/mmce" "$fixture"
