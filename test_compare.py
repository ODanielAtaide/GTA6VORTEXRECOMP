import unittest
from vortex_compare import compare

def run(fps, ms, drops=None, n=5):
    return [f"{fps} fps, {ms} ms/frame" + (f", dropped {drops} without a host pipeline" if drops is not None else "")] * n

class CompareTests(unittest.TestCase):
    def test_speed(self):
        result = compare(run(2, 500, 1), run(4, 250, 1))
        self.assertEqual(result["median_fps_change_pct"], 100.0)
        self.assertEqual(result["median_frame_time_change_pct"], -50.0)
        self.assertEqual(result["drop_guard"], "pass")
    def test_regression(self):
        self.assertEqual(compare(run(2, 500, 1), run(4, 250, 2))["drop_guard"], "regression")
    def test_unknown(self):
        self.assertEqual(compare(run(2, 500), run(4, 250, 1))["drop_guard"], "unknown")
    def test_partial(self):
        self.assertEqual(compare(run(2, 500, 1), run(4, 250, 1, 4) + run(4, 250, None, 1))["drop_guard"], "unknown")
    def test_insufficient(self):
        with self.assertRaises(ValueError):
            compare(run(2, 500, n=1), run(4, 250))
    def test_threshold(self):
        with self.assertRaises(ValueError):
            compare(run(2, 500), run(4, 250), min_samples=0)

if __name__ == "__main__":
    unittest.main()
