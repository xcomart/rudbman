# Application icons — placeholders

**Every file in this directory is logman's application icon, copied verbatim as
a temporary placeholder. None of it is rudbman's artwork.**

`icon.svg` draws a shell prompt chevron and a block cursor on a dark tile: it is
the mark of a terminal, not of a database tool. It is here so that the shell has
*something* to embed in a Windows executable, hand to a `.desktop` entry and put
in a macOS bundle while the real mark is being drawn. Replace all of it — the
SVG and everything rasterised from it — before the first release.

| File | Used by |
|---|---|
| `icon.svg` | The source the raster files were produced from |
| `icon-128.png`, `icon-256.png` | Linux `.desktop` icon sizes (see `packaging/`) |
| `icon.ico` | Embedded into the Windows executable by `crates/rudbman-app/build.rs` under resource ID 1 |
| `icon.icns` | The macOS `.app` bundle |

`crates/rudbman-app/assets/icons/logo.svg` is the same placeholder mark again,
in the monochrome form the self-drawn title bar tints from the theme. It has to
be redrawn together with the files here, or the title bar and the desktop icon
will stop agreeing.
