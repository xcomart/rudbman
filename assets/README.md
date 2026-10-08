# Application icons

`icon.svg` is the master. Everything else in this directory is generated from
it and should never be edited by hand.

The mark is a database cylinder with a gold cap on a dark One Dark tile. One
subject, filling the tile, because 16 px — a taskbar, a window list — is the
size an app icon is mostly seen at and there is room for one shape there. The
body is the app's `accent` blue and the cap is its `grid_pk` gold, both from
`rugpui`'s `src/theme.rs`; the reasoning for those two colours, and for
not using the palette's green, is in the comment at the top of `icon.svg`.

## Regenerating

```sh
python3 assets/render.py
```

With no arguments this redraws `icon-128.png`, `icon-256.png`, `icon.ico` and
`icon.icns` from `icon.svg`, then re-opens each one and prints what it actually
contains. Run it after any edit to the master.

Every size in every file is rendered from the vector at that size, not scaled
down from a larger bitmap — the difference is visible at 16 px, where a
downscaled 256 smears the cylinder's grooves into haze. The script rasterises
through GdkPixbuf's SVG loader; its module docstring says why that one.

## What each file is for

| File | Used by |
|---|---|
| `icon.svg` | The master. Edit this one |
| `icon-128.png`, `icon-256.png` | Linux `.desktop` icon sizes (see `packaging/`); the title bar also embeds `icon-128.png` |
| `icon.ico` | Embedded into the Windows executable by `crates/rudbman-app/build.rs` under resource ID 1. Seven sizes, 16–256: Explorer, the taskbar, the caption and Alt-Tab each pick a different one |
| `icon.icns` | The macOS `.app` bundle. Six sizes, 32–1024; the container has no 16 px entry, its smallest slot being 16 pt at 2× |
| `render.py` | The generator |

## The title bar uses the generated PNG

The self-drawn title bar shows this mark too: `crates/rudbman-app/src/icons.rs`
embeds `icon-128.png` under the asset path `icons/app-icon.png`, and the bar
uses gpui's `img` element on every platform to preserve its colours. A tintable
`svg` element would turn the filled tile into a solid silhouette. Inactive
windows dim the complete image without losing its internal detail.

There is no second drawing to maintain: edit `icon.svg`, run `render.py` to
refresh the shipped images, and rebuild the application.

An earlier bar drew a monochrome outline stand-in
(`crates/rudbman-app/assets/icons/logo.svg`, since removed): the tile was then
a near-flat dark swatch that vanished against dark chrome. The plate's
gradient, its ring and its embossed edge are what let the real icon take the
stand-in's place.
