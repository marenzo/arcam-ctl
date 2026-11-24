"""Package license texts from the inputs selected by the release build."""

import io
import json
from pathlib import Path
import subprocess
import sys
import tarfile

build_log, metadata, output = map(Path, sys.argv[1:])
crates = json.loads(metadata.read_text())
idf_out = next(
    Path(event["out_dir"])
    for line in build_log.read_text().splitlines()
    if (event := json.loads(line))["reason"] == "build-script-executed"
    and "esp-idf-sys" in event["package_id"]
)
project = json.loads((idf_out / "build/project_description.json").read_text())
sdk = Path(project["idf_path"])
files = {}
inventory = []


def collect(root, label, recursive=True):
    """Recognize upstream license, copying, copyright and notice filenames."""
    paths = root.rglob("*") if recursive else root.iterdir()
    for path in paths:
        name = path.name.lower()
        if name.startswith(("license", "licence", "copying", "copyright", "notice")):
            if path.is_file() and path.suffix.lower() not in {".py", ".c", ".h", ".rs"}:
                files[f"{label}/{path.relative_to(root)}"] = path


files["project/LICENSE"] = Path("/src/LICENSE")
files["project/documentation-license.txt"] = Path("/src/docs/LICENSE")
files["viewer/three-r128/LICENSE"] = Path("/src/docs/wiring/three-r128/LICENSE")
collect(sdk, "esp-idf", recursive=False)
for path in (sdk / "LICENSES").glob("*"):
    if path.is_file():
        files[f"esp-idf/LICENSES/{path.name}"] = path
for component in project["build_component_paths"]:
    path = Path(component)
    if not path.name:
        continue  # IDF also lists the generated project itself.
    inventory.append(f"ESP-IDF component: {path.name}")
    collect(path, f"components/{path.name}")
for crate in crates["packages"]:
    if not crate["source"]:
        continue
    label = f'{crate["name"]}-{crate["version"]}'
    root = Path(crate["manifest_path"]).parent
    inventory.append(f'{label}  {crate["license"] or crate["license_file"]}  {crate["repository"] or ""}')
    collect(root, f"rust/{label}")
    if crate["license_file"]:
        path = root / crate["license_file"]
        files[f"rust/{label}/{path.name}"] = path

sysroot = Path(subprocess.check_output(["rustc", "--print", "sysroot"], text=True).strip())
for root in [sysroot / "share/doc/rust", sysroot / "lib/rustlib/src/rust"]:
    if root.is_dir():
        collect(root, "rust-toolchain", recursive=False)
        for path in (root / "LICENSES").glob("*"):
            if path.is_file():
                files[f"rust-toolchain/LICENSES/{path.name}"] = path

with tarfile.open(output, "w:gz") as archive:
    index = ("Arcam Radia bridge build dependencies and upstream notices\n"
             "The inventory includes build tools and target dependencies.\n\n"
             + "\n".join(sorted(inventory)) + "\n").encode()
    entry = tarfile.TarInfo("inventory.txt")
    entry.size = len(index)
    archive.addfile(entry, io.BytesIO(index))
    for label, source in sorted(files.items()):
        # Archive the notice contents even if a package ships a symlink.
        data = source.read_bytes()
        entry = tarfile.TarInfo(label)
        entry.size = len(data)
        archive.addfile(entry, io.BytesIO(data))
