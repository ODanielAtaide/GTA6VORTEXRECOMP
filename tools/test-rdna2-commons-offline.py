# SPDX-License-Identifier: GPL-3.0-or-later
"""Small fixture tests for the offline adapter; no game data or GPU needed."""
import importlib.util
from pathlib import Path
import struct
import tempfile
import unittest

spec = importlib.util.spec_from_file_location(
    "offline_adapter", Path(__file__).with_name("rdna2-commons-offline.py"))
adapter = importlib.util.module_from_spec(spec)
spec.loader.exec_module(adapter)


class InspectSpirv(unittest.TestCase):
    def check_words(self, words):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "fixture.spv"
            path.write_bytes(struct.pack("<%dI" % len(words), *words))
            return adapter.inspect_spirv(path)

    def test_metadata(self):
        # Synthetic instruction stream for parser only, not a valid executable module.
        header = [0x07230203, 0x00010500, 0, 10, 0]
        cap = [(2 << 16) | 17, 1]
        entry = [(5 << 16) | 15, 5, 1, 0x6e69616d, 0]
        self.assertEqual(self.check_words(header + cap + entry),
                         {"version": "0x10500", "execution_models": [5],
                          "capabilities": [1]})

    def test_invalid_instruction_length(self):
        header = [0x07230203, 0x00010500, 0, 10, 0]
        for bad in [0, (2 << 16) | 17]:
            with self.subTest(bad=bad), self.assertRaises(ValueError):
                self.check_words(header + [bad])

    def test_invalid_magic(self):
        with self.assertRaises(ValueError):
            self.check_words([0, 0, 0, 0, 0])

    def test_short_header(self):
        with self.assertRaises(ValueError):
            self.check_words([0x07230203])


if __name__ == "__main__":
    unittest.main()
