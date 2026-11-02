#!/usr/bin/env python3
"""Build the compact US county-boundary asset used by the map.

Input (download once, outside the repo):

  * US counties GeoJSON (Plotly's copy of the 2010 Census cartographic
    boundary file, public domain):
      https://raw.githubusercontent.com/plotly/datasets/master/geojson-counties-fips.json

Output (little-endian binary loaded by `src/geodata.rs`):

  assets/counties.bin  "SSCY" + u32 ring_count + rings of f32 (lon, lat)

The basemap already labels counties and cities, so this asset carries only
the geometry; the app draws the outline overlay and leaves names to the
tiles.
"""

from __future__ import annotations

import argparse
import json
import struct
from pathlib import Path

COUNTY_MAGIC = b"SSCY"
VERSION = 1


def write_counties(src: Path, out: Path) -> None:
    data = json.loads(src.read_text())
    rings: list[list[tuple[float, float]]] = []
    for feature in data["features"]:
        geometry = feature.get("geometry") or {}
        kind = geometry.get("type")
        if kind == "Polygon":
            polygons = [geometry["coordinates"]]
        elif kind == "MultiPolygon":
            polygons = geometry["coordinates"]
        else:
            continue
        for polygon in polygons:
            for ring in polygon:
                points = [(float(p[0]), float(p[1])) for p in ring if len(p) >= 2]
                if len(points) >= 3:
                    rings.append(points)

    with out.open("wb") as fh:
        fh.write(COUNTY_MAGIC)
        fh.write(struct.pack("<II", VERSION, len(rings)))
        for points in rings:
            fh.write(struct.pack("<I", len(points)))
            fh.write(struct.pack(f"<{len(points) * 2}f", *(v for p in points for v in p)))
    total = sum(len(r) for r in rings)
    print(
        f"counties: {len(rings)} rings, {total} points, "
        f"{out.stat().st_size / 1e6:.2f} MB"
    )


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--counties", type=Path, required=True)
    parser.add_argument(
        "--out", type=Path, default=Path("crates/stormscope-app/assets")
    )
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    write_counties(args.counties, args.out / "counties.bin")


if __name__ == "__main__":
    main()
