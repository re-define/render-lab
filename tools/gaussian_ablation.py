"""Full-texture pruning and precision sweep on the trained Modern Hall Gaussians."""

import argparse
import json
from pathlib import Path

import numpy as np
import torch
from gaussian_texture import FIELDS, PlyData, pack, write_ply
from PIL import Image
from scipy import sparse
from train_texture_grid import base_colors


def basis(geometry, boxes, width, height):
    """One sparse column per Gaussian, one row per source texel, with engine cutoffs."""
    indices, values, offsets = [], [], [0]
    for (x, y, a, b, c), (left, top, right, bottom) in zip(geometry, boxes):
        xx = np.arange(left, min(right, width), dtype=np.float32)
        yy = np.arange(top, min(bottom, height), dtype=np.float32)
        dx, dy = x - xx[None, :], y - yy[:, None]
        power = -0.5 * (a * dx**2 + c * dy**2) - b * dx * dy
        weight = np.exp(np.minimum(power, 0)).clip(max=0.99)
        py, px = np.nonzero((power <= 0) & (weight >= 1 / 255))
        indices.append(((py + top) * width + px + left).astype(np.int32))
        values.append(weight[py, px])
        offsets.append(offsets[-1] + len(py))
    return sparse.csc_matrix(
        (np.concatenate(values), np.concatenate(indices), offsets),
        shape=(width * height, len(geometry)),
    ).tocsr()


def compact_geometry(g, boxes):
    result = g.copy()
    origin = boxes[:, :2].astype(np.float32)
    result[:, :2] = (g[:, :2] - origin).astype(np.float16).astype(np.float32) + origin
    root = np.sqrt(g[:, [2, 4]])
    corr = np.rint(128 * g[:, 3] / root.prod(axis=1)).clip(-127, 127) / 128
    root = root.astype(np.float16).astype(np.float32)
    result[:, [2, 4]] = root**2
    result[:, 3] = corr * root.prod(axis=1)
    return result


def color_quant(c, bits):
    levels = (1 << (bits - 1)) - 1
    scale = np.abs(c).max(axis=0) / levels
    return np.rint(c / np.where(scale == 0, 1, scale)).clip(-levels, levels) * scale


def refit(normal, rhs, prior):
    """Jacobi-preconditioned CG in PyTorch for the three regularized least-squares fits."""
    device = "cuda" if torch.cuda.is_available() else "cpu"
    normal = normal.tocsr().astype(np.float64)
    normal.sort_indices()

    def tensor(x):
        return torch.tensor(x, device=device, dtype=torch.float64)

    mat = torch.sparse_csr_tensor(
        normal.indptr,
        normal.indices,
        normal.data,
        size=normal.shape,
        device=device,
        dtype=torch.float64,
    )
    ridge = 0.1

    def multiply(p):
        return torch.sparse.mm(mat, p) + ridge * p

    x = tensor(prior)
    r = tensor(rhs) + ridge * x - multiply(x)
    inv = 1 / (tensor(normal.diagonal())[:, None] + ridge)
    p = inv * r
    rz = (r * p).sum(0)
    for _ in range(200):
        ap = multiply(p)
        step = rz / (p * ap).sum(0).clamp_min(1e-30)
        x += step * p
        r -= step * ap
        z = inv * r
        next_rz = (r * z).sum(0)
        if next_rz.max().item() < 1e-12:
            break
        p = z + (next_rz / rz.clamp_min(1e-30)) * p
        rz = next_rz
    return x.cpu().numpy().astype(np.float32)


def normal_equations(matrix, target):
    a = matrix.astype(np.float64)
    return (a.T @ a).tocsr(), a.T @ target.astype(np.float64)


