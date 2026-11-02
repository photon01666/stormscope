#!/usr/bin/env python3
"""Build the compact US county-boundary asset used by the map.

Input (download once, outside the repo):

  * US counties GeoJSON (Plotly's copy of the 2010 Census cartographic
    boundary file, public domain):
      https://raw.githubusercontent.com/plotly/datasets/master/geojson-counties-fips.json

Output (little-endian binary loaded by `src/geodata.rs`):

  assets/counties.bin  "SSCY" v3 + u32 county_count + counties:
                       { [6] NWS county-zone code, u32 ring_count,
                         rings of f32 (lon, lat) }

The basemap already labels counties and cities, so this asset carries only
the geometry; the app draws the outline overlay and leaves names to the
tiles. Each county keeps its NWS zone code (e.g. "OKC003") so zone-based
watches can be filled from the bundled geometry instead of one API request
per county.
"""

from __future__ import annotations

import argparse
import json
import struct
from pathlib import Path

COUNTY_MAGIC = b"SSCY"
VERSION = 3

# NWS county-zone codes use the USPS state abbreviation plus "C" and the
# three-digit county FIPS (OK + C + 003). The Census file only carries the
# numeric state FIPS, so map it once here.
STATE_FIPS = {
    "01": "AL", "02": "AK", "04": "AZ", "05": "AR", "06": "CA", "08": "CO",
    "09": "CT", "10": "DE", "11": "DC", "12": "FL", "13": "GA", "15": "HI",
    "16": "ID", "17": "IL", "18": "IN", "19": "IA", "20": "KS", "21": "KY",
    "22": "LA", "23": "ME", "24": "MD", "25": "MA", "26": "MI", "27": "MN",
    "28": "MS", "29": "MO", "30": "MT", "31": "NE", "32": "NV", "33": "NH",
    "34": "NJ", "35": "NM", "36": "NY", "37": "NC", "38": "ND", "39": "OH",
    "40": "OK", "41": "OR", "42": "PA", "44": "RI", "45": "SC", "46": "SD",
    "47": "TN", "48": "TX", "49": "UT", "50": "VT", "51": "VA", "53": "WA",
    "54": "WV", "55": "WI", "56": "WY", "60": "AS", "66": "GU", "69": "MP",
    "72": "PR", "78": "VI",
}


def write_counties(src: Path, out: Path) -> None:
    data = json.loads(src.read_text())
    counties: list[tuple[str, list[list[tuple[float, float]]]]] = []
    for feature in data["features"]:
        props = feature.get("properties") or {}
        state = str(props.get("STATE") or "").zfill(2)
        county = str(props.get("COUNTY") or "").zfill(3)
        abbr = STATE_FIPS.get(state)
        if not abbr or len(county) != 3:
            continue
        zone = f"{abbr}C{county}"
        geometry = feature.get("geometry") or {}
        kind = geometry.get("type")
        if kind == "Polygon":
            polygons = [geometry["coordinates"]]
        elif kind == "MultiPolygon":
            polygons = geometry["coordinates"]
        else:
            continue
        rings: list[list[tuple[float, float]]] = []
        for polygon in polygons:
            for ring in polygon:
                points = [(float(p[0]), float(p[1])) for p in ring if len(p) >= 2]
                if len(points) >= 3:
                    rings.append(points)
        if rings:
            counties.append((zone, rings))

    with out.open("wb") as fh:
        fh.write(COUNTY_MAGIC)
        fh.write(struct.pack("<II", VERSION, len(counties)))
        for zone, rings in counties:
            fh.write(zone.encode("ascii"))
            fh.write(struct.pack("<I", len(rings)))
            for points in rings:
                fh.write(struct.pack("<I", len(points)))
                fh.write(struct.pack(f"<{len(points) * 2}f", *(v for p in points for v in p)))
    total = sum(len(r) for _, rings in counties for r in rings)
    print(
        f"counties: {len(counties)} counties, {total} points, "
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
