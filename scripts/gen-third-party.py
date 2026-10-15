#!/usr/bin/env python3
"""Generate docs/THIRD-PARTY.md from `cargo metadata` (run in the repo root)."""
import collections
import json
import subprocess

meta = json.loads(subprocess.run(
    ["cargo", "metadata", "--format-version", "1"],
    capture_output=True, text=True, check=True).stdout)

by = collections.defaultdict(list)
for p in meta["packages"]:
    lic = p.get("license") or "(declared per crate)"
    by[lic].append(f"{p['name']} {p['version']}")

total = len(meta["packages"])
notable = {
    "egui", "eframe", "wgpu", "winit", "reqwest", "tokio", "image", "serde",
    "chrono", "nexrad-data", "nexrad-decode", "lru", "earcutr", "libc",
    "rustls", "log", "env_logger", "anyhow", "thiserror", "serde_json",
    "chrono-tz",
}

out = []
out.append("# Third-party licenses")
out.append("")
out.append("stormscope itself is [MIT](../LICENSE). Its dependency tree is entirely")
out.append("permissive (MIT / Apache-2.0 family); every crate is listed below with the")
out.append("license expression it declares in its own manifest. Dual-licensed crates")
out.append("are used under a permissive option (for example `self_cell` under")
out.append("Apache-2.0, `r-efi` under MIT).")
out.append("")
out.append(f"Generated from `cargo metadata`: {total} crates, {len(by)} distinct")
out.append("license expressions. Regenerate with:")
out.append("")
out.append("```sh")
out.append("python3 scripts/gen-third-party.py    # writes docs/THIRD-PARTY.md")
out.append("```")
out.append("")
out.append("| License expression | Crates | Examples |")
out.append("| --- | ---: | --- |")
for lic in sorted(by, key=lambda k: -len(by[k])):
    crates = sorted(by[lic])
    picks = [c for c in crates if c.split()[0] in notable] or crates[:3]
    cell = ", ".join(f"`{c}`" for c in picks[:4]) + (" …" if len(picks) > 4 else "")
    out.append(f"| `{lic}` | {len(crates)} | {cell} |")
out.append("")
out.append("Notes for redistributors:")
out.append("")
out.append("- Apache-2.0 requires preserving the license text and any NOTICE files of")
out.append("  the crates you redistribute. If you need a full bundle with license")
out.append("  texts, `cargo install cargo-about && cargo about generate about.hbs`")
out.append("  produces one from the same metadata.")
out.append("- Radar data, alerts, outlooks and basemap tiles are served by public")
out.append("  providers under their own terms — see the README attribution table.")
out.append("")
open("docs/THIRD-PARTY.md", "w").write("\n".join(out) + "\n")
print(f"wrote docs/THIRD-PARTY.md ({total} crates, {len(by)} expressions)")
