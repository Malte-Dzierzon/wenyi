"""Offline contract for local package rebuilds."""

from __future__ import annotations

import importlib.util
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]


def script(name):
    spec = importlib.util.spec_from_file_location(name, ROOT / "scripts" / f"{name}.py")
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def test_sidecar_rebuilds_local_packages_instead_of_reusing_cached_wheels(monkeypatch):
    sidecar = script("desktop_sidecar")
    commands = []
    monkeypatch.setattr(sidecar.sys, "argv", ["desktop_sidecar.py"])
    monkeypatch.setattr(
        sidecar.subprocess, "run", lambda command, **kwargs: commands.append(command)
    )
    sidecar.main()
    sync = commands[0]
    assert sync[:2] == ["uv", "sync"]
    for package in ("wenyi-core", "wenyi-backend", "wenyi-desktop"):
        assert ("--reinstall-package", package) in set(zip(sync, sync[1:]))
