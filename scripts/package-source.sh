#!/bin/sh
set -eu
ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
PROMPTGEN_SOURCE=${PROMPTGEN_SOURCE:-"$ROOT/../promptGen"}
PROMPTGEN_SOURCE=$(CDPATH= cd -- "$PROMPTGEN_SOURCE" && pwd -P)
export PROMPTGEN_SOURCE
OUT=${1:-"$ROOT/../upagent-system-final-2026-10-03.tar.gz"}
case "$OUT" in
  /*) ;;
  *) OUT=$(CDPATH= cd -- "$(dirname -- "$OUT")" && pwd)/$(basename -- "$OUT") ;;
esac
TMP="${OUT%.gz}.tmp.tar"
for path in "$OUT" "$OUT.sha256" "$TMP"; do
  if [ -e "$path" ]; then
    printf 'refusing to replace existing package output: %s\n' "$path" >&2
    exit 1
  fi
done
python3 "$ROOT/scripts/refresh-integrity.py"
python3 "$ROOT/scripts/verify-static.py"
# Deterministic source archive: stable order/owner/mtime and gzip timestamp removed.
# Python is already required by the integrity gates and works with macOS BSD tar.
python3 - "$ROOT" "$PROMPTGEN_SOURCE" "$TMP" <<'PY'
import calendar
from pathlib import Path
import sys
import tarfile

root = Path(sys.argv[1])
promptgen_root = Path(sys.argv[2])
sys.path.insert(0, str(root / 'scripts'))
from source_integrity import IGNORED_PARTS, load_promptgen_source
epoch = calendar.timegm((2026, 10, 3, 0, 0, 0))

def stable_metadata(info):
    info.uid = info.gid = 0
    info.uname = info.gname = ''
    info.mtime = epoch
    info.pax_headers = {}
    return info

def add_source(archive, path):
    archive.add(path, arcname=str(path.relative_to(root.parent)),
                recursive=False, filter=stable_metadata)
    if path.is_dir() and not path.is_symlink():
        for child in sorted(path.iterdir()):
            if child.name not in IGNORED_PARTS:
                add_source(archive, child)

source_snapshot, _ = load_promptgen_source(promptgen_root)

with tarfile.open(sys.argv[3], 'w', format=tarfile.PAX_FORMAT) as archive:
    add_source(archive, root)
    for path in source_snapshot.source_files(promptgen_root):
        archive.add(
            path,
            arcname=str(Path('promptGen') / path.relative_to(promptgen_root)),
            recursive=False,
            filter=stable_metadata,
        )
    verification = promptgen_root / 'VERIFICATION.json'
    archive.add(
        verification,
        arcname='promptGen/VERIFICATION.json',
        recursive=False,
        filter=stable_metadata,
    )
PY
gzip -n -9 -c "$TMP" > "$OUT"
python3 -c 'from pathlib import Path; import sys; Path(sys.argv[1]).unlink()' "$TMP"
SHA=$(sha256sum "$OUT" | awk '{print $1}')
printf '%s  %s\n' "$SHA" "$(basename -- "$OUT")" > "$OUT.sha256"
printf 'PACKAGE_CREATED\n%s\n%s\n' "$OUT" "$SHA"
