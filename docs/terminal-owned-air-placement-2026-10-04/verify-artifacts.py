"""Verify original byte hashes, including losslessly archived logs/patches."""
from pathlib import Path
import gzip,hashlib,json
root=Path(__file__).resolve().parent
manifest=json.loads((root/'artifact-sha256.json').read_text())
for name,digest in manifest.items():
 path=root/name
 data=path.read_bytes() if path.exists() else gzip.decompress(path.with_name(path.name+'.gz').read_bytes())
 assert hashlib.sha256(data).hexdigest()==digest,name
print(f'{len(manifest)} original artifact hashes verified')
