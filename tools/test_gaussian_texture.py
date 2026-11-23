"""Cross-check G2T lists against the physical-parameter PLY model at every test texel."""

import struct
import tempfile
import unittest
from pathlib import Path

import numpy as np
from gaussian_texture import pack, write_ply


def ply_image(rows, width, height):
    """Independent reference: direct local-axis evaluation inside the CUDA tile rectangle."""
    out = np.zeros((height, width, 3), np.float64)
    for x, y, r, g, b, sx, sy, angle in rows.astype(np.float64):
        middle = (sx * sx + sy * sy) / 2
        radius = np.ceil(
            3 * np.sqrt(middle + np.sqrt(max(0.1, middle**2 - (sx * sy) ** 2)))
        )
        lo = np.maximum(0, np.trunc((np.array([x, y]) - radius) / 16)).astype(int) * 16
        hi = np.minimum(
            [width, height],
            np.trunc((np.array([x, y]) + radius + 15) / 16).astype(int) * 16,
        )
        if np.any(lo >= hi):
            continue
        dx = x - np.arange(lo[0], hi[0])[None, :]
        dy = y - np.arange(lo[1], hi[1])[:, None]
        cs, sn = np.cos(angle), np.sin(angle)
        w = np.minimum(
            0.99,
            np.exp(
                -0.5
                * (((cs * dx - sn * dy) / sx) ** 2 + ((sn * dx + cs * dy) / sy) ** 2)
            ),
        )
        out[lo[1] : hi[1], lo[0] : hi[0]] += np.where(w >= 1 / 255, w, 0)[
            :, :, None
        ] * [r, g, b]
    return out


class PackedTests(unittest.TestCase):
    def test_sizes_edges_signed_color_and_empty_tiles(self):
        width, height = 67, 49
        rows = np.array(
            [
                [-2, 2, 1, 0.1, -0.2, 12, 1, 0.83],
                [16.2, 16, 0, 0.4, 1, 8, 7, -1.23],
                [44.8, 30, 1, 0, 0.2, 0.5, 5, 0.15],
                [64, 47, 0.3, -0.5, 0.2, 2, 0.8, 1.4],
            ],
            np.float32,
        )
        expected = ply_image(rows, width, height)
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "field.ply"
            write_ply(path, rows)
            for size in (8, 16, 32):
                blob = pack(path, width, height, size)
                _, _, _, _, n, refs, dir_word, ids_word = struct.unpack_from(
                    "<4s7I", blob
                )
                records = np.frombuffer(
                    blob, dtype="<f4", count=10 * n, offset=32
                ).reshape(n, 10)
                offsets = np.frombuffer(
                    blob, dtype="<u4", count=ids_word - dir_word, offset=4 * dir_word
                )
                ids = np.frombuffer(blob, dtype="<u2", count=refs, offset=4 * ids_word)
                out = np.zeros_like(expected)
                for y in range(height):
                    for x in range(width):
                        tile = (y // size) * ((width + size - 1) // size) + x // size
                        for i in ids[offsets[tile] : offsets[tile + 1]]:
                            record = records[i]
                            lo, hi = record[8:].view("<u4")
                            if not (
                                (lo & 65535) <= x // 16 < (hi & 65535)
                                and (lo >> 16) <= y // 16 < (hi >> 16)
                            ):
                                continue
                            dx, dy = record[:2] - [x, y]
                            a, b, c = record[2:5]
                            weight = min(
                                0.99,
                                np.exp(-0.5 * (a * dx**2 + c * dy**2) - b * dx * dy),
                            )
                            if weight >= 1 / 255:
                                out[y, x] += weight * record[5:8]
                np.testing.assert_allclose(out, expected, atol=4e-6, rtol=1e-5)


if __name__ == "__main__":
    unittest.main()
