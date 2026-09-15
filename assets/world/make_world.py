#!/usr/bin/env python3
"""Bake the world map the GeoJSON viewer draws under its data: world.bin.

Everything is vector geometry from Natural Earth 1:10m (public domain), so the
map stays a clean line at any zoom rather than a raster that turns to blocks:

    land      polygons — continents and islands, filled; their outline is the
              coastline
    lakes     polygons — inland water, filled over the land
    borders   polylines — the land boundaries between countries
    rivers    polylines — river and lake centrelines, ranked by size
    cities    a table — every populated place: name, position, population,
              capital flag, biggest first

Each geometry layer is stored at several levels of detail, simplified with
Douglas-Peucker to a tolerance four times coarser at each step. A map draws
the coarsest level whose tolerance is under half a pixel, so the whole world
at a glance costs a few thousand vertices and a single coastline up close
still has every one Natural Earth digitised.

Format (little-endian):

    b"RCWORLD1"  u32 payload length  zlib(payload)

    payload:
        u8 layer count, then per layer:
            u8 kind (0 polygons, 1 lines)  u8 level count, then per level:
                f32 tolerance (degrees)  u32 shape count, then per shape:
                    u8 rank  u32 point count
                    i32 lat * 1e5  i32 lon * 1e5          the first point
                    (count - 1) x (i16 dlat, i16 dlon)     steps of 1e-4 degrees
        u32 city count, then per city:
            i32 lat * 1e5  i32 lon * 1e5  u32 population  u8 flags (1 capital)
            u8 name length  name (ASCII)

A polygon layer's shapes are rings, wound the shapefile way — outer rings
clockwise, holes counter-clockwise — so a non-zero fill of all of them at once
leaves the holes empty. A step longer than an i16 of 1e-4 degrees reaches is
split into several along the same straight line.

Run from anywhere; it downloads into NE_CACHE (default /tmp/naturalearth) and
writes world.bin next to itself.

    python3 make_world.py
"""

import io
import math
import os
import struct
import sys
import unicodedata
import urllib.request
import zipfile
import zlib

MAGIC = b"RCWORLD1"
CACHE = os.environ.get("NE_CACHE", "/tmp/naturalearth")
BASE = "https://naciscdn.org/naturalearth/10m"
SOURCES = {
    "land": f"{BASE}/physical/ne_10m_land.zip",
    "lakes": f"{BASE}/physical/ne_10m_lakes.zip",
    "borders": f"{BASE}/cultural/ne_10m_admin_0_boundary_lines_land.zip",
    "rivers": f"{BASE}/physical/ne_10m_rivers_lake_centerlines_scale_rank.zip",
    "places": f"{BASE}/cultural/ne_10m_populated_places.zip",
}

# Tolerance per level, finest first. Each is four times the one before: a map
# picks the coarsest one under half a pixel, so whatever was thrown away is
# always smaller than a pixel.
LEVELS = [0.003, 0.012, 0.05, 0.2, 0.8]

# One step of a delta, in degrees: 11 m, far below the finest tolerance, and an
# i16 of them reaches 3.2 degrees.
STEP = 1e-4
REACH = 30000 * STEP

POLYGONS, LINES = 0, 1


def fetch(name, ext="shp"):
    """One member of a Natural Earth zip, downloaded once and cached."""
    path = os.path.join(CACHE, f"{name}.{ext}")
    if not os.path.exists(path):
        url = SOURCES[name]
        print(f"fetching {url}", file=sys.stderr)
        with urllib.request.urlopen(url, timeout=300) as r:
            blob = r.read()
        with zipfile.ZipFile(io.BytesIO(blob)) as z:
            for want in ("shp", "dbf"):
                member = next((n for n in z.namelist() if n.endswith("." + want)), None)
                if member is not None:
                    with open(os.path.join(CACHE, f"{name}.{want}"), "wb") as f:
                        f.write(z.read(member))
    with open(path, "rb") as f:
        return f.read()


def shapes(shp):
    """Each record's parts as lists of (lon, lat), for polylines and polygons."""
    pos = 100
    while pos + 8 <= len(shp):
        _, words = struct.unpack_from(">ii", shp, pos)
        pos += 8
        end = pos + words * 2
        (kind,) = struct.unpack_from("<i", shp, pos)
        if kind in (3, 5):
            n_parts, n_points = struct.unpack_from("<ii", shp, pos + 36)
            parts = struct.unpack_from(f"<{n_parts}i", shp, pos + 44)
            pts = struct.unpack_from(f"<{2 * n_points}d", shp, pos + 44 + n_parts * 4)
            bounds = list(parts) + [n_points]
            yield [
                [(pts[2 * i], pts[2 * i + 1]) for i in range(bounds[k], bounds[k + 1])]
                for k in range(n_parts)
            ]
        else:
            yield []
        pos = end


