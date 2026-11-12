#!/usr/bin/env python3
"""独立小型夹具验证轴序、输入域、量化与坏输入；不生成成品或访问存储卡。"""
import struct
import tempfile
import unittest
from pathlib import Path

import numpy as np
import cube2dxo as c


def cube_text(header="", transform=lambda r, g, b: (r, g, b)):
    rows = ["LUT_3D_SIZE 2", header]
    for b in range(2):
        for g in range(2):
            for r in range(2):
                rows.append(" ".join(map(str, transform(r, g, b))))
    return "\n".join(rows) + "\n"


class CubeTests(unittest.TestCase):
    def read(self, text):
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / "fixture.cube"
            path.write_text(text)
            return c.read_cube(path)[1]

    def test_identity_and_rgb_axis_order(self):
        lut = self.read(cube_text())
        points = np.array([[0.13, 0.41, 0.92], [1, 0, 0], [0, 0, 1]])
        np.testing.assert_allclose(c.sample(lut, points), points, atol=1e-7)

    def test_domain_is_input_not_output(self):
        lut = self.read(cube_text("DOMAIN_MIN -1 0 2\nDOMAIN_MAX 1 4 6"))
        np.testing.assert_allclose(c.sample(lut, [[0, 1, 5]]), [[0.5, 0.25, 0.75]])
        np.testing.assert_allclose(lut.values[0, 0, 0], [0, 0, 0])
        np.testing.assert_allclose(c.sample(lut, [[-3, 5, 9]]), [[0, 1, 1]])

    def test_resolve_input_range_and_neutral(self):
        lut = self.read(cube_text("LUT_3D_INPUT_RANGE -1 3"))
        np.testing.assert_allclose(c.sample(lut, [[0, 1, 2]]), [[0.25, 0.5, 0.75]])
        _, inv = c.neutral_inverse(lut)
        self.assertAlmostEqual(float(inv[512]), 1.0, places=5)
        h, stock, tail = c.decode_table(c.STOCK)
        actual = c.compose(lut, stock, tone="neutral")
        np.testing.assert_allclose(actual, stock, atol=1e-6)

    def test_outputs_are_not_clipped_before_interpolation(self):
        lut = self.read(cube_text(transform=lambda r, g, b: (r * 3 - 1, g, b)))
        np.testing.assert_allclose(c.sample(lut, [[0.25, 0.5, 0.5]]), [[-0.25, 0.5, 0.5]])

    def test_bad_cube_headers_and_values(self):
        cases = [
            cube_text("DOMAIN_MIN 1 0 0"),
            cube_text("DOMAIN_MAX nan 1 1"),
            cube_text("DOMAIN_MIN 0 0"),
            cube_text("LUT_3D_INPUT_RANGE 0 inf"),
            cube_text("LUT_3D_INPUT_RANGE 0 1\nDOMAIN_MIN 0 0 0"),
            cube_text("LUT_3D_SIZE 2"),
            cube_text("LUT_1D_SIZE 2"),
            cube_text("UNRECOGNIZED_HEADER 1 2"),
            cube_text().replace("0 0 0", "nan 0 0"),
            cube_text().replace("LUT_3D_SIZE 2", "LUT_3D_SIZE 1"),
            cube_text() + "DOMAIN_MAX 1 1 1\n",
        ]
        for text in cases:
            with self.subTest(text=text[:90]), self.assertRaises(ValueError):
                self.read(text)

    def test_original_binary_fixture_and_roundtrip(self):
        h, stock, tail = c.decode_table(c.STOCK)
        original = Path(c.STOCK).read_bytes()
        self.assertEqual(c.encode_table(h, c.compose(None, stock), tail), original)
        index = 15 * 256
        word = struct.unpack_from("<I", original, 0x80 + index * 4)[0]
        self.assertEqual((word & 1023, word >> 10 & 1023, word >> 20 & 1023), (1023, 0, 17))
        # 该断言仅锁定原厂字节布局，不宣称硬件输入域已确证。

    def test_quantization_error_bound(self):
        h, _, tail = c.decode_table(c.STOCK)
        values = np.random.default_rng(50).random((16, 16, 16, 3))
        encoded = c.encode_table(h, values, tail)
        words = np.frombuffer(encoded, dtype="<u4", count=4096, offset=128)
        channels = np.stack([words & 1023, words >> 10 & 1023, words >> 20 & 1023], axis=-1)
        self.assertLessEqual(float(np.max(np.abs(channels.reshape(values.shape) / 1023 - values))), 0.5 / 1023)

    def test_bad_binary_header_and_nan(self):
        h, stock, tail = c.decode_table(c.STOCK)
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / "bad.bin"
            path.write_bytes(bytes(c.TABLE_LEN))
            with self.assertRaises(ValueError):
                c.decode_table(path)
        stock[0, 0, 0, 0] = np.nan
        with self.assertRaises(ValueError):
            c.encode_table(h, stock, tail)


if __name__ == "__main__":
    unittest.main(verbosity=2)
