# Fonts

Three families, all under the SIL Open Font License 1.1. Each was downloaded
from the `google/fonts` repository on 2026-09-16 and subsetted to Latin plus
the box-drawing and arrow characters this page uses, with `pyftsubset
--flavor=woff2`. The variable axes are preserved.

| File | Family | Axes | License |
|---|---|---|---|
| `Newsreader.woff2` | Newsreader | `opsz` 6-72, `wght` 200-800 | `OFL-Newsreader.txt` |
| `Newsreader-Italic.woff2` | Newsreader Italic | `opsz` 6-72, `wght` 200-800 | `OFL-Newsreader.txt` |
| `Archivo.woff2` | Archivo | `wdth` 62-125, `wght` 100-900 | `OFL-Archivo.txt` |
| `JetBrainsMono.woff2` | JetBrains Mono | `wght` 100-800 | `OFL-JetBrainsMono.txt` |

Newsreader is by Production Type. Archivo is by Omnibus-Type. JetBrains Mono is
by JetBrains. The OFL requires that these copyright and license notices travel
with the font files, which is what the `OFL-*.txt` files here are for.

To regenerate, subset the upstream variable TTFs with the feature set
`kern,liga,clig,calt,tnum,case,frac,sups,onum,smcp,c2sc`.
