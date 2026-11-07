#!/usr/bin/env python3
"""Assemble the reviewed native-library closure in a disposable image rootfs."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess


def readelf(path, option):
    return subprocess.check_output(['readelf', option, '--wide', str(path)], text=True)


def needed(path):
    return set(re.findall(r'\(NEEDED\).*?\[(.*?)\]', readelf(path, '--dynamic')))


def prune(root, executable, report):
    root = root.resolve()
    status = root / 'var/lib/dpkg/status.d'
    removed = {}
    owners = {}
    manifests = {}
    for manifest in status.glob('*.md5sums'):
        rows = [line.split(maxsplit=1) for line in manifest.read_text().splitlines() if line]
        manifests[manifest] = rows
        for digest, name in rows:
            owners[name] = digest

    def remove(path):
        name = str(path.relative_to(root))
        if path.is_symlink():
            removed[name] = {'symlink': str(path.readlink())}
        else:
            content = path.read_bytes()
            if name in owners:
                assert hashlib.md5(content, usedforsecurity=False).hexdigest() == owners[name], name
            removed[name] = {'sha256': hashlib.sha256(content).hexdigest()}
        path.unlink()

    # files/dns NSS are built into this reviewed libc, and the image enforces C locale.
    assert 'Version: 2.41-12+deb13u4\n' in (status / 'libc6').read_text()
    symbols = readelf(executable, '--dyn-syms')
    imports = {line.split()[7].split('@')[0] for line in symbols.splitlines()
               if len(line.split()) >= 8 and line.split()[6] == 'UND'}
    locale_imports = {'iconv', 'iconv_open', 'iconv_close', 'setlocale', 'uselocale'}
    assert not imports & locale_imports, f'Review charset/locale modules: {imports & locale_imports}'

    retired = {'libssl3t64': '3.5.7-1~deb13u3', 'zlib1g': '1:1.3.dfsg+really1.3.1-1+b1',
               'libstdc++6': '14.2.0-19', 'libgomp1': '14.2.0-19', 'libzstd1': '1.5.7+dfsg-1'}
    for package, version in retired.items():
        metadata = status / package
        assert f'Version: {version}\n' in metadata.read_text(), f'Review changed {package}'
        manifest = status / (package + '.md5sums')
        for _, name in manifests[manifest]:
            remove(root / name)
        documentation = root / 'usr/share/doc' / package
        if documentation.is_symlink():
            remove(documentation)
        remove(metadata)
        remove(manifest)

    libraries = {}
    for path in root.rglob('*'):
        if path.is_file() and not path.is_symlink():
            with path.open('rb') as file:
                if file.read(4) == b'\x7fELF':
                    libraries[path.name] = path
    interpreter = re.search(r'Requesting program interpreter: (.*?)\]', readelf(executable, '--program-headers'))[1]
    retained = set()
    pending = needed(executable) | {Path(interpreter).name}
    reviewed = {'libc.so.6', 'libm.so.6', 'libgcc_s.so.1', Path(interpreter).name}
    while pending:
        name = pending.pop()
        assert name in reviewed, f'Review new native dependency: {name}'
        assert name in libraries, f'Missing required library: {name}'
        path = libraries[name]
        if path not in retained:
            retained.add(path)
            pending |= needed(path)
    assert (root / interpreter.lstrip('/')).resolve() in retained, 'Missing ELF interpreter'
    for path in libraries.values():
        if path not in retained:
            remove(path)
    # These tables only serve iconv/legacy encodings, which the release does not use.
    for path in list(root.rglob('*')):
        if 'gconv' in path.relative_to(root).parts and (path.is_file() or path.is_symlink()):
            remove(path)
    for path in list((root / 'usr/lib').rglob('*')):
        if path.is_symlink() and '.so' in path.name and not path.exists():
            remove(path)

    nsswitch = root / 'etc/nsswitch.conf'
    before_nsswitch = nsswitch.read_text()
    nsswitch.write_text('# Only libc built-in NSS backends are supported in this image.\n' +
                        ''.join(f'{name}: {backends}\n' for name, backends in (
                            ('passwd', 'files'), ('group', 'files'), ('shadow', 'files'),
                            ('gshadow', 'files'), ('hosts', 'files dns'), ('networks', 'files'),
                            ('protocols', 'files'), ('services', 'files'), ('ethers', 'files'),
                            ('rpc', 'files'), ('netgroup', 'files'))))
    cache = root / 'etc/ld.so.cache'
    if cache.exists():
        remove(cache)
    # Keep retained package identity and describe only the files actually shipped.
    for manifest, rows in manifests.items():
        if manifest.exists():
            manifest.write_text(''.join(
                f'{hashlib.md5(nsswitch.read_bytes(), usedforsecurity=False).hexdigest() if name == "etc/nsswitch.conf" else digest}  {name}\n'
                for digest, name in rows if name not in removed))
    for path in sorted(root.rglob('*'), key=lambda path: len(path.parts), reverse=True):
        if path.is_dir() and not path.is_symlink() and any(name.startswith(str(path.relative_to(root)) + '/') for name in removed):
            try:
                path.rmdir()
            except OSError:
                pass
    result = {'retired_packages': retired, 'removed': dict(sorted(removed.items())),
              'retained_libraries': {str(path.relative_to(root)): sorted(needed(path)) for path in sorted(retained)},
              'interpreter': interpreter, 'nsswitch_before': before_nsswitch,
              'nsswitch_after': nsswitch.read_text(), 'release_imports': sorted(imports)}
    report.write_text(json.dumps(result, indent=2) + '\n')
    print('Retained native libraries:', ', '.join(sorted(path.name for path in retained)))
    print(f'Removed {len(removed)} unused package/module files; audit: {report}')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', required=True, type=Path)
    parser.add_argument('--executable', required=True, type=Path)
    parser.add_argument('--report', required=True, type=Path)
    args = parser.parse_args()
    prune(args.root, args.executable, args.report)
