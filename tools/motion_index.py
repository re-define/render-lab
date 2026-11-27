"""Fit the shared feature metric and bake the Silver Wolf PCA search tree."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import tempfile

import numpy as np


# The reader recomputes this key from the embedded source bank, recipe and exact
# tool text. LF normalization keeps a checkout with CRLF equivalent.
FORMAT = 2
KEY_DOMAIN = b"render-lab/motion-search/2\x00"


def source_key(bank, poses):
    h = hashlib.sha256(KEY_DOMAIN)
    for data in (Path(__file__).read_bytes(), bank):
        h.update(data.replace(b"\r\n", b"\n"))
        h.update(b"\x00")
    h.update(poses)
    return h.hexdigest()


def payload_bytes(tree):
    return json.dumps(tree, separators=(",", ":"), allow_nan=False).encode("utf8")


def build_tree(clips):
    horizon = 23
    dimensions = 6
    leaf_size = 16
    starts = np.cumsum([0] + [len(c) for c in clips[:-1]])
    samples = np.concatenate(
        [start + np.arange(len(c) - horizon) for start, c in zip(starts, clips)]
    )
    features = np.concatenate([c[:-horizon, :24] for c in clips]).astype(np.float64)
    mean = features.mean(axis=0).astype(np.float32)
    # Displacement, direction, foot position, foot velocity have different units.
    floor = [0.05, 0.05, 0.1, 0.1] * 3 + [0.05] * 6 + [0.15] * 6
    weights = [5.0, 5.0, 1.6, 1.6] * 3 + [0.8] * 6 + [0.018] * 6
    gain = (np.sqrt(weights) / np.maximum(features.std(axis=0), floor)).astype(
        np.float32
    )
    normalized = (features.astype(np.float32) - mean) * gain
    covariance = np.cov(normalized, rowvar=False) if len(features) > 1 else np.zeros((24, 24))
    eigenvalues, eigenvectors = np.linalg.eigh(covariance)
    axes = eigenvectors[:, -dimensions:][:, ::-1].T.astype(np.float32)
    # Canonicalize eigenvector signs so the serialized tree is reproducible.
    axes *= np.sign(axes[np.arange(dimensions), np.abs(axes).argmax(axis=1)])[:, None]
    projected = normalized @ axes.T
    nodes = [None]
    ordered = []

    def fill(node, indices):
        points = projected[indices]
        low, high = points.min(axis=0), points.max(axis=0)
        if len(indices) <= leaf_size:
            first, count = len(ordered), len(indices)
            ordered.extend(samples[indices].tolist())
        else:
            first, count = len(nodes), 0
            nodes.extend([None, None])
            axis = (high - low).argmax()
            indices = indices[np.argsort(points[:, axis], kind="stable")]
            mid = len(indices) // 2
            fill(first, indices[:mid])
            fill(first + 1, indices[mid:])
        nodes[node] = {
            "low": low.tolist(),
            "high": high.tolist(),
            "first": first,
            "count": count,
        }

    fill(0, np.arange(len(samples)))
    total = eigenvalues.sum()
    return {
        "version": 1,
        "mean": mean.tolist(),
        "gain": gain.tolist(),
        "axes": axes.tolist(),
        "variance_retained": float(eigenvalues[-dimensions:].sum() / total) if total > 0 else 0.0,
        "leaf_size": leaf_size,
        "nodes": nodes,
        "order": ordered,
    }


def current_index(encoded, key):
    # The envelope ends with the raw tree. Check its bytes without decoding the
    # thousands of bounds and float coefficients on every incremental invocation.
    prefix, marker, suffix = encoded.partition(b',"tree":')
    if not marker or not suffix.endswith(b"}"):
        return False
    try:
        header = json.loads(prefix + b"}")
        return (
            header["format"] == FORMAT
            and header["numpy"] == np.__version__
            and header["source_key"] == key
            and header["tree_sha256"] == hashlib.sha256(memoryview(suffix)[:-1]).hexdigest()
        )
    except (KeyError, ValueError, TypeError):
        return False


def cook(directory, *, check=False, force=False):
    bank = (directory / "bank.json").read_bytes()
    poses = (directory / "poses.bin").read_bytes()
    key = source_key(bank, poses)
    output = directory / "search.json"
    if output.is_file() and not force and current_index(output.read_bytes(), key):
        return False
    if check:
        raise ValueError("motion search index is stale or damaged; run tools/motion_index.py")
    meta = json.loads(bank)
    frames = [clip["frames"] for clip in meta["clips"]]
    if meta["version"] != 1 or meta["fps"] != 30 or not meta["bones"] or not frames or min(frames) <= 23:
        raise ValueError("invalid motion bank or clip lengths")
    rows = np.frombuffer(poses, dtype="<f4").reshape(sum(frames), 28 + 4 * len(meta["bones"]))
    clips = np.split(rows, np.cumsum(frames)[:-1])
    if not np.isfinite(rows).all():
        raise ValueError("non-finite motion bank")
    tree = payload_bytes(build_tree(clips))
    header = dict(format=FORMAT, numpy=np.__version__, source_key=key,
                  tree_sha256=hashlib.sha256(tree).hexdigest())
    packed = payload_bytes(header)[:-1] + b',"tree":' + tree + b"}"
    name = None
    try:
        with tempfile.NamedTemporaryFile(dir=directory, delete=False) as dest:
            name = dest.name
            dest.write(packed)
        os.replace(name, output)
    finally:
        if name is not None:
            Path(name).unlink(missing_ok=True)
    return True


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("bank", type=Path, nargs="?", default=Path(__file__).resolve().parents[1] / "assets/motion")
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--check", action="store_true", help="fail if the index needs baking")
    mode.add_argument("--force", action="store_true", help="recompute even if all inputs match")
    args = parser.parse_args()
    try:
        built = cook(args.bank, check=args.check, force=args.force)
    except (OSError, ValueError, KeyError) as error:
        parser.exit(1, f"{error}\n")
    print("baked search.json" if built else "search.json is up to date")


if __name__ == "__main__":
    main()
