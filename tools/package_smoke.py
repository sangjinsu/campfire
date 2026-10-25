#!/usr/bin/env python3
"""Check checkout and packaged Campfire crates from isolated consumers.

The script never publishes. Package archives, SHA-256 manifests, command logs,
and a result summary are retained in --output. By default, build work uses a
fresh temporary target; --target-dir and CARGO_TARGET_DIR override it.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import tarfile
import tempfile
import tomllib
from collections.abc import Mapping
from pathlib import Path, PurePosixPath


ROOT = Path(__file__).resolve().parents[1]
CRATES = ("campfire-runtime", "campfire-arena", "campfire-server")
VERSION = "0.1.0"
LICENSE_SHA256 = "51089a80b7758f0afa852cae8dee25073a8eb5e42fc8a185ba8c91cb84874b9f"
CONSUMER = r'''use axum::{Router, routing::get};
use campfire_arena::ArenaModule;
use campfire_runtime::{Campfire, Config, Game, GameContext, GameError, GameModule, RuntimeOptions};
use serde_json::{Value, json};
use std::time::Duration;

struct LocalModule;
struct LocalGame;

impl GameModule for LocalModule {
    fn create(&self, _settings: &Value, _context: &GameContext) -> Result<Box<dyn Game>, GameError> {
        Ok(Box::new(LocalGame))
    }
}

impl Game for LocalGame {
    fn player_joined(&mut self, _player_id: &str) {}
    fn player_left(&mut self, _player_id: &str) {}
    fn player_connection_changed(&mut self, _player_id: &str, _connected: bool) {}
    fn validate_input(&self, _player_id: &str, _payload: &Value) -> Result<(), GameError> { Ok(()) }
    fn apply_input(&mut self, _player_id: &str, _payload: &Value) {}
    fn tick(&mut self, _dt: Duration) {}
    fn snapshot(&self) -> Value { json!({"status": "ok"}) }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = Campfire::new_with_options(Config::default(), RuntimeOptions::default())?;
    runtime.register_game("local", LocalModule)?;
    runtime.register_game("arena", ArenaModule)?;
    let observer = runtime.observer();
    let snapshot = observer.snapshot().ok_or("observer unavailable")?;
    assert_eq!(snapshot.game_ids, vec!["arena", "local"]);
    let _app = Router::new().route("/probe", get(|| async { "ok" })).merge(runtime.router());
    let report = runtime.shutdown_with_timeout(Duration::from_secs(2)).await;
    assert!(report.completed);
    println!("consumer_ok games=2 shutdown_completed={}", report.completed);
    Ok(())
}
'''


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def retain_lockfile(source_dir: Path, evidence_dir: Path, label: str) -> dict[str, str]:
    source = source_dir / "Cargo.lock"
    if not source.is_file():
        raise RuntimeError(f"missing consumer lockfile: {source}")
    retained = evidence_dir / f"{label}.Cargo.lock"
    shutil.copy2(source, retained)
    return {"file": str(retained.relative_to(evidence_dir.parent)), "sha256": sha256(retained)}


def toml_string(value: Path | str) -> str:
    return json.dumps(str(value))


def choose_target_dir(explicit: Path | None, environ: Mapping[str, str], temporary: Path) -> tuple[Path, str]:
    if explicit is not None:
        return explicit.expanduser().resolve(), "argument"
    inherited = environ.get("CARGO_TARGET_DIR", "")
    if inherited.strip():
        return Path(inherited).expanduser().resolve(), "environment"
    return (temporary / "target").resolve(), "temporary"


def package_members(archive: Path, name: str) -> list[str]:
    """Return checked archive members; reject links and path traversal."""
    prefix = f"{name}-{VERSION}/"
    members = []
    with tarfile.open(archive, "r:gz") as source:
        for member in source:
            path = PurePosixPath(member.name)
            if (
                not member.name.startswith(prefix)
                or path.is_absolute()
                or ".." in path.parts
                or not (member.isfile() or member.isdir())
            ):
                raise ValueError(f"unsafe package member: {member.name}")
            if member.isfile():
                members.append(member.name)
    if not members:
        raise ValueError(f"empty package: {archive}")
    return sorted(members)


def extract_package(archive: Path, destination: Path, name: str) -> Path:
    members = package_members(archive, name)
    with tarfile.open(archive, "r:gz") as source:
        for member in source:
            target = destination / member.name
            if member.isdir():
                target.mkdir(parents=True, exist_ok=True)
            elif member.name in members:
                target.parent.mkdir(parents=True, exist_ok=True)
                with source.extractfile(member) as input_file, target.open("wb") as output_file:
                    shutil.copyfileobj(input_file, output_file)
    return destination / f"{name}-{VERSION}"


def local_patch_config(packages: dict[str, Path]) -> str:
    """Override unpublished crates.io versions with the extracted archives."""
    lines = ["[patch.crates-io]"]
    for name in ("campfire-runtime", "campfire-arena"):
        if name in packages:
            lines.append(f"{name} = {{ path = {toml_string(packages[name])} }}")
    return "\n".join(lines) + "\n"


def validate_package_source(package: Path, name: str, members: list[str]) -> dict[str, str]:
    manifest = tomllib.loads((package / "Cargo.toml").read_text())
    details = manifest["package"]
    expected = {
        "name": name,
        "version": VERSION,
        "rust-version": "1.96",
        "license": "MIT",
        "repository": "https://github.com/sangjinsu/campfire",
    }
    for key, value in expected.items():
        if details.get(key) != value:
            raise ValueError(f"{name} package {key} must be {value!r}; found {details.get(key)!r}")
    if not details.get("description"):
        raise ValueError(f"{name} package has no description")
    canonical_license = (ROOT / "LICENSE").read_bytes()
    if hashlib.sha256(canonical_license).hexdigest() != LICENSE_SHA256:
        raise ValueError("canonical MIT LICENSE bytes changed; review and update the expected hash")
    if (package / "LICENSE").read_bytes() != canonical_license:
        raise ValueError(f"{name} package MIT LICENSE differs from canonical text")
    for member in members:
        parts = PurePosixPath(member).parts[1:]
        if any(part in {".git", "target", "artifacts", ".env"} for part in parts):
            raise ValueError(f"{name} package contains excluded workspace data: {member}")
    for dependency in ("campfire-runtime", "campfire-arena"):
        if dependency == name or dependency not in manifest.get("dependencies", {}):
            continue
        spec = manifest["dependencies"][dependency]
        if spec.get("version") != VERSION or "path" in spec:
            raise ValueError(f"{name} normalized dependency {dependency} is not registry-ready")
    return {key: str(value) for key, value in expected.items()} | {"description": details["description"]}


def consumer_manifest(packages: dict[str, Path]) -> str:
    lines = [
        '[package]',
        'name = "campfire-package-consumer"',
        'version = "0.1.0"',
        'edition = "2024"',
        'rust-version = "1.96"',
        '',
        '[dependencies]',
        f'campfire-runtime = {{ path = {toml_string(packages["campfire-runtime"])}, version = "{VERSION}" }}',
        f'campfire-arena = {{ path = {toml_string(packages["campfire-arena"])}, version = "{VERSION}" }}',
        'axum = "0.8"',
        'tokio = { version = "1", features = ["macros", "rt-multi-thread"] }',
        'serde_json = "1"',
    ]
    return "\n".join(lines) + "\n"


def run_command(command: list[str], cwd: Path, env: dict[str, str], log: Path) -> None:
    with log.open("w") as stream:
        stream.write(f"cwd: {cwd}\ncommand: {' '.join(command)}\n\n")
        stream.flush()
        try:
            result = subprocess.run(command, cwd=cwd, env=env, stdout=stream, stderr=subprocess.STDOUT, check=False)
        except OSError as error:
            stream.write(f"\nprocess error: {error}\n")
            raise RuntimeError(f"command could not start; see {log}") from error
        stream.write(f"\nexit_code: {result.returncode}\n")
    if result.returncode:
        raise RuntimeError(f"command failed ({result.returncode}); see {log}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True, help="New directory for logs and package evidence")
    parser.add_argument("--target-dir", type=Path, help="Cargo target; default CARGO_TARGET_DIR or a fresh temporary target")
    parser.add_argument("--online", action="store_true", help="Allow Cargo to fetch external crates")
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    offline = [] if args.online else ["--offline"]
    repository_lock_sha256 = sha256(ROOT / "Cargo.lock")
    summary: dict[str, object] = {
        "status": "running", "version": VERSION, "msrv": "pending", "target_dir": "pending", "target_source": "pending",
        "archive_mode": "runtime verified; arena/server archive without lockfile, then compiled from extracted archive",
        "internal_source_override": "temporary [patch.crates-io] paths to extracted package archives",
        "checkout_consumer": "pending", "archive_consumer": "pending", "server_archive_check": "pending",
        "archives": {}, "metadata": {}, "consumer_lockfiles": {},
        "repository_lockfile": {"before_sha256": repository_lock_sha256, "after_sha256": "pending", "unchanged": False},
        "lockfile_exclusion_reason": "unpublished internal crate versions are absent from crates.io; root Cargo.lock stays unchanged",
        "verification_limits": [
            "arena/server archives omit Cargo.lock; reproducible packaged resolution and cargo install --locked are unverified",
            "crates.io publication and public registry download are unverified",
        ],
        "registry_publish": "not attempted",
    }
    try:
        with tempfile.TemporaryDirectory(prefix="campfire-package-smoke-") as temporary:
            temp = Path(temporary)
            target, target_source = choose_target_dir(args.target_dir, os.environ, temp)
            summary["target_dir"] = str(target)
            summary["target_source"] = target_source
            env = os.environ.copy()
            env["CARGO_TARGET_DIR"] = str(target)
            env["CARGO_BUILD_JOBS"] = "4"
            package_dir = output / "packages"
            package_dir.mkdir()
            extracted = {}
            for name in CRATES:
                command = ["cargo", "package", "-p", name, "--allow-dirty", "--locked"]
                if name == "campfire-runtime":
                    # Runtime's normal package verification checks its source independently.
                    log = output / "package-runtime.log"
                else:
                    # Offer prior archives as local patches; skip package lockfile generation.
                    config = temp / f"package-{name}-config.toml"
                    config.write_text(local_patch_config(extracted))
                    command += ["--no-verify", "--exclude-lockfile", "--config", str(config)]
                    log = output / f"package-{name}.log"
                run_command(command + offline, ROOT, env, log)
                archive = target / "package" / f"{name}-{VERSION}.crate"
                if not archive.is_file():
                    raise RuntimeError(f"missing archive: {archive}")
                retained = package_dir / archive.name
                shutil.copy2(archive, retained)
                members = package_members(retained, name)
                lockfile_included = f"{name}-{VERSION}/Cargo.lock" in members
                if lockfile_included != (name == "campfire-runtime"):
                    raise ValueError(f"{name} package lockfile inclusion differs from the expected archive mode")
                (package_dir / f"{name}.files.txt").write_text("\n".join(members) + "\n")
                summary["archives"][name] = {
                    "sha256": sha256(retained), "files": len(members), "lockfile_included": lockfile_included,
                }
                extracted[name] = extract_package(retained, temp / "extracted", name)
                summary["metadata"][name] = validate_package_source(extracted[name], name, members)
            summary["msrv"] = "verified 1.96 in all normalized package manifests"
            lockfile_dir = output / "lockfiles"
            lockfile_dir.mkdir()
            checkout = temp / "checkout-consumer"
            checkout.joinpath("src").mkdir(parents=True)
            (checkout / "Cargo.toml").write_text(consumer_manifest({
                name: ROOT / "crates" / name for name in CRATES[:2]
            }))
            (checkout / "src/main.rs").write_text(CONSUMER)
            run_command(["cargo", "generate-lockfile", *offline], checkout, env, output / "checkout-lock.log")
            summary["consumer_lockfiles"]["checkout"] = retain_lockfile(checkout, lockfile_dir, "checkout")
            run_command(["cargo", "run", "--locked", *offline], checkout, env, output / "checkout-consumer.log")
            summary["checkout_consumer"] = "passed"

            archive_consumer = temp / "archive-consumer"
            archive_consumer.joinpath("src").mkdir(parents=True)
            archive_consumer.joinpath(".cargo").mkdir()
            (archive_consumer / ".cargo/config.toml").write_text(local_patch_config(extracted))
            (archive_consumer / "Cargo.toml").write_text(consumer_manifest(extracted))
            (archive_consumer / "src/main.rs").write_text(CONSUMER)
            run_command(["cargo", "generate-lockfile", *offline], archive_consumer, env, output / "archive-lock.log")
            summary["consumer_lockfiles"]["archive"] = retain_lockfile(archive_consumer, lockfile_dir, "archive")
            run_command(["cargo", "run", "--locked", *offline], archive_consumer, env, output / "archive-consumer.log")
            summary["archive_consumer"] = "passed"

            # A separate manifest check compiles the binary crate from its normalized archive.
            # CARGO_HOME is untouched; only this temp tree supplies unpublished internal crates.
            extracted["campfire-server"].joinpath(".cargo").mkdir()
            (extracted["campfire-server"] / ".cargo/config.toml").write_text(local_patch_config(extracted))
            run_command(
                ["cargo", "generate-lockfile", *offline], extracted["campfire-server"], env,
                output / "server-archive-lock.log",
            )
            summary["consumer_lockfiles"]["server_archive"] = retain_lockfile(
                extracted["campfire-server"], lockfile_dir, "server-archive",
            )
            run_command(
                ["cargo", "check", "--manifest-path", str(extracted["campfire-server"] / "Cargo.toml"), "--locked", *offline],
                extracted["campfire-server"], env, output / "server-archive-check.log",
            )
            summary["server_archive_check"] = "passed"
        summary["status"] = "passed"
    except (RuntimeError, ValueError, OSError) as error:
        summary["status"] = "failed"
        summary["error"] = str(error)
    repository_lock_after_sha256 = sha256(ROOT / "Cargo.lock")
    summary["repository_lockfile"]["after_sha256"] = repository_lock_after_sha256
    summary["repository_lockfile"]["unchanged"] = repository_lock_after_sha256 == repository_lock_sha256
    if not summary["repository_lockfile"]["unchanged"]:
        summary["status"] = "failed"
        summary["error"] = "repository Cargo.lock changed during package smoke"
    (output / "summary.json").write_text(json.dumps(summary, ensure_ascii=False, indent=2) + "\n")
    print(json.dumps(summary, ensure_ascii=False, indent=2))
    return 0 if summary["status"] == "passed" else 1


if __name__ == "__main__":
    raise SystemExit(main())
