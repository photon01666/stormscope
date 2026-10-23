# Third-party licenses

Stormscope itself is [MIT](../LICENSE). Its dependency tree is entirely
permissive (MIT / Apache-2.0 family); every crate is listed below with the
license expression it declares in its own manifest. Dual-licensed crates
are used under a permissive option (for example `self_cell` under
Apache-2.0, `r-efi` under MIT).

Generated from `cargo metadata`: 502 crates, 35 distinct
license expressions. Regenerate with:

```sh
python3 scripts/gen-third-party.py    # writes docs/THIRD-PARTY.md
```

| License expression | Crates | Examples |
| --- | ---: | --- |
| `MIT OR Apache-2.0` | 244 | `anyhow 1.0.104`, `chrono 0.4.45`, `chrono-tz 0.10.4`, `eframe 0.36.2` … |
| `MIT` | 105 | `lru 0.12.5`, `lru 0.16.4`, `nexrad-data 1.0.0-rc.7`, `nexrad-decode 1.0.0-rc.3` … |
| `Apache-2.0 OR MIT` | 30 | `atomic-waker 1.1.2`, `autocfg 1.5.1`, `bit-set 0.10.0` |
| `Unicode-3.0` | 18 | `icu_collections 2.3.0`, `icu_locale_core 2.3.0`, `icu_normalizer 2.3.0` |
| `MIT/Apache-2.0` | 15 | `bitflags 1.3.2`, `downcast-rs 1.2.1`, `foreign-types 0.5.0` |
| `Apache-2.0` | 12 | `winit 0.30.13` |
| `Zlib OR Apache-2.0 OR MIT` | 11 | `bytemuck 1.25.2`, `bytemuck_derive 1.12.1`, `dispatch2 0.3.1` |
| `Unlicense OR MIT` | 10 | `aho-corasick 1.1.5`, `byteorder-lite 0.1.0`, `jiff 0.2.38` |
| `MIT OR Apache-2.0 OR Zlib` | 7 | `cursor-icon 1.2.0`, `glow 0.17.0`, `lru-slab 0.1.3` |
| `Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT` | 7 | `linux-raw-sys 0.12.1`, `linux-raw-sys 0.4.15`, `rustix 0.38.44` |
| `ISC` | 4 | `earcutr 0.5.0` |
| `Zlib` | 4 | `foldhash 0.1.5`, `foldhash 0.2.0`, `slotmap 1.1.1` |
| `Apache-2.0 OR ISC OR MIT` | 3 | `rustls 0.23.45` |
| `Apache-2.0/MIT` | 3 | `pollster 0.4.0`, `pollster 1.0.1`, `rustc-hash 1.1.0` |
| `BSL-1.0` | 2 | `clipboard-win 5.4.1`, `error-code 3.4.0` |
| `MIT OR Zlib OR Apache-2.0` | 2 | `miniz_oxide 0.8.9`, `miniz_oxide 0.9.1` |
| `BSD-3-Clause OR Apache-2.0` | 2 | `moxcms 0.8.1`, `pxfm 0.1.30` |
| `BSD-3-Clause OR MIT OR Apache-2.0` | 2 | `num_enum 0.7.6`, `num_enum_derive 0.7.6` |
| `MIT OR Apache-2.0 OR LGPL-2.1-or-later` | 2 | `r-efi 5.3.0`, `r-efi 6.0.0` |
| `Unlicense/MIT` | 2 | `same-file 1.0.6`, `walkdir 2.5.0` |
| `CDLA-Permissive-2.0` | 2 | `webpki-root-certs 1.0.9`, `webpki-roots 1.0.9` |
| `BSD-2-Clause OR Apache-2.0 OR MIT` | 2 | `zerocopy 0.8.62`, `zerocopy-derive 0.8.62` |
| `0BSD OR MIT OR Apache-2.0` | 1 | `adler2 2.0.1` |
| `ISC AND (Apache-2.0 OR ISC)` | 1 | `aws-lc-rs 1.18.1` |
| `ISC AND (Apache-2.0 OR ISC) AND Apache-2.0 AND MIT AND BSD-3-Clause AND (Apache-2.0 OR ISC OR MIT) AND (Apache-2.0 OR ISC OR MIT-0)` | 1 | `aws-lc-sys 0.45.0` |
| `MIT / Apache-2.0` | 1 | `cgl 0.3.2` |
| `Apache-2.0 AND MIT` | 1 | `dpi 0.1.2` |
| `CC0-1.0 OR MIT-0 OR Apache-2.0` | 1 | `dunce 1.0.5` |
| `(MIT OR Apache-2.0) AND OFL-1.1 AND Ubuntu-font-1.0` | 1 | `epaint_default_fonts 0.36.2` |
| `bzip2-1.0.6` | 1 | `libbz2-rs-sys 0.2.5` |
| `Apache-2.0 AND ISC` | 1 | `ring 0.17.14` |
| `Apache-2.0 OR BSL-1.0` | 1 | `ryu 1.0.23` |
| `Apache-2.0 OR GPL-2.0-only` | 1 | `self_cell 1.3.0` |
| `BSD-3-Clause` | 1 | `subtle 2.6.1` |
| `(MIT OR Apache-2.0) AND Unicode-3.0` | 1 | `unicode-ident 1.0.26` |

Notes for redistributors:

- Apache-2.0 requires preserving the license text and any NOTICE files of
  the crates you redistribute. If you need a full bundle with license
  texts, `cargo install cargo-about && cargo about generate about.hbs`
  produces one from the same metadata.
- Radar data, alerts, outlooks and basemap tiles are served by public
  providers under their own terms — see the README attribution table.

