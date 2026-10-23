# Bundled fonts

The desktop application embeds these fonts in the binary (`import "fonts/<file>.ttf";`
in `ui/app.slint`), so every package format ships them without a font install step.
All three families are licensed under the SIL Open Font License 1.1. The full text of
each, with its copyright line, is in `licenses/`, and `fonts.toml` lists every font file
so `tools/licenses/inventory.py` can add them to the third-party license inventory
(`THIRD-PARTY-LICENSES.md`, `ephervex licenses`).

Only static TTF files are bundled. The renderer ignores variable-font axes, so a
variable font would draw every weight at its default instance.

| Role | Family | Weights | Version |
| --- | --- | --- | --- |
| Headings | Pixelify Sans | 400, 600 | 1.000 |
| Interface text | Space Grotesk | 400, 500, 600, 700 | 2.000 |
| Logs, versions, paths | IBM Plex Mono | 400, 500, 600 | 2.005 |

## Pixelify Sans

- Upstream: https://github.com/eifetx/Pixelify-Sans, commit
  `39df74aba80df8157546034b878e8be1eb565ced`.
- Copyright 2021 The Pixelify Sans Project Authors
  (https://github.com/eifetx/Pixelify-Sans). No Reserved Font Name.
- Licenses and credits: `licenses/OFL-PixelifySans.txt`,
  `licenses/AUTHORS-PixelifySans.txt`, `licenses/CONTRIBUTORS-PixelifySans.txt`, each
  byte-identical to the upstream `OFL.txt`, `AUTHORS.txt`, and `CONTRIBUTORS.txt`.

| File | SHA-256 | Upstream file SHA-256 |
| --- | --- | --- |
| `PixelifySans-Regular.ttf` | `2fd99b2673568676c56cdb01264cd816d163ae1b614aafed3287d159f838d954` | `5436d485d1713afa376e1744911cbbcc0849eae1eb55ec9bc34c0753993ffef9` |
| `PixelifySans-SemiBold.ttf` | `f278a9ba213ba78970258433e219330fcc5d3e3dfaf1c16249b35364fde6326f` | `a4b54982991cd47450df317a451a7f066bf6b91aa033e105643aba2cf7bd35c3` |

Modification: the `liga` feature is removed, because the fi, fl, and ffi ligatures
render illegibly at interface sizes. The upstream fonts have no `clig`. Every other
layout feature (GSUB `aalt`, `case`, `ccmp`, `dlig`, `frac`, `locl`, `ordn`, `sinf`,
`subs`, `sups`; GPOS `kern`, `mark`, `mkmk`), all glyphs, and all names are kept. The
files are reproduced byte for byte from the upstream static files with fontTools 4.63.0.
The feature list is spelled out because `--layout-features-=` subtracts from
`pyftsubset`'s short default list rather than from every feature:

```sh
pyftsubset <upstream-file> --unicodes='*' --glyphs='*' \
    --layout-features=aalt,case,ccmp,dlig,frac,locl,ordn,sinf,subs,sups,kern,mark,mkmk \
    --name-IDs='*' --name-legacy --notdef-outline --glyph-names --legacy-kern --hinting \
    --output-file=<bundled-file>
```

The font declares no Reserved Font Name, so the modified files keep the original names.

## Space Grotesk

- Upstream: https://github.com/floriankarsten/space-grotesk, commit
  `03507d024a01282884232081fc6011c09ff4e849`.
- Copyright 2020 The Space Grotesk Project Authors
  (https://github.com/floriankarsten/space-grotesk). No Reserved Font Name.
- Licenses and credits: `licenses/OFL-SpaceGrotesk.txt`,
  `licenses/AUTHORS-SpaceGrotesk.txt`, `licenses/CONTRIBUTORS-SpaceGrotesk.txt`, each
  byte-identical to the upstream `OFL.txt`, `AUTHORS.txt`, and `CONTRIBUTORS.txt`.

| File | SHA-256 | Origin |
| --- | --- | --- |
| `SpaceGrotesk-Regular.ttf` | `5ede28c4425f3fe4830c8f4754b39e9a87a93d0c3baa5e0a9924532aaa8a98bd` | upstream static file, unmodified |
| `SpaceGrotesk-Medium.ttf` | `3183b7eb0b4241476360e2cd8e527868616cc16d3ce332c4376505989b772b44` | upstream static file, unmodified |
| `SpaceGrotesk-SemiBold.ttf` | `d542c59acf93ad3994097f83ec8e62c2d4a67502de188d9a6e74168ecd8ca1e1` | instanced, see below |
| `SpaceGrotesk-Bold.ttf` | `7209bbb75fc0f5c546a5f5773b0db74ffc6abf04c2148c1105cace5765a96bdb` | upstream static file, unmodified |

Modification: upstream ships no static SemiBold, so `SpaceGrotesk-SemiBold.ttf` is the
upstream variable font `SpaceGrotesk[wght].ttf` (SHA-256
`acad6de1fc93436f5c0f1f4137751ef04f1aea3063e7036535970ffcfbd79f72`, axis 300 to 700)
instanced at `wght=600` with fontTools 4.63.0, with its name table rewritten to the
family "Space Grotesk", style "SemiBold" (name IDs 1, 2, 3, 4, 6, 16, 17), weight class
600, and the regular style bits. Rebuilding gives the same tables except the `head`
modification timestamp, so the bytes are not reproducible, but the content is.

```python
from fontTools.ttLib import TTFont
from fontTools.varLib import instancer

font = TTFont("SpaceGrotesk[wght].ttf")
semibold = instancer.instantiateVariableFont(font, {"wght": 600}, inplace=False)
names = semibold["name"]
for record in list(names.names):
    if record.nameID in (1, 2, 3, 4, 6, 16, 17, 21, 22, 25):
        names.removeNames(nameID=record.nameID)
for platform, encoding, language in ((3, 1, 0x409), (1, 0, 0)):
    names.setName("Space Grotesk SemiBold", 1, platform, encoding, language)
    names.setName("Regular", 2, platform, encoding, language)
    names.setName("Version 2.000;SpaceGrotesk-SemiBold", 3, platform, encoding, language)
    names.setName("Space Grotesk SemiBold", 4, platform, encoding, language)
    names.setName("SpaceGrotesk-SemiBold", 6, platform, encoding, language)
    names.setName("Space Grotesk", 16, platform, encoding, language)
    names.setName("SemiBold", 17, platform, encoding, language)
semibold["OS/2"].usWeightClass = 600
semibold["OS/2"].fsSelection = (semibold["OS/2"].fsSelection & ~0b100001) | 0b1000000
semibold["head"].macStyle = 0
semibold.save("SpaceGrotesk-SemiBold.ttf")
```

## IBM Plex Mono

- Upstream: https://github.com/IBM/plex, release `@ibm/plex-mono@2.5.0`
  (`ibm-plex-mono.zip`, SHA-256
  `6d23f01257663d8cc49a0d64c22ced630b79e0e2a0ac08a0da86e9a38bbc481c`), files from
  `ibm-plex-mono/fonts/complete/ttf/`. The font files report version 2.005.
- Copyright (c) 2017 IBM Corp. with Reserved Font Name "Plex".
- License: `licenses/OFL-IBMPlexMono.txt` is the `LICENSE.txt` of the `@ibm/plex-mono`
  2.5.0 package from the same release (the font files are byte-identical to the zip's),
  byte for byte, CRLF line endings and the copyright sign included. The zip's own
  `license.txt` carries the same terms, differing only in line wrapping and line endings.

| File | SHA-256 |
| --- | --- |
| `IBMPlexMono-Regular.ttf` | `7c6fbddca4b700be918f5f6183d9bd4464fa427fe435f0b480d77fe2bb8c5a43` |
| `IBMPlexMono-Medium.ttf` | `98fbd727aae340b236955879dabed4d991aac9e8e90b3b2a67ce4a59221cc97c` |
| `IBMPlexMono-SemiBold.ttf` | `f04d7c488ddf7d1fa99f2574efc3406ea4cbe17bb1af3a1ab960f84d0c96a172` |

These files are unmodified. The font declares the Reserved Font Name "Plex", which the
OFL forbids a modified version from using, so the files are never subset, renamed, or
otherwise changed. A change to them needs a different family name.
