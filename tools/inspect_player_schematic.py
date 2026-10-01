"""Inspect the downloaded Sponge v3 player fixtures without changing block states."""
import gzip,struct,json,hashlib,sys

def parse(path):
 b=gzip.open(path,'rb').read(); pos=0
 def read(n):
  nonlocal pos
  v=b[pos:pos+n];pos+=n;assert len(v)==n;return v
 def num(fmt):return struct.unpack('>'+fmt,read(struct.calcsize(fmt)))[0]
 def string():return read(num('H')).decode()
 def tag(t):
  if t in [1,2,3,4,5,6]:return num({1:'b',2:'h',3:'i',4:'q',5:'f',6:'d'}[t])
  if t==7:return list(read(num('i')))
  if t==8:return string()
  if t==9:
   kind=num('B'); n=num('i');return [tag(kind) for _ in range(n)]
  if t==10:
   d={}
   while True:
    kind=num('B')
    if kind==0:return d
    name=string();d[name]=tag(kind)
  if t in [11,12]:return [num('i' if t==11 else 'q') for _ in range(num('i'))]
  raise ValueError(t)
 assert num('B')==10
 name=string();root=tag(10);assert pos==len(b)
 return root

for p in sys.argv[1:]:
 r=parse(p); s=r.get('Schematic',r); assert s['Version']==3; pal=s['Blocks']['Palette']; data=s['Blocks']['Data']; inv={v:k for k,v in pal.items()}; vals=[];value=shift=0
 for byte in data:
  value |= (byte&127)<<shift
  if byte&128:shift+=7
  else: vals.append(value);value=shift=0
 assert shift==0
 width,height,length=[s[k] for k in ['Width','Height','Length']]
 assert len(vals)==width*height*length
 blocks=[{'at':[i%width,i//(width*length),(i//width)%length],'state':inv[v]} for i,v in enumerate(vals) if inv[v]!='minecraft:air']
 out={'source_file':p,'sha256':hashlib.sha256(open(p,'rb').read()).hexdigest(),'size':[width,height,length],'metadata':s.get('Metadata',{}),'palette':pal,'blocks':blocks}
 json.dump(out,open(p+'.json','w'),indent=2)
 print(p,'size',out['size'],'sha256',out['sha256'])
 print(json.dumps(pal,indent=2))
