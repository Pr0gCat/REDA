import json,subprocess
from pathlib import Path
r=Path(__file__).parent
for name in ['default_probe','family_probe','wave_default']:
 with (r/(name+'-build.log')).open('w') as f:
  p=subprocess.run(json.loads((r/(name+'-build-arguments.json')).read_text()),stdout=f,stderr=subprocess.STDOUT)
 if p.returncode:raise SystemExit(p.returncode)
