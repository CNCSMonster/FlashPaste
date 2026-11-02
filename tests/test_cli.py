"""Offline CLI tests; no production URL or credentials are used."""
import importlib.util
import io
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch


spec = importlib.util.spec_from_file_location(
    "flashpaste_cli", Path(__file__).resolve().parents[1] / "flashpaste-cli.py"
)
cli = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cli)


class UntitledCreateTests(unittest.TestCase):
    def create(self, *arguments):
        with patch.dict(os.environ, {"FLASHPASTE_TOKEN": "test-only"}), \
                patch("sys.argv", ["flashpaste-cli.py", "create", *arguments]), \
                patch("sys.stdin", io.StringIO("不要用正文作为标题")), \
                patch("sys.stdout", io.StringIO()), \
                patch.object(cli, "request", return_value={"id": "test-id"}) as request:
            cli.main()
        return request.call_args.args[2]

    def test_stdin_defaults_to_no_title(self):
        self.assertIsNone(self.create()["title"])

    def test_import_does_not_use_filename_as_title(self):
        with tempfile.TemporaryDirectory(prefix="flashpaste-cli-", dir="/tmp/opencode") as directory:
            file = Path(directory) / "不应作为标题.md"
            file.write_text("# 文件正文", encoding="utf-8")
            payload = self.create("--file", str(file))
            self.assertIsNone(payload["title"])
            self.assertEqual(payload["content_type"], "markdown")

    def test_blank_title_is_absent_and_explicit_title_is_preserved(self):
        self.assertIsNone(self.create("--title", " \t ")["title"])
        self.assertEqual(self.create("--title", "  自定义标题  ")["title"], "自定义标题")

    def test_import_preserves_bom_and_crlf(self):
        with tempfile.TemporaryDirectory(prefix="flashpaste-cli-", dir="/tmp/opencode") as directory:
            file = Path(directory) / "original.html"
            original = b"\xef\xbb\xbf<h1>Original</h1>\r\n<p>Body</p>\r\n"
            file.write_bytes(original)
            self.assertEqual(self.create("--file", str(file))["content"].encode("utf-8"), original)

    def test_xdg_config_toml_loading(self):
        with tempfile.TemporaryDirectory(prefix="flashpaste-xdg-", dir="/tmp/opencode") as directory:
            cfg_dir = Path(directory) / "flashpaste"
            cfg_dir.mkdir()
            cfg_file = cfg_dir / "config.toml"
            cfg_file.write_text('url = "https://custom.workers.dev"\ntoken = "toml-token"\n', encoding="utf-8")
            with patch.dict(os.environ, {"XDG_CONFIG_HOME": directory}, clear=True):
                url, token = cli.get_config()
                self.assertEqual(url, "https://custom.workers.dev")
                self.assertEqual(token, "toml-token")


if __name__ == "__main__":
    unittest.main()
