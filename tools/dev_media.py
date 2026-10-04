import base64
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shlex
import shutil
import subprocess
import tempfile


LIMIT = 5 * 1024 ** 3


def relative_path(value):
    path = PurePosixPath(value)
    if not value or path.is_absolute() or '..' in path.parts or '\0' in value:
        raise ValueError('media paths must be nonescaping relative paths')
    return path


def digest(path):
    result = hashlib.sha256()
    with path.open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            result.update(chunk)
    return result.hexdigest()


def mounted(root):
    point = root / 'music'
    if not point.is_mount():
        return False
    result = subprocess.run(['diskutil', 'info', '-plist', str(point)], capture_output=True, check=True)
    import plistlib
    info = plistlib.loads(result.stdout)
    if info.get('WritableVolume') is not False or info.get('WritableMedia') is not False:
        raise RuntimeError('music is mounted writable; stop users and remount it read-only')
    image = str((root / 'music.sparseimage').resolve())
    attached = subprocess.run(['hdiutil', 'info', '-plist'], capture_output=True, check=True)
    images = plistlib.loads(attached.stdout).get('images', [])
    if not any(item.get('image-path') == image and any(
        entity.get('mount-point') == str(point) for entity in item.get('system-entities', [])
    ) for item in images):
        raise RuntimeError('music mount does not belong to this setup')
    return True


def mount(root):
    if mounted(root):
        return root / 'music'
    if not (root / 'media-manifest.json').is_file():
        raise RuntimeError('music has not been imported; run just media import first')
    point = root / 'music'
    point.mkdir(exist_ok=True)
    if any(point.iterdir()):
        raise RuntimeError('refusing to mount over files in the music mountpoint')
    subprocess.run(['hdiutil', 'attach', str(root / 'music.sparseimage'), '-readonly',
                    '-nobrowse', '-mountpoint', str(point)], check=True, timeout=30)
    if not mounted(root):
        raise RuntimeError('music did not mount read-only')
    return point


def verify(root):
    point = mount(root)
    manifest = json.loads((root / 'media-manifest.json').read_text())
    total = 0
    for item in manifest['files']:
        path = point / str(relative_path(item['path']))
        if path.is_symlink() or not path.is_file() or path.stat().st_size != item['bytes']:
            raise RuntimeError(f'music file changed: {item["path"]}')
        if digest(path) != item['sha256']:
            raise RuntimeError(f'music hash differs: {item["path"]}')
        total += item['bytes']
    if total != manifest['bytes'] or total > LIMIT:
        raise RuntimeError('music manifest exceeds its size limit or total differs')
    print(f'verified {len(manifest["files"])} files, {total / 1024 ** 3:.3f} GiB; read-only mount')


REMOTE_MANIFEST = '''import hashlib,json,os,pathlib,sys
base=pathlib.Path(sys.argv[1]).resolve()
albums=json.loads(__import__('base64').b64decode(sys.argv[2]))
selected=[]
total=0
for album in albums:
    directory=base/album
    if directory.is_symlink() or not directory.resolve().is_relative_to(base):
        raise RuntimeError('unsafe album path')
    files=[]
    for current,dirs,names in os.walk(directory,followlinks=False):
        dirs.sort()
        if any((pathlib.Path(current)/name).is_symlink() for name in dirs):
            raise RuntimeError('symlink in album')
        for name in sorted(names):
            path=pathlib.Path(current)/name
            if path.is_symlink() or not path.is_file():
                raise RuntimeError('nonregular media file')
            size=path.stat().st_size
            sha=hashlib.sha256()
            with path.open('rb') as stream:
                for chunk in iter(lambda:stream.read(1048576),b''):
                    sha.update(chunk)
            files.append(dict(path=str(path.relative_to(base)),bytes=size,sha256=sha.hexdigest()))
    size=sum(item['bytes'] for item in files)
    if total+size<=int(sys.argv[3]):
        selected.append(dict(album=album,files=files))
        total+=size
print(json.dumps(dict(albums=[item['album'] for item in selected],files=[file for item in selected for file in item['files']],bytes=total)))
'''


