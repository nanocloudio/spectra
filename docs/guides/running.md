# Running Spectra

This guide brings up the smallest useful Spectra graph on a Linux host
and smoke checks it: the `codec` module decodes a BMP still and the
host display sink captures the decoded frame to disk as a PPM file. It
is self-contained — the input image is generated first, the deployment
config is embedded below and piped straight into `fluxor run` on
stdin, so there is nothing else to fetch and no screen is needed.

Source: `modules/app/codec/mod.rs` (the module the graph runs) and
`modules/app/codec/manifest.toml` (its ports).

## Prerequisites

Follow the store setup in the repository [README](../../README.md),
then build the modules:

```sh
fluxor modules build --all
```

## Run

The graph is three modules: `host_asset_source` streams the file's
bytes into the codec's `encoded` port, the codec sniffs the format
(BMP, from the `BM` magic) and decodes it, and `linux_display` in file
mode captures each frame arriving on `pixels` as a PPM under
`target/spectra_demo/`.

First generate a 320×240 gradient BMP to decode:

```sh
python3 - <<'EOF'
import struct
w, h = 320, 240
rows = []
for y in range(h - 1, -1, -1):
    row = bytes(c for x in range(w) for c in (y * 255 // h, x * 255 // w, 128))
    rows.append(row + b"\x00" * ((4 - len(row) % 4) % 4))
pixels = b"".join(rows)
header = struct.pack("<2sIHHI", b"BM", 54 + len(pixels), 0, 0, 54)
dib = struct.pack("<IiiHHIIiiII", 40, w, h, 1, 24, 0, len(pixels), 0, 0, 0, 0)
with open("/tmp/spectra_demo.bmp", "wb") as f:
    f.write(header + dib + pixels)
EOF
```

then run the graph, config on stdin:

```sh
fluxor run - <<'EOF'
target: linux
tick_us: 100

platform:
  display:
    driver: host
    mode: file
    path: ./target/spectra_demo/frame_%04d.ppm
    width: 320
    height: 240

modules:
  - name: asset
    type: host_asset_source
    path: /tmp/spectra_demo.bmp

  - name: codec
    width: 320
    height: 240

wiring:
  - from: asset.stream
    to: codec.encoded
    rate: video
  - from: codec.pixels
    to: display.pixels
    rate: video
EOF
```

`fluxor run` compiles the YAML into a binary config, gathers the
`codec.fmod` it names, and launches the `fluxor-linux` runtime on the
pair. Log lines similar to these mean the graph is live and the decode
happened:

```text
[... INFO fluxor_linux] [linux_display] mode=file 320x240 scale=1 (153600 bytes/frame) header=false path='./target/spectra_demo/frame_%04d.ppm'
[... INFO fluxor_linux] [inst] 3 of 3 modules loaded
[... INFO fluxor::kernel::module::syscalls] [dec] image
[... INFO fluxor::kernel::module::syscalls] [dec] bmp
[... INFO fluxor::kernel::module::syscalls] [img] decoded 320x240 -> 320x240
```

## Smoke check

From another terminal, confirm the decoded frame landed and is a
well-formed PPM at the configured raster:

```sh
ls target/spectra_demo/
# frame_0000.ppm
head -2 target/spectra_demo/frame_0000.ppm
# P6
# 320 240
```

The frame came from the running graph: the bytes entered through
`host_asset_source`, the codec detected BMP and decoded it to RGB565,
and `linux_display` expanded the pixels into the PPM.

Any BMP, GIF, PNG, or JPEG file works in `path:`; the codec sniffs the
format from the first bytes. The `width`/`height` parameters on the
codec set its output raster — a source at another size is scaled to
fit.

## Stopping

Ctrl+C in the terminal running `fluxor run` stops the runtime. Each
run is stateless; running the command again starts fresh (the frame
counter restarts at `frame_0000.ppm`).

## Other targets

The same module builds for rp2350, bcm2712, and wasm —
`fluxor modules build --all` produces all of them. Deploying to a
board or a browser is a fluxor concern; this guide's graph is the
Linux-host proof that the decode path works.
