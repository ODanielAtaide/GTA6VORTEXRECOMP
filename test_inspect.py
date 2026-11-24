import struct
import unittest
from vortex_inspect import inspect

def fixture():
    data = bytearray(64+24+112+40)
    data[:2] = b"MZ"
    struct.pack_into("<I", data, 60, 64)
    data[64:68] = b"PE\0\0"
    struct.pack_into("<HHIIIHH", data, 68, 0x8664, 1, 0, 0, 0, 112, 2)
    struct.pack_into("<H", data, 88, 0x20b)
    return data

class InspectorTests(unittest.TestCase):
    def test_valid(self):
        self.assertEqual(inspect(fixture())["format"], "PE32+")
    def test_bad_magic(self):
        with self.assertRaises(ValueError):
            inspect(b"bad")
    def test_truncated(self):
        with self.assertRaises(ValueError):
            inspect(fixture()[:-1])
    def test_bad_offset(self):
        data = fixture()
        struct.pack_into("<I", data, 60, 0xffffffff)
        with self.assertRaises(ValueError):
            inspect(data)
    def test_bad_optional_header(self):
        data = fixture()
        struct.pack_into("<H", data, 88, 0)
        with self.assertRaises(ValueError):
            inspect(data)

if __name__ == "__main__":
    unittest.main()
