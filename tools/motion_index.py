"""Fit the shared feature metric and bake the Silver Wolf PCA search tree."""

import json

import numpy as np


def write_index(clips, output):
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
    eigenvalues, eigenvectors = np.linalg.eigh(np.cov(normalized, rowvar=False))
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
    data = {
        "version": 1,
        "mean": mean.tolist(),
        "gain": gain.tolist(),
        "axes": axes.tolist(),
        "variance_retained": float(eigenvalues[-dimensions:].sum() / eigenvalues.sum()),
        "leaf_size": leaf_size,
        "nodes": nodes,
        "order": ordered,
    }
    (output / "search.json").write_text(
        json.dumps(data, separators=(",", ":")), encoding="utf8"
    )