def import_media(root, remote, source, plan):
    if (root / 'media-manifest.json').exists():
        existing = json.loads((root / 'media-manifest.json').read_text())
        if (remote and remote != existing['remote']) or (source and source != existing['source']):
            raise RuntimeError('music is already imported from another source; existing copy retained')
        if plan.is_file():
            requested = json.loads(plan.read_text())
            requested = requested.get('manifest', requested)
            albums = [item['album'] for item in requested['selected_complete_album_directories']]
            if albums != existing.get('requested_albums', existing['albums']):
                raise RuntimeError('music selection differs; replacing a personal dataset requires an explicit retention decision')
        verify(root)
        return
    if not remote or not re.fullmatch(r'[a-zA-Z0-9_.-]+@[a-zA-Z0-9_.-]+', remote):
        raise ValueError('remote must be user@host')
    if not source or not PurePosixPath(source).is_absolute() or '..' in PurePosixPath(source).parts:
        raise ValueError('source must be an absolute path without parent traversal')
    selection = json.loads(plan.read_text())
    selection = selection.get('manifest', selection)
    albums = [item['album'] for item in selection['selected_complete_album_directories']]
    for album in albums:
        relative_path(album)
    encoded = base64.b64encode(json.dumps(albums).encode()).decode()
    command = shlex.join(['python3', '-', source, encoded, str(LIMIT)])
    print('reading source manifest and hashes; no source writes', flush=True)
    response = subprocess.run(['ssh', '-oBatchMode=yes', '-oConnectTimeout=10', remote, command],
                              input=REMOTE_MANIFEST, capture_output=True, text=True, timeout=300, check=True)
    manifest = json.loads(response.stdout)
    if not manifest['files'] or not 0 < manifest['bytes'] <= LIMIT:
        raise RuntimeError('source selection is empty or exceeds 5 GiB')
    for item in manifest['files']:
        relative_path(item['path'])
    if shutil.disk_usage(root).free < manifest['bytes'] + 10 * 1024 ** 3:
        raise RuntimeError('music import would cross the 10 GiB free-space reserve')
    manifest.update({'remote': remote, 'source': source, 'limit_bytes': LIMIT, 'requested_albums': albums})
    image = root / 'music.sparseimage'
    point = root / 'music'
    point.mkdir(exist_ok=True)
    if point.is_mount() or any(point.iterdir()):
        raise RuntimeError('music mountpoint is occupied')
    if not image.exists():
        subprocess.run(['hdiutil', 'create', '-size', '6g', '-type', 'SPARSE', '-fs', 'APFS',
                        '-volname', 'LumenDevMusic', str(image)], check=True, timeout=60)
    image.chmod(0o600)
    subprocess.run(['hdiutil', 'attach', str(image), '-nobrowse', '-mountpoint', str(point)],
                   check=True, timeout=30)
    try:
        with tempfile.NamedTemporaryFile(dir=root, prefix='media-files-', delete=True) as listing:
            listing.write(b'\0'.join(item['path'].encode() for item in manifest['files']) + b'\0')
            listing.flush()
            print(f'copying {len(manifest["albums"])} album directories, {manifest["bytes"] / 1024 ** 3:.3f} GiB', flush=True)
            subprocess.run(['rsync', '-rt', '--from0', f'--files-from={listing.name}', '--stats',
                            '-e', 'ssh -oBatchMode=yes -oConnectTimeout=10',
                            f'{remote}:{shlex.quote(source.rstrip("/") + "/")}', str(point) + '/'],
                           check=True, timeout=900)
        for item in manifest['files']:
            path = point / item['path']
            if path.stat().st_size != item['bytes'] or digest(path) != item['sha256']:
                raise RuntimeError(f'copy differs from source manifest: {item["path"]}')
        destination = root / 'media-manifest.json'
        destination.write_text(json.dumps(manifest, indent=2, ensure_ascii=False) + '\n')
        destination.chmod(0o600)
    finally:
        subprocess.run(['hdiutil', 'detach', str(point)], check=True, timeout=30)
    mount(root)
    print('music copy verified and mounted read-only', flush=True)
