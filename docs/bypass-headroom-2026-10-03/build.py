import json,subprocess
from pathlib import Path
r=Path(__file__).parent
with (r/"build.log").open("w") as f:
 p=subprocess.run(json.loads((r/"build-arguments.json").read_text()),stdout=f,stderr=subprocess.STDOUT)
raise SystemExit(p.returncode)
