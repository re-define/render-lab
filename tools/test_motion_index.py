"""Regression coverage for the motion index content key and atomic publication."""
import json
from pathlib import Path
import tempfile
import unittest

import numpy as np

from motion_index import cook


class MotionCookTest(unittest.TestCase):
    def test_single_candidate_is_a_valid_leaf(self):
        with tempfile.TemporaryDirectory() as temp:
            bank = Path(temp)
            manifest = {"version": 1, "fps": 30, "bones": ["root"],
                        "clips": [{"name": "idle", "frames": 24}]}
            (bank / "bank.json").write_text(json.dumps(manifest), encoding="utf8")
            (bank / "poses.bin").write_bytes(np.zeros((24, 32), dtype="<f4").tobytes())
            self.assertTrue(cook(bank))
            tree = json.loads((bank / "search.json").read_bytes())["tree"]
            self.assertEqual(tree["order"], [0])
            self.assertEqual(tree["nodes"][0]["count"], 1)
            self.assertEqual(tree["variance_retained"], 0)

    def test_only_changed_inputs_rebuild_and_bad_inputs_preserve_the_last_good_tree(self):
        with tempfile.TemporaryDirectory() as temp:
            bank = Path(temp)
            meta = {"version": 1, "fps": 30, "bones": ["root"],
                    "clips": [{"name": "walk", "frames": 80}]}
            manifest = bank / "bank.json"
            manifest.write_text(json.dumps(meta), encoding="utf8")
            data = np.random.default_rng(9).standard_normal((80, 32)).astype("<f4")
            source = bank / "poses.bin"
            source.write_bytes(data.tobytes())
            output = bank / "search.json"
            self.assertTrue(cook(bank))
            first = output.read_bytes()
            stamp = output.stat().st_mtime_ns
            self.assertFalse(cook(bank))
            self.assertFalse(cook(bank, check=True))
            self.assertEqual(stamp, output.stat().st_mtime_ns)
            self.assertTrue(cook(bank, force=True))
            self.assertEqual(first, output.read_bytes())

            # The source can keep its byte length while changing a sample.
            data[23, 2] += 1
            source.write_bytes(data.tobytes())
            with self.assertRaises(ValueError):
                cook(bank, check=True)
            self.assertTrue(cook(bank))
            self.assertNotEqual(first, output.read_bytes())
            meta["clips"][0]["name"] = "idle"
            manifest.write_text(json.dumps(meta), encoding="utf8")
            self.assertTrue(cook(bank))

            good = output.read_bytes()
            tampered = json.loads(good)
            tampered["tree"]["nodes"][0]["low"][0] += 1
            output.write_text(json.dumps(tampered, separators=(",", ":")), encoding="utf8")
            with self.assertRaises(ValueError):
                cook(bank, check=True)
            self.assertTrue(cook(bank))
            self.assertEqual(good, output.read_bytes())
            source.write_bytes(b"short")
            with self.assertRaises(ValueError):
                cook(bank)
            self.assertEqual(good, output.read_bytes())
