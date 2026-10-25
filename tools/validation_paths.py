"""Portable executable and Cargo artifact paths for validation runners."""
from __future__ import annotations

import os
from pathlib import Path
import shutil

ROOT = Path(__file__).resolve().parents[1]


def resolve_target_dir(explicit: str | None) -> Path:
    value = explicit or os.environ.get("CARGO_TARGET_DIR")
    return Path(value).expanduser().resolve() if value else ROOT / "target"


def resolve_executable(explicit: str | None, env_name: str, executable: str) -> Path:
    value = explicit or os.environ.get(env_name) or executable
    found = shutil.which(value)
    if found is None:
        raise FileNotFoundError(f"Required executable {executable!r} was not found (option, {env_name}, or PATH)")
    return Path(found).resolve()


def server_binary(target_dir: Path) -> Path:
    return target_dir / "debug/campfire-server"


def account_binary(target_dir: Path) -> Path:
    return target_dir / "debug/examples/account_server"
