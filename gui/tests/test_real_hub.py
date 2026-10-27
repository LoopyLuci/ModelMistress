"""The client against a real hub. Runs only when MM_HUB_EXE names a model-mistress binary; it starts that binary in a
throwaway home, reads it, and stops it again."""

from __future__ import annotations

import os
import subprocess
import time

import pytest

from mm_gui.hub import Hub

EXE = os.environ.get("MM_HUB_EXE", "")


@pytest.mark.skipif(not EXE, reason="set MM_HUB_EXE to a model-mistress binary to test against a real hub")
def test_the_client_reads_a_real_hub(tmp_path):
    proc = subprocess.Popen([EXE, "serve", "--home", str(tmp_path), "--port", "0"],
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        end = time.monotonic() + 60
        while not (tmp_path / "control.json").exists() and time.monotonic() < end:
            time.sleep(0.2)
        hub = Hub.find(tmp_path)

        assert "version" in hub.status()
        assert isinstance(hub.models(), list)
        assert isinstance(hub.loaded(), list)
        assert isinstance(hub.config(), dict)
    finally:
        if (tmp_path / "control.json").exists():
            Hub.find(tmp_path).stop()
        proc.wait(timeout=60)
