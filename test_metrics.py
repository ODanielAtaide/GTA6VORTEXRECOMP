import unittest
from vortex_metrics import summarize

class MetricsTests(unittest.TestCase):
    def test_windows(self):
        result = summarize(["2.0 fps, 500.0 ms/frame", "4.0 fps, 250.0 ms/frame"])
        self.assertEqual(result["sample_count"], 2)
        self.assertEqual(result["median_window_fps"], 3.0)
        self.assertEqual(result["median_window_ms_per_frame"], 375.0)
    def test_target_budget(self):
        result = summarize(["4 fps, 250 ms/frame", "30 fps, 33.3 ms/frame"])
        self.assertEqual(result["windows_meeting_30_fps"], 1)
        self.assertEqual(result["target_fps"], 30)
        self.assertAlmostEqual(result["median_frame_budget_multiple"], 4.2495, places=4)
    def test_ansi_and_drops(self):
        result = summarize(["\x1b[32mINFO\x1b[0m 4.0 fps, 250.0 ms/frame, dropped 7 without a host pipeline"])
        self.assertEqual(result["median_dropped_draws_per_frame"], 7)
    def test_missing_drop(self):
        self.assertIsNone(summarize(["4 fps, 250 ms/frame"])["median_dropped_draws_per_frame"])
    def test_invalid(self):
        with self.assertRaises(ValueError):
            summarize(["0 fps, 0 ms/frame", "not a sample"])
    def test_ignore_invalid(self):
        self.assertEqual(summarize(["-2 fps, 500 ms/frame", "4 fps, 250 ms/frame"])["sample_count"], 1)

if __name__ == "__main__":
    unittest.main()
