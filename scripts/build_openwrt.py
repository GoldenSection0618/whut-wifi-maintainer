#!/usr/bin/env python3
"""Build one committed source snapshot using the official OpenWrt SDK."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import tempfile
import tomllib

PACKAGE = "whut-wifi-maintainer"


def run(*command: str, cwd: Path, capture: bool = False) -> str:
    result = subprocess.run(command, cwd=cwd, check=True, text=True,
                            stdout=subprocess.PIPE if capture else None)
    return result.stdout.strip() if capture else ""


def sha256(path: Path) -> str:
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def reject_symlinks(path: Path) -> None:
    """Check lexical ancestors before resolving; never delete through a link."""
    for parent in (path, *path.parents):
        if parent.is_symlink():
            raise ValueError(f"Symbolic links are not allowed: {parent}")


def extract_snapshot(archive: Path, destination: Path) -> None:
    with tarfile.open(archive) as source:
        for member in source.getmembers():
            path = Path(member.name)
            if path.is_absolute() or ".." in path.parts or not (member.isfile() or member.isdir()):
                raise ValueError("Source archive must contain only relative regular files/directories")
        source.extractall(destination, filter="data")


def rebuild_recipe(sdk: Path, snapshot: Path) -> Path:
    """Replace only this package's directory, after checking all deletion boundaries."""
    recipe = sdk / "package" / PACKAGE
    reject_symlinks(recipe)
    if recipe.resolve().parent != (sdk.resolve() / "package"):
        raise ValueError("Package directory escapes SDK/package")
    if recipe.exists():
        if not recipe.is_dir():
            raise ValueError("Package path is not a directory")
        for path in recipe.rglob("*"):
            if path.is_symlink():
                raise ValueError("Existing package directory contains a symbolic link")
    incoming = snapshot / "packaging/openwrt"
    for path in (incoming, *incoming.rglob("*")):
        reject_symlinks(path)
    # Copy first, so a failed copy leaves the existing recipe untouched.
    with tempfile.TemporaryDirectory(prefix=".whut-recipe-", dir=recipe.parent) as temporary:
        replacement = Path(temporary) / PACKAGE
        shutil.copytree(incoming, replacement)
        if recipe.exists():
            shutil.rmtree(recipe)
        replacement.rename(recipe)
    return recipe


