"""Fit the Modern Hall floor or pack a physical-scale 2DGaussian PLY for Vulkan."""

import argparse
import math
import struct
from pathlib import Path

import numpy as np
from plyfile import PlyData, PlyElement

FIELDS = ("x", "y", "r", "g", "b", "scale_0", "scale_1", "rot_0")


def write_ply(path, rows):
    vertices = np.empty(len(rows), dtype=[(name, "<f4") for name in FIELDS])
    for name, col in zip(FIELDS, rows.T):
        vertices[name] = col
    path.parent.mkdir(parents=True, exist_ok=True)
    PlyData([PlyElement.describe(vertices, "vertex")], byte_order="<").write(path)


def pack(ply, width, height, size):
    if not (0 < width <= 32768 and 0 < height <= 32768 and size in (8, 16, 32)):
        raise ValueError("Expected dimensions in 1..32768 and tile size 8, 16 or 32")
    data = PlyData.read(str(ply))["vertex"]
    rows = np.stack([data[name] for name in FIELDS], axis=1).astype(np.float32)
    if not np.isfinite(rows).all() or np.any(rows[:, 5:7] <= 0):
        raise ValueError("Invalid Gaussian parameters or nonpositive physical scale")
    x, y, r, g, b, sx, sy, angle = rows.T
    cs, sn = np.cos(angle), np.sin(angle)
    xx = (sx * cs) ** 2 + (sy * sn) ** 2
    yy = (sx * sn) ** 2 + (sy * cs) ** 2
    xy = (sy**2 - sx**2) * cs * sn
    det = xx * yy - xy**2
    if not np.all(np.isfinite(det) & (det > 0)):
        raise ValueError("Singular or unrepresentable float32 covariance")
    a, cross, c = yy / det, -xy / det, xx / det
    mid = (xx + yy) * 0.5
    radius = np.ceil(3 * np.sqrt(mid + np.sqrt(np.maximum(0.1, mid**2 - det))))

    # The CUDA rasterizer's 16-texel coverage is part of the exported field.
    # Keep it even when building a different lookup tile size.
    def limits(center, extent):
        n = (extent + 15) // 16
        return (
            np.clip(np.trunc((center - radius) / 16), 0, n).astype(np.uint32),
            np.clip(np.trunc((center + radius + 15) / 16), 0, n).astype(np.uint32),
        )

    lx, hx = limits(x, width)
    ly, hy = limits(y, height)
    rec = np.empty((len(x), 10), dtype="<u4")
    rec[:, :8] = (
        np.stack((x, y, a, cross, c, r, g, b), axis=1).astype("<f4").view("<u4")
    )
    rec[:, 8] = lx | (ly << 16)
    rec[:, 9] = hx | (hy << 16)
    nx, ny = (width + size - 1) // size, (height + size - 1) // size
    buckets = [[] for _ in range(nx * ny)]
    # Use the stored conic's marginal ellipse bounds, rounded outward.
    det_q = a.astype(float) * c - cross.astype(float) ** 2
    if np.any(det_q <= 0):
        raise ValueError("Invalid packed conic")
    cutoff = 2 * math.log(255)
    ex, ey = np.sqrt(cutoff * c / det_q) + 1, np.sqrt(cutoff * a / det_q) + 1
    for i in range(len(rows)):
        left = max(int(lx[i]) * 16, math.floor(float(x[i]) - ex[i]), 0)
        top = max(int(ly[i]) * 16, math.floor(float(y[i]) - ey[i]), 0)
        right = min(int(hx[i]) * 16 - 1, math.ceil(float(x[i]) + ex[i]), width - 1)
        bottom = min(int(hy[i]) * 16 - 1, math.ceil(float(y[i]) + ey[i]), height - 1)
        if left > right or top > bottom:
            continue
        for ty in range(top // size, bottom // size + 1):
            for tx in range(left // size, right // size + 1):
                buckets[ty * nx + tx].append(i)
    counts = np.fromiter(map(len, buckets), dtype=np.int64)
    offsets = np.r_[0, counts.cumsum()]
    first = 8 + 10 * len(rows)
    if first + len(offsets) + offsets[-1] >= 2**32:
        raise ValueError("Texture exceeds 32-bit GPU addressing")
    header = struct.pack(
        "<4s7I",
        b"G2T1",
        width,
        height,
        size,
        len(rows),
        int(offsets[-1]),
        first,
        first + len(offsets),
    )
    ids = np.fromiter(
        (i for group in buckets for i in group),
        dtype="<u2" if len(rows) <= 65536 else "<u4",
    )
    payload = (
        header
        + rec.tobytes()
        + offsets.astype("<u4").tobytes()
        + ids.tobytes()
        + bytes(-ids.nbytes % 4)
    )
    print(
        f"{len(rows)} Gaussians; tile {size}; {len(ids)} references; candidates mean {counts.mean():.2f} "
        f"p95 {np.percentile(counts, 95):.0f} max {counts.max()}; {len(payload)} bytes",
        flush=True,
    )
    return payload


def fit_floor(path, ply, steps):
    import torch
    from torch import nn
    from train_texture_grid import base_colors

    rgb = next(rgb for name, rgb, _ in base_colors(path) if name == "FloorTiles")
    h, w, _ = rgb.shape
    device = "cuda" if torch.cuda.is_available() else "cpu"
    torch.manual_seed(47)
    target = torch.tensor(rgb, device=device, dtype=torch.float32) / 255
    axis = torch.arange(-3, 4, device=device)
    neighbors = torch.cartesian_prod(axis, axis)
    levels = []
    params = []
    for spacing in (40, 12):
        nx, ny = math.ceil(w / spacing) + 2, math.ceil(h / spacing) + 2
        indices = torch.cartesian_prod(
            torch.arange(ny, device=device), torch.arange(nx, device=device)
        ).flip(1)
        base = (indices.float() - 0.5) * spacing
        # Infinite lattice covers image edges; one border cell is explicitly trained.
        init = torch.zeros((nx * ny, 8), device=device)
        if not levels:
            init[:, 5:] = target.mean((0, 1)) / (2 * math.pi * 0.5**2)
        param = nn.Parameter(init)
        params.append(param)
        levels.append((spacing, nx, ny, base, param))
    opt = torch.optim.Adam(params, lr=0.016)

    def unpack(base, raw, s):
        return (
            base + 0.27 * s * raw[..., :2].tanh(),
            (0.18 + 0.64 * raw[..., 2:4].sigmoid()) * s,
            raw[..., 4],
            raw[..., 5:],
        )

    def render(pixels):
        result = torch.zeros((len(pixels), 3), device=device)
        for s, nx, ny, base, param in levels:
            cells = pixels[:, None, :] // s + 1 + neighbors
            ix, iy = cells.unbind(-1)
            valid = (ix >= 0) & (ix < nx) & (iy >= 0) & (iy < ny)
            ids = iy.clamp(0, ny - 1) * nx + ix.clamp(0, nx - 1)
            center, scales, angle, color = unpack(base[ids], param[ids], s)
            dx, dy = (center - pixels[:, None, :]).unbind(-1)
            cs, sn = angle.cos(), angle.sin()
            q = ((dx * cs - dy * sn) / scales[..., 0]).square() + (
                (dx * sn + dy * cs) / scales[..., 1]
            ).square()
            weight = (-0.5 * q).exp().clamp_max(0.99)
            weight = torch.where(valid & (weight >= 1 / 255), weight, 0)
            result = result + (weight[..., None] * color).sum(1)
        return result

    for step in range(steps):
        px = torch.randint(w, (1024,), device=device)
        py = torch.randint(h, (1024,), device=device)
        predicted = render(torch.stack((px, py), dim=-1))
        loss = (predicted - target[py, px]).square().mean()
        opt.zero_grad(set_to_none=True)
        loss.backward()
        opt.step()
        if step == steps * 3 // 4:
            opt.param_groups[0]["lr"] *= 0.25
    with torch.no_grad():
        rows = []
        for s, _, _, base, param in levels:
            center, scale, angle, color = unpack(base, param, s)
            rows.append(torch.cat((center, color, scale, angle[:, None]), dim=1))
        write_ply(ply, torch.cat(rows).cpu().numpy())
    print(
        f"PyTorch {torch.__version__}, {device}, {steps} Adam steps; saved {ply}",
        flush=True,
    )
    return w, h


def main():
    cli = argparse.ArgumentParser(description=__doc__)
    sub = cli.add_subparsers(dest="command", required=True)
    fit = sub.add_parser("fit")
    fit.add_argument("scene", type=Path)
    fit.add_argument("ply", type=Path)
    fit.add_argument("--steps", type=int, default=1800)
    convert = sub.add_parser("pack")
    convert.add_argument("ply", type=Path)
    convert.add_argument("output", type=Path)
    convert.add_argument("--width", type=int, required=True)
    convert.add_argument("--height", type=int, required=True)
    convert.add_argument("--tile", type=int, default=32, choices=(8, 16, 32))
    args = cli.parse_args()
    if args.command == "fit":
        if args.steps < 1:
            cli.error("positive training steps required")
        fit_floor(args.scene, args.ply, args.steps)
    else:
        blob = pack(args.ply, args.width, args.height, args.tile)
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_bytes(blob)


if __name__ == "__main__":
    main()