def dbf(blob, want):
    """One dict per record, holding the `want`ed fields as stripped strings."""
    _, _, _, _, n_recs, hdr_len, rec_len = struct.unpack_from("<BBBBIHH", blob, 0)
    fields, off, pos = [], 0, 32
    while blob[pos] != 0x0D:
        name = blob[pos : pos + 11].split(b"\0")[0].decode("latin1")
        length = blob[pos + 16]
        if name.lower() in want:
            fields.append((name.lower(), off, length))
        off += length
        pos += 32
    for i in range(n_recs):
        base = hdr_len + i * rec_len + 1
        yield {
            name: blob[base + o : base + o + ln].decode("utf-8", "replace").strip()
            for name, o, ln in fields
        }


def douglas_peucker(pts, eps):
    """Drop every vertex within `eps` of the line through its neighbours."""
    if len(pts) < 3:
        return pts
    keep = bytearray(len(pts))
    keep[0] = keep[-1] = 1
    stack = [(0, len(pts) - 1)]
    while stack:
        a, b = stack.pop()
        (x0, y0), (x1, y1) = pts[a], pts[b]
        dx, dy = x1 - x0, y1 - y0
        norm = math.hypot(dx, dy)
        far, at = eps, None
        if norm == 0.0:
            for i in range(a + 1, b):
                d = math.hypot(pts[i][0] - x0, pts[i][1] - y0)
                if d > far:
                    far, at = d, i
        else:
            for i in range(a + 1, b):
                x, y = pts[i]
                d = abs(dy * (x - x0) - dx * (y - y0)) / norm
                if d > far:
                    far, at = d, i
        if at is not None:
            keep[at] = 1
            stack.append((a, at))
            stack.append((at, b))
    return [p for p, k in zip(pts, keep) if k]


def simplify_ring(ring, eps):
    """A closed ring simplified, still closed and still a ring: split at its
    farthest point from the start, so neither half is a degenerate line."""
    if ring[0] == ring[-1]:
        ring = ring[:-1]
    if len(ring) < 4:
        return ring
    x0, y0 = ring[0]
    far = max(range(len(ring)), key=lambda i: (ring[i][0] - x0) ** 2 + (ring[i][1] - y0) ** 2)
    a = douglas_peucker(ring[: far + 1], eps)
    b = douglas_peucker(ring[far:] + [ring[0]], eps)
    return a[:-1] + b[:-1]


def span(pts):
    lons = [p[0] for p in pts]
    lats = [p[1] for p in pts]
    return max(max(lons) - min(lons), max(lats) - min(lats))


def split_dateline(run):
    """Break a line wherever it steps across the antimeridian."""
    out, cur = [], [run[0]]
    for prev, p in zip(run, run[1:]):
        if abs(p[0] - prev[0]) > 180.0:
            out.append(cur)
            cur = []
        cur.append(p)
    out.append(cur)
    return [r for r in out if len(r) >= 2]


def encode(pts, rank):
    """One shape: rank, count, the first point, then quantised steps."""
    dense = [pts[0]]
    for (x0, y0), (x1, y1) in zip(pts, pts[1:]):
        n = int(max(abs(x1 - x0), abs(y1 - y0)) / REACH) + 1
        for k in range(1, n + 1):
            f = k / n
            dense.append((x0 + (x1 - x0) * f, y0 + (y1 - y0) * f))
    lon0, lat0 = dense[0]
    out = [struct.pack("<BIii", rank, len(dense), round(lat0 * 1e5), round(lon0 * 1e5))]
    prev = (round(lat0 / STEP), round(lon0 / STEP))
    steps = bytearray()
    for lon, lat in dense[1:]:
        cur = (round(lat / STEP), round(lon / STEP))
        steps += struct.pack("<hh", cur[0] - prev[0], cur[1] - prev[1])
        prev = cur
    out.append(bytes(steps))
    return b"".join(out)