def sdk_rustc(sdk: Path) -> Path:
    compilers = list((sdk / "staging_dir").glob("target-*/host/bin/rustc"))
    if len(compilers) != 1:
        raise RuntimeError("Expected one SDK target host Rust compiler")
    return compilers[0]


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sdk", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True, help="New, non-existing delivery directory")
    parser.add_argument("--package-release", type=int, required=True)
    parser.add_argument("--system-rust", action="store_true",
                        help="Use installed rustup toolchain instead of building rust/host")
    args = parser.parse_args()
    reject_symlinks(args.sdk.absolute())
    reject_symlinks(args.output.absolute())
    source = Path(__file__).resolve().parents[1]
    sdk, output = args.sdk.resolve(), args.output.resolve()
    if args.package_release < 1:
        parser.error("Package release must be positive")
    if output.exists():
        parser.error("Delivery directory already exists; previous artifacts must not be overwritten")
    if output.is_relative_to(source) or output.is_relative_to(sdk):
        parser.error("Delivery must be outside source and SDK")
    if not (sdk / "rules.mk").is_file() or not (sdk / "feeds/packages/lang/rust/rust-package.mk").is_file():
        parser.error("SDK or packages feed missing; run scripts/feeds update base packages in the SDK")
    if any(char.isspace() for char in str(sdk)):
        parser.error("OpenWrt SDK path must not contain whitespace")
    if run("git", "status", "--porcelain", cwd=source, capture=True):
        parser.error("Commit source changes before building; untracked files are not packaged")
    commit = run("git", "rev-parse", "HEAD", cwd=source, capture=True)
    # The manifest, recipe and program all come from this exact archive, never the working tree.
    with tempfile.TemporaryDirectory(prefix="whut-source-") as temporary:
        temporary = Path(temporary)
        archive = temporary / "source.tar.gz"
        run("git", "archive", "--format=tar.gz", f"--output={archive}", commit, cwd=source)
        snapshot = temporary / "snapshot"
        extract_snapshot(archive, snapshot)
        version = tomllib.loads((snapshot / "Cargo.toml").read_text())["package"]["version"]
        if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version):
            parser.error("Package version must be numeric major.minor.patch")
        sdk_lock = json.loads((snapshot / "packaging/openwrt/sdk.json").read_text())
        # OpenWrt expects an archive root named PKG_NAME-PKG_VERSION.
        archive_name = f"{PACKAGE}-{version}-{commit}.tar.gz"
        archive = temporary / archive_name
        run("git", "archive", "--format=tar.gz", f"--prefix={PACKAGE}-{version}/",
            f"--output={archive}", commit, cwd=source)
        reject_symlinks(sdk / "dl" / archive_name)
        (sdk / "dl").mkdir(exist_ok=True)
        shutil.copy2(archive, sdk / "dl" / archive_name)
        archive_digest = sha256(archive)
        recipe = rebuild_recipe(sdk, snapshot)
        (recipe / "source.mk").write_text(
            f"WHUT_VERSION:={version}\nWHUT_RELEASE:={args.package_release}\n"
            f"WHUT_SOURCE_COMMIT:={commit}\nWHUT_SOURCE_ARCHIVE:={archive_name}\n"
            f"WHUT_SOURCE_SHA256:={archive_digest}\n")
    if not args.system_rust:
        run("./scripts/feeds", "install", "rust", cwd=sdk)
    run("./scripts/feeds", "install", "ca-bundle", cwd=sdk)
    config_path = sdk / ".config"
    reject_symlinks(config_path)
    config = config_path.read_text() if config_path.exists() else ""
    names = ("ALL", "ALL_KMODS", "ALL_NONSHARED", f"PACKAGE_{PACKAGE}")
    config = "\n".join(line for line in config.splitlines()
                       if not any(line.startswith(f"CONFIG_{name}=") or
                                  line == f"# CONFIG_{name} is not set" for name in names))
    config_path.write_text(config + f"\n# CONFIG_ALL is not set\n# CONFIG_ALL_KMODS is not set\n"
                          f"# CONFIG_ALL_NONSHARED is not set\nCONFIG_PACKAGE_{PACKAGE}=m\n")
    build_args = []
    rust_bin = None
    if args.system_rust:
        rust_path = shutil.which("rustc")
        if not rust_path:
            parser.error("rustc is not installed")
        rust_bin = Path(rust_path).absolute().parent
        if not (rust_bin / "cargo").is_file() or any(c.isspace() for c in str(rust_bin)):
            parser.error("Rust bin directory must contain cargo and have no whitespace")
        build_args = ["WHUT_SYSTEM_RUST=1", f"WHUT_RUST_BIN={rust_bin}"]
    run("make", "defconfig", *build_args, cwd=sdk)
    run("make", f"package/{PACKAGE}/clean", *build_args, cwd=sdk)
    run("make", f"package/{PACKAGE}/compile", "-j2", "V=s", *build_args, cwd=sdk)
    packages = sorted((sdk / "bin").rglob(f"{PACKAGE}-{version}-r{args.package_release}.apk"))
    if len(packages) != 1:
        raise RuntimeError(f"Expected one APK for requested release, found {len(packages)}")
    architecture = re.search(r'^CONFIG_TARGET_ARCH_PACKAGES="([^"]+)"$', config_path.read_text(), re.M)
    if not architecture:
        raise RuntimeError("SDK did not report its package architecture")
    with tempfile.TemporaryDirectory(prefix="whut-apk-") as directory:
        extracted = Path(directory)
        run(str(sdk / "staging_dir/host/bin/apk"), "extract", "--allow-untrusted", "--no-chown",
            "--destination", str(extracted), str(packages[0]), cwd=sdk)
        binary_data = (extracted / "usr/bin" / PACKAGE).read_bytes()
    version_metadata = sdk / "include/version.mk"
    sdk_version = re.search(r"^VERSION_NUMBER:.*,(\d+\.\d+\.\d+)\)$", version_metadata.read_text(), re.M)
    if not sdk_version:
        raise RuntimeError("SDK release metadata was not recognized")
    compilers = list((sdk / "staging_dir").glob("toolchain-*/bin/*-openwrt-linux-musl-gcc"))
    if len(compilers) != 1:
        raise RuntimeError("Expected one SDK C toolchain")
    rustc = str(rust_bin / "rustc" if rust_bin else sdk_rustc(sdk))
    manifest = {
        "version": version, "package_release": args.package_release, "source_commit": commit,
        "source_archive_sha256": archive_digest, "recipe_source_commit": commit,
        "architecture": architecture[1], "sdk_version": sdk_version[1], "sdk_lock": sdk_lock,
        "sdk_version_metadata_sha256": sha256(version_metadata),
        "rustc": run(rustc, "--version", "--verbose", cwd=sdk, capture=True),
        "linker": run(str(compilers[0]), "--version", cwd=sdk, capture=True),
        "feeds": {feed: run("git", "rev-parse", "HEAD", cwd=sdk / "feeds" / feed, capture=True)
                  for feed in ("base", "packages")},
        "apk_binary_sha256": hashlib.sha256(binary_data).hexdigest(),
    }
    # Exclusive creation prevents accidental overwrite even if another build raced us.
    output.mkdir(parents=True, exist_ok=False)
    artifact, binary = output / packages[0].name, output / PACKAGE
    shutil.copy2(packages[0], artifact)
    binary.write_bytes(binary_data)
    binary.chmod(0o755)
    manifest["artifacts"] = [{"name": item.name, "bytes": item.stat().st_size, "sha256": sha256(item)}
                             for item in (artifact, binary)]
    (output / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    (output / "SHA256SUMS").write_text("".join(
        f"{item['sha256']}  {item['name']}\n" for item in manifest["artifacts"]))
    print(json.dumps(manifest, indent=2))


if __name__ == "__main__":
    main()
