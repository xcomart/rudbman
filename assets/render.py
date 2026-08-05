#!/usr/bin/env python3
"""Rasterise `assets/icon.svg` into every format the packaging needs.

Usage
-----
    python3 assets/render.py

With no arguments this regenerates the whole set from the master SVG:

    assets/icon-128.png   assets/icon-256.png   assets/icon.ico   assets/icon.icns

and then re-opens each result and prints what is actually in it. That is the
only supported way to change the raster files — edit `assets/icon.svg` and run
this. Editing a PNG, the `.ico` or the `.icns` by hand puts them out of step
with the master and with each other.

For one-off renders of some other SVG (a mark being tried out, the title bar
glyph) pass files and sizes explicitly:

    python3 assets/render.py path/to/mark.svg --sizes 16 32 256 [--outdir DIR]

Why GdkPixbuf and not something more obvious
--------------------------------------------
The usual command line rasterisers are not installed here and are not worth
adding as a build dependency: there is no ``rsvg-convert``, no ``inkscape``,
and ``cairosvg`` is not importable. What *is* present, because the application
is a GTK program and the toolkit is already a hard dependency of this
repository, is GdkPixbuf with its SVG loader (librsvg behind the same
interface). Rendering through it costs nothing extra to install and gives the
same librsvg output the desktop itself produces when it draws a scalable icon,
which is exactly the renderer whose result we care about matching.

Every size is drawn from the vector
-----------------------------------
``Pixbuf.new_from_file_at_size`` re-renders the vector at the requested pixel
size instead of scaling a large bitmap down, and every size in every container
below is produced that way — the 16 px frame in the `.ico` is a 16 px drawing,
not a shrunk 256. The difference is plain at that size: a downscaled 256 turns
the cylinder's grooves into grey haze across the barrel, while a fresh 16 px
render drops them and keeps the silhouette clean. Pillow would happily resample
for us in both container writers, so each one is handed a complete set of
exact-size frames through ``append_images`` and never gets the chance.
"""

from __future__ import annotations

import argparse
import io
import sys
from pathlib import Path

import gi
from PIL import Image

gi.require_version("GdkPixbuf", "2.0")
from gi.repository import GdkPixbuf  # noqa: E402  (must follow require_version)

ASSETS = Path(__file__).resolve().parent
MASTER = ASSETS / "icon.svg"

#: Standalone PNGs. These are the sizes the Linux `.desktop` entry installs;
#: see `packaging/`.
PNG_SIZES = (128, 256)

#: Frames in the Windows `.ico`. Explorer, the taskbar, the window title bar
#: and Alt-Tab each pick a different one out of the same file, so leaving a
#: size out means Windows scales a neighbouring frame and shows its artefacts.
#: `crates/rudbman-app/build.rs` embeds this file as resource ID 1.
ICO_SIZES = (16, 24, 32, 48, 64, 128, 256)

#: Frames in the macOS `.icns`. There is no 16 px entry in the container's
#: chunk table: its smallest type, `ic11`, is the 16 pt slot at 2x, which is
#: 32 px. macOS derives the 1x 16 px image from that itself.
ICNS_SIZES = (32, 64, 128, 256, 512, 1024)


def rasterise(svg: Path, size: int) -> Image.Image:
    """Render *svg* at *size*x*size* pixels and return it as a Pillow image."""
    pixbuf = GdkPixbuf.Pixbuf.new_from_file_at_size(str(svg), size, size)
    if pixbuf is None:  # loader failure normally raises, but be certain
        raise RuntimeError(f"the SVG loader returned nothing for {svg}")
    # Going through an in-memory PNG rather than `get_pixels()` avoids having
    # to reconcile the pixbuf's row stride with Pillow's, which differ whenever
    # the width is not a multiple of four.
    ok, buf = pixbuf.save_to_bufferv("png", [], [])
    if not ok:
        raise RuntimeError(f"could not encode the {size} px render of {svg}")
    image = Image.open(io.BytesIO(buf))
    image.load()
    return image.convert("RGBA")


def render(svg: Path, size: int, outdir: Path | None = None) -> Path:
    """Render *svg* at *size* and write `<stem>-<size>.png`; return that path."""
    target = (outdir or svg.parent) / f"{svg.stem}-{size}.png"
    target.parent.mkdir(parents=True, exist_ok=True)
    rasterise(svg, size).save(target, "png")
    return target


def build_all(svg: Path = MASTER, outdir: Path | None = None) -> list[Path]:
    """Produce the full shipped set from *svg*. Returns the files written."""
    out = outdir or svg.parent
    out.mkdir(parents=True, exist_ok=True)
    written: list[Path] = []

    for size in PNG_SIZES:
        target = out / f"icon-{size}.png"
        rasterise(svg, size).save(target, "png")
        written.append(target)

    # Pillow's ICO writer looks through `[im] + append_images` for a frame
    # whose size matches each requested one and only resamples when it finds
    # none, so handing it every size means it resamples nothing. The base image
    # has to be the largest, because the writer skips any requested size larger
    # than the base.
    largest = max(ICO_SIZES)
    ico_frames = {size: rasterise(svg, size) for size in ICO_SIZES}
    ico = out / "icon.ico"
    ico_frames[largest].save(
        ico,
        "ico",
        sizes=[(s, s) for s in ICO_SIZES],
        append_images=[ico_frames[s] for s in ICO_SIZES if s != largest],
    )
    written.append(ico)

    # The ICNS writer keys `append_images` by width and resizes only for a
    # width it was not given, so the same rule applies: supply them all.
    icns_frames = [rasterise(svg, size) for size in ICNS_SIZES]
    icns = out / "icon.icns"
    icns_frames[-1].save(icns, "icns", append_images=icns_frames)
    written.append(icns)

    return written


def verify(paths: list[Path]) -> None:
    """Re-open each generated file and print what it really contains."""
    for path in paths:
        with Image.open(path) as im:
            fmt = im.format
            if fmt == "ICO":
                sizes = sorted(im.ico.sizes())
                detail = f"{len(sizes)} frames: " + ", ".join(
                    f"{w}x{h}" for w, h in sizes
                )
            elif fmt == "ICNS":
                sizes = sorted({w * scale for w, _, scale in im.info["sizes"]})
                detail = f"{len(sizes)} sizes: " + ", ".join(str(s) for s in sizes)
            else:
                detail = f"{im.size[0]}x{im.size[1]} {im.mode}"
        print(f"  {path.name:<14} {fmt:<5} {detail}  ({path.stat().st_size} bytes)")


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(
        description="Rasterise the application icon from its master SVG."
    )
    parser.add_argument(
        "svg", nargs="*", type=Path, help="SVGs to render (default: the full set)"
    )
    parser.add_argument(
        "--sizes", nargs="+", type=int, help="pixel sizes, for one-off renders"
    )
    parser.add_argument(
        "--outdir", type=Path, default=None, help="write here instead of alongside"
    )
    args = parser.parse_args(argv)

    if not args.svg and not args.sizes:
        if not MASTER.is_file():
            parser.error(f"no master SVG at {MASTER}")
        written = build_all(MASTER, args.outdir)
        print(f"regenerated from {MASTER.name}:")
        verify(written)
        return 0

    svgs = args.svg or [MASTER]
    sizes = args.sizes or list(PNG_SIZES)
    for svg in svgs:
        if not svg.is_file():
            parser.error(f"no such file: {svg}")
        for size in sizes:
            print(render(svg, size, args.outdir))
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