def figure(path, results, baseline, count, crops):
    import matplotlib.pyplot as plt

    fig = plt.figure(figsize=(10.8, 4.5), layout="constrained")
    grid = fig.add_gridspec(2, 3, width_ratios=[2.7, 1, 1])
    ax = fig.add_subplot(grid[:, 0])
    n = [row["count"] for row in results]
    for key, name, color in [
        ("drop_only", "Prune only", "#777e88"),
        ("compact", "Prune + refit + 16-byte records", "#205cb1"),
    ]:
        ax.plot(n, [row[key] for row in results], ".-", color=color, label=name)
    ax.axhline(
        baseline,
        linestyle="--",
        linewidth=1,
        color="#a34b23",
        label="Original trained model",
    )
    y = next(row["compact"] for row in results if row["count"] == count)
    ax.scatter([count], [y], s=58, facecolor="white", edgecolor="#205cb1", zorder=4)
    ax.annotate(
        f"{count:,} / {y:.2f} dB",
        (count, y),
        xytext=(-55, 18),
        textcoords="offset points",
        fontsize=9,
    )
    ax.set(
        xlabel="Gaussian count",
        ylabel="Full-texture RGB PSNR (dB)",
        title="Modern Hall / FloorTiles",
    )
    ax.grid(axis="y", alpha=0.22)
    ax.legend(frameon=False, fontsize=8, loc="lower right")
    for i, (label, rgb) in enumerate(crops):
        view = fig.add_subplot(grid[i // 2, 1 + i % 2])
        view.imshow(rgb, interpolation="nearest")
        view.set_title(label, fontsize=9)
        view.set_axis_off()
    path.parent.mkdir(parents=True, exist_ok=True)
    fig.savefig(path, dpi=160)
    plt.close(fig)


def main():
    cli = argparse.ArgumentParser(description=__doc__)
    cli.add_argument("scene", type=Path)
    cli.add_argument("ply", type=Path)
    cli.add_argument("output", type=Path)
    cli.add_argument("--loss-db", type=float, default=0.1)
    cli.add_argument("--figure", type=Path)
    args = cli.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    img = next(im for name, im, _ in base_colors(args.scene) if name == "FloorTiles")
    height, width = img.shape[:2]
    target = img.reshape(-1, 3).astype(np.float32) / 255
    v = PlyData.read(str(args.ply))["vertex"]
    rows = np.stack([v[key] for key in FIELDS], axis=1).astype(np.float32)
    blob = pack(args.ply, width, height, 32)
    records = np.frombuffer(blob, "<u4", 10 * len(rows), 32).reshape(-1, 10)
    geom = records[:, :5].view("<f4")
    color = records[:, 5:8].view("<f4")
    box = records[:, 8:]
    boxes = (
        np.stack(
            [box[:, 0] & 65535, box[:, 0] >> 16, box[:, 1] & 65535, box[:, 1] >> 16],
            axis=1,
        ).astype(np.int32)
        * 16
    )
    a = basis(geom, boxes, width, height)
    aq = basis(compact_geometry(geom, boxes), boxes, width, height)

    def score(image):
        return float(
            -10 * np.log10(np.mean((image.clip(0, 1) - target) ** 2, dtype=np.float64))
        )

    baseline = score(a @ color)
    half = basis(geom.astype(np.float16).astype(np.float32), boxes, width, height)
    precision = {
        "fp32": baseline,
        "fp16": score(half @ color.astype(np.float16).astype(np.float32)),
        "geometry_only": score(aq @ color),
    }
    del half
    for shape in ("axis_aligned", "isotropic"):
        simplified = geom.copy()
        simplified[:, 3] = 0
        if shape == "isotropic":
            diag_iso = np.sqrt(geom[:, 2] * geom[:, 4] - geom[:, 3] ** 2)
            simplified[:, [2, 4]] = diag_iso[:, None]
        simple = basis(simplified, boxes, width, height)
        precision[shape] = score(simple @ color)
        precision[shape + "_refit"] = score(
            simple @ refit(*normal_equations(simple, target), color)
        )
        del simple
    for bits in (8, 6, 4):
        q = color_quant(color, bits)
        precision[f"rgb{bits}_only"] = score(a @ q)
        precision[f"mixed_rgb{bits}"] = score(aq @ q)
    print("precision", precision, flush=True)
    # Accumulate the normal equations in double precision before GPU optimization.
    normal, rhs = normal_equations(a, target)
    diag = normal.diagonal()
    keep = np.arange(len(rows))
    results = []
    best = None
    c = color.copy()

    def crop(rgb):
        return rgb.reshape(height, width, 3)[316:444, 446:574].clip(0, 1).copy()

    def render(matrix, indices, coeff):
        full = np.zeros_like(color)
        full[indices] = coeff
        return matrix @ full

    quarter_crop = None
    for frac in (
        1,
        0.875,
        0.75,
        0.625,
        0.5625,
        0.5,
        0.4375,
        0.375,
        0.3125,
        0.25,
        0.1875,
        0.125,
    ):
        count = round(len(rows) * frac)
        if count < len(keep):
            # Squared change in unbounded RGB from removing one fitted column.
            energy = diag[keep] * np.square(c).sum(axis=1)
            chosen = np.sort(np.argsort(-energy, kind="stable")[:count])
            keep = keep[chosen]
        c = refit(normal[keep][:, keep], rhs[keep], color[keep])

        quant_image = render(aq, keep, color_quant(c, 8))
        if frac == 0.25:
            quarter_crop = (f"{count:,} / compact", crop(quant_image))
        row = {
            "count": count,
            "drop_only": score(render(a, keep, color[keep])),
            "refit": score(render(a, keep, c)),
            "compact": score(quant_image),
        }
        print(row, flush=True)
        results.append(row)
        if row["compact"] >= baseline - args.loss_db:
            best = (keep.copy(), c.copy(), quant_image)
    if best is None:
        raise ValueError(
            "No measured compact configuration meets the specified quality budget"
        )
    keep, c, image = best
    pruned = rows[keep].copy()
    pruned[:, 2:5] = c
    path = args.output / "FloorTiles-pruned.ply"
    write_ply(path, pruned)
    (args.output / "FloorTiles-pruned.g2t").write_bytes(pack(path, width, height, 32))
    Image.fromarray(
        np.rint(255 * image.reshape(height, width, 3).clip(0, 1)).astype(np.uint8)
    ).save(args.output / "compact-decoded.png")
    report = {
        "extent": [width, height],
        "precision": precision,
        "pruning": results,
        "loss_budget_db": args.loss_db,
        "selected": len(keep),
    }
    (args.output / "sweep.json").write_text(
        json.dumps(report, indent=2), encoding="utf8"
    )
    if args.figure:
        figure(
            args.figure,
            results,
            baseline,
            len(keep),
            [
                ("Source RGB", crop(target)),
                ("Original 6,554", crop(a @ color)),
                (f"{len(keep):,} / compact", crop(image)),
                quarter_crop,
            ],
        )


if __name__ == "__main__":
    main()
