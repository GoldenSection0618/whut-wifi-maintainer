#!/usr/bin/env python3
"""Build a committed source snapshot with the official OpenWrt SDK and Rust helper."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil
import subprocess
import tomllib


def run(*command: str, cwd: Path, capture: bool = False) -> str:
    result = subprocess.run(command, cwd=cwd, check=True, text=True,
                            stdout=subprocess.PIPE if capture else None)
    return result.stdout.strip() if capture else ""


def sha256(path: Path) -> str:
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sdk", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--system-rust", action="store_true",
                        help="Use installed rustup toolchain and target instead of building rust/host")
    args = parser.parse_args()
    source = Path(__file__).resolve().parents[1]
    sdk, output = args.sdk.resolve(), args.output.resolve()
    if not (sdk / "rules.mk").is_file() or not (sdk / "feeds/packages/lang/rust/rust-package.mk").is_file():
        parser.error("SDK or packages feed missing; run scripts/feeds update base packages in the SDK")
    if any(char.isspace() for char in str(sdk)):
        parser.error("OpenWrt SDK path must not contain whitespace")
    if run("git", "status", "--porcelain", cwd=source, capture=True):
        parser.error("Commit source changes before building; untracked files are not packaged")
    commit = run("git", "rev-parse", "HEAD", cwd=source, capture=True)
    version = tomllib.loads((source / "Cargo.toml").read_text())["package"]["version"]
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version):
        parser.error("Package version must be a numeric major.minor.patch")
    package = "whut-wifi-maintainer"
    archive = sdk / "dl" / f"{package}-{version}.tar.gz"
    archive.parent.mkdir(parents=True, exist_ok=True)
    run("git", "archive", "--format=tar.gz", f"--prefix={package}-{version}/",
        f"--output={archive}", commit, cwd=source)
    recipe = sdk / "package" / package
    shutil.copytree(source / "packaging/openwrt", recipe, dirs_exist_ok=True)
    (recipe / "source.mk").write_text(
        f"WHUT_VERSION:={version}\nWHUT_SOURCE_SHA256:={sha256(archive)}\n")
    if not args.system_rust:
        run("./scripts/feeds", "install", "rust", cwd=sdk)
    run("./scripts/feeds", "install", "ca-bundle", cwd=sdk)
    config_path = sdk / ".config"
    config = config_path.read_text() if config_path.exists() else ""
    names = ("ALL", "ALL_KMODS", "ALL_NONSHARED", f"PACKAGE_{package}")
    config = "\n".join(line for line in config.splitlines()
                       if not any(line.startswith(f"CONFIG_{name}=") or
                                  line == f"# CONFIG_{name} is not set" for name in names))
    config_path.write_text(config + f"\n# CONFIG_ALL is not set\n# CONFIG_ALL_KMODS is not set\n"
                          f"# CONFIG_ALL_NONSHARED is not set\nCONFIG_PACKAGE_{package}=m\n")
    build_args = ["WHUT_SYSTEM_RUST=1"] if args.system_rust else []
    run("make", "defconfig", *build_args, cwd=sdk)
    run("make", f"package/{package}/clean", *build_args, cwd=sdk)
    run("make", f"package/{package}/compile", "-j2", "V=s", *build_args, cwd=sdk)
    packages = sorted((sdk / "bin").rglob(f"{package}-{version}-r*.apk"))
    if len(packages) != 1:
        raise RuntimeError(f"Expected one APK, found {len(packages)}")
    config = config_path.read_text()
    architecture = re.search(r'^CONFIG_TARGET_ARCH_PACKAGES="([^"]+)"$', config, re.M)
    if not architecture:
        raise RuntimeError("SDK did not report its package architecture")
    binaries = list((sdk / "build_dir").glob(f"target-*/{package}-{version}/.pkgdir/{package}/usr/bin/{package}"))
    if len(binaries) != 1:
        raise RuntimeError(f"Expected one installed binary, found {len(binaries)}")
    version_metadata = (sdk / "include/version.mk").read_text()
    sdk_version = re.search(r"^VERSION_NUMBER:.*,(\d+\.\d+\.\d+)\)$", version_metadata, re.M)
    if not sdk_version:
        raise RuntimeError("SDK release metadata was not recognized")
    output.mkdir(parents=True, exist_ok=True)
    artifact = output / packages[0].name
    binary = output / package
    shutil.copy2(packages[0], artifact)
    shutil.copy2(binaries[0], binary)
    manifest = {
        "version": version, "source_commit": commit, "source_archive_sha256": sha256(archive),
        "architecture": architecture[1],
        "sdk_version": sdk_version[1],
        "sdk_version_metadata_sha256": sha256(sdk / "include/version.mk"),
        "rustc": run("rustc", "--version", cwd=sdk, capture=True) if args.system_rust else "SDK rust/host",
        "artifacts": [{"name": item.name, "bytes": item.stat().st_size, "sha256": sha256(item)}
                      for item in [artifact, binary]],
    }
    (output / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    (output / "SHA256SUMS").write_text("".join(
        f"{item['sha256']}  {item['name']}\n" for item in manifest["artifacts"]))
    print(json.dumps(manifest, indent=2))


if __name__ == "__main__":
    main()
