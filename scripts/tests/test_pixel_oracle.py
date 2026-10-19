"""编译并执行纯 C 像素/时序反例；不依赖 macOS SDK、产品进程或图像采集授权。"""

import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


class PixelOracleTests(unittest.TestCase):
    """临时产物只由本测试拥有；每次重编译当前源，避免复用旧二进制证据。"""

    def test_exact_pixel_and_600_frame_replay_contracts(self):
        # clang 与普通 C17 编译器均可用；缺少编译器时明确跳过，不伪造通过。
        compiler = shutil.which("clang") or shutil.which("cc")
        if compiler is None:
            # 这是构建环境缺项，与像素、原生运行或捕获成功无关。
            self.skipTest("requires a C17 compiler for the portable pure fixtures")
        source = Path(__file__).resolve().parents[1] / "macos_frame_probe"
        with tempfile.TemporaryDirectory(prefix="omni-pixel-oracle-pure-") as directory:
            # 严格 warning 采用所有平台支持的 C 检查，不链接任何 Apple 框架。
            binary = Path(directory) / "pixel-oracle-tests"
            platform_flags = []
            if sys.platform == "darwin":
                # macOS 仍按产品的 14.0 build floor 编译，不把本机运行视为 14.0 验收。
                platform_flags.append("-mmacosx-version-min=14.0")
            built = subprocess.run(
                [compiler, "-std=c17", "-Wall", "-Wextra", "-Werror", "-Wconversion",
                 "-Wsign-conversion", "-pedantic", *platform_flags, str(source / "pixel_oracle.c"),
                 str(source / "pixel_oracle_tests.c"), "-o", str(binary)],
                capture_output=True, timeout=30, check=False,
            )
            self.assertEqual(built.returncode, 0, built.stderr.decode(errors="replace"))
            # 测试有固定操作数和外部期限；只输出检查数和明确的零 capture 声明。
            run = subprocess.run([str(binary)], capture_output=True, timeout=10, check=False)
            self.assertEqual(run.returncode, 0, run.stderr.decode(errors="replace"))
            summary = json.loads(run.stdout)
            self.assertEqual(summary["pure_cases"], 16)
            self.assertGreaterEqual(summary["pure_checks"], 5000)
            self.assertEqual(summary["max_replay_frames"], 600)
            self.assertGreater(summary["owned_scalar_bytes"], 0)
            self.assertEqual(summary["peak_owned_pixel_bytes"], 261120)
            self.assertEqual(summary["capture_invocations"], 0)
            self.assertEqual(summary["native_audits"], 0)
            self.assertFalse(summary["latency_acceptance"])


if __name__ == "__main__":
    # 标准 unittest 入口仅运行本文件纯测试，不启动产品或其他 native harness。
    unittest.main()