def build_layer(kind, parts, keep):
    """`parts` is (rank, points); `keep(eps, rank)` filters a level's shapes.
    Each level is simplified from the one before it, which is both far faster
    than starting from the source every time and guarantees that every level
    has fewer vertices than the last."""
    levels = []
    current = parts
    for eps in LEVELS:
        simplified = []
        for rank, pts in current:
            if not keep(eps, rank) or span(pts) < 2.0 * eps:
                continue
            if kind == POLYGONS:
                s = simplify_ring(pts, eps)
                if len(s) >= 3:
                    simplified.append((rank, s))
            else:
                s = douglas_peucker(pts, eps)
                if len(s) >= 2:
                    simplified.append((rank, s))
        current = simplified
        blob = b"".join(encode(p, r) for r, p in simplified)
        vertices = sum(len(p) for _, p in simplified)
        print(
            f"    @{eps}: {len(simplified)} shapes, {vertices} vertices, {len(blob) / 1024:.0f} kB",
            file=sys.stderr,
        )
        levels.append(struct.pack("<fI", eps, len(simplified)) + blob)
    return struct.pack("<BB", kind, len(levels)) + b"".join(levels)


def rings(name, rank_of=None):
    """Every ring of a polygon layer, with the rank of its record."""
    ranks = rank_of(fetch(name, "dbf")) if rank_of else None
    out = []
    for i, record in enumerate(shapes(fetch(name))):
        for ring in record:
            if len(ring) >= 4:
                out.append((ranks[i] if ranks else 0, ring))
    return out


def lines(name, rank_of=None):
    ranks = rank_of(fetch(name, "dbf")) if rank_of else None
    out = []
    for i, record in enumerate(shapes(fetch(name))):
        for part in record:
            if len(part) >= 2:
                for run in split_dateline(part):
                    out.append((ranks[i] if ranks else 0, run))
    return out


def lake_ranks(blob):
    # Natural Earth's scalerank: 0 for the largest lakes, rising to about 12.
    return [min(15, max(0, int(float(r["scalerank"] or 12)))) for r in dbf(blob, {"scalerank"})]


def river_ranks(blob):
    # Stroke weight 0.15 ... 2.0 over the sixteen ranks a nibble holds; bigger
    # is a bigger river.
    return [
        min(15, max(0, round((float(r["strokeweig"] or 0.2) - 0.15) * 8.0)))
        for r in dbf(blob, {"strokeweig"})
    ]


def keep_lake(eps, rank):
    # Zoomed out to a continent, only the lakes a continent's map would show.
    return rank <= {0.003: 15, 0.012: 12, 0.05: 8, 0.2: 5, 0.8: 3}[eps]


def keep_river(eps, rank):
    return eps < 0.05 or rank >= (2 if eps < 0.2 else 5)


def ascii_name(name):
    stripped = unicodedata.normalize("NFKD", name).encode("ascii", "ignore")
    return stripped[:255]


def build_cities():
    rows = dbf(
        fetch("places", "dbf"),
        {"nameascii", "name", "latitude", "longitude", "pop_max", "adm0cap"},
    )
    out = []
    for r in rows:
        name = ascii_name(r["nameascii"] or r["name"])
        if not name:
            continue
        pop = max(0, int(float(r["pop_max"] or 0)))
        flags = 1 if r["adm0cap"] in ("1", "1.0", "1.00000000000") else 0
        rec = struct.pack(
            "<iiIBB",
            round(float(r["latitude"]) * 1e5),
            round(float(r["longitude"]) * 1e5),
            pop,
            flags,
            len(name),
        )
        out.append((pop, rec + name))
    out.sort(key=lambda p: -p[0])
    print(f"  cities: {len(out)}", file=sys.stderr)
    return struct.pack("<I", len(out)) + b"".join(rec for _, rec in out)


def main():
    os.makedirs(CACHE, exist_ok=True)
    layers = []
    print("  land", file=sys.stderr)
    layers.append(build_layer(POLYGONS, rings("land"), lambda eps, rank: True))
    print("  lakes", file=sys.stderr)
    layers.append(build_layer(POLYGONS, rings("lakes", lake_ranks), keep_lake))
    print("  borders", file=sys.stderr)
    layers.append(build_layer(LINES, lines("borders"), lambda eps, rank: True))
    print("  rivers", file=sys.stderr)
    layers.append(build_layer(LINES, lines("rivers", river_ranks), keep_river))
    payload = struct.pack("<B", len(layers)) + b"".join(layers) + build_cities()
    packed = zlib.compress(payload, 9)
    here = os.path.dirname(os.path.abspath(__file__))
    with open(os.path.join(here, "world.bin"), "wb") as f:
        f.write(MAGIC + struct.pack("<I", len(payload)) + packed)
    print(
        f"world.bin: {len(packed) / 1024:.0f} kB ({len(payload) / 1024:.0f} kB unpacked)",
        file=sys.stderr,
    )


if __name__ == "__main__":
    main()
