#!/usr/bin/env python3
import json, pathlib, re, sys, tomllib, hashlib
root=pathlib.Path(__file__).resolve().parents[1]
components=[]

def add(name,version,kind,scope='required',purl=None,properties=None):
    c={'type':kind,'name':name,'version':str(version),'scope':scope}
    if purl:c['purl']=purl
    if properties:c['properties']=[{'name':k,'value':str(v)} for k,v in properties.items()]
    key=(c['type'],c['name'],c['version'],c.get('purl'))
    if not any((x['type'],x['name'],x['version'],x.get('purl'))==key for x in components): components.append(c)

def env_example():
    out={}
    for raw in (root/'.env.example').read_text().splitlines():
        if not raw or raw.lstrip().startswith('#') or '=' not in raw: continue
        k,v=raw.split('=',1); out[k.strip()]=v.strip().strip('"')
    return out

cargo=tomllib.loads((root/'core'/'Cargo.toml').read_text())
add(cargo['package']['name'],cargo['package']['version'],'application')
for name,spec in cargo.get('dependencies',{}).items():
    ver=spec if isinstance(spec,str) else spec.get('version','unknown')
    ver=str(ver).lstrip('=')
    add(name,ver,'library',purl=f'pkg:cargo/{name}@{ver}')
for reqfile in ('ui/requirements.txt','renderer/requirements.txt','embedding_cpu/requirements.txt'):
    for raw in (root/reqfile).read_text().splitlines():
        raw=raw.strip()
        if not raw or raw.startswith('#'):continue
        m=re.match(r'([A-Za-z0-9_.-]+)==([^\s;]+)',raw)
        if m:add(m.group(1),m.group(2),'library',purl=f'pkg:pypi/{m.group(1).lower()}@{m.group(2)}',properties={'requirements_file':reqfile})
lock=root/'core'/'Cargo.lock'
if lock.exists():
    data=tomllib.loads(lock.read_text())
    for pkg in data.get('package',[]):
        name,ver=pkg.get('name'),pkg.get('version')
        if name and ver:add(name,ver,'library',purl=f'pkg:cargo/{name}@{ver}',properties={'source':'Cargo.lock'})
# Container base references are part of reproducibility provenance even when a registry digest
# is not available on the source-only build machine.
for df in ('Dockerfile.core','Dockerfile.ui','Dockerfile.renderer','Dockerfile.embedding-cpu'):
    for line in (root/df).read_text().splitlines():
        m=re.match(r'FROM\s+([^\s]+)',line)
        if m:
            ref=m.group(1); name=ref.split('@')[0]; ver=(name.split(':',1)[1] if ':' in name else 'latest')
            add(name,ver,'container',purl=f'pkg:docker/{name.replace(":","@")}',properties={'dockerfile':df,'reference':ref})
env=env_example()
add('vllm-mlx',env.get('VLLM_MLX_VERSION','unknown'),'library',properties={'runtime':'native Apple Silicon'})
for key,label in (('OLMO_MODEL_REPO','OLMo local generation model'),('EMBEDDING_MODEL_REPO','MLX embedding model'),('CPU_EMBEDDING_MODEL','CPU embedding model')):
    if env.get(key): add(env[key],env.get(key+'_REVISION','unresolved') or 'resolved-at-runtime','machine-learning-model',properties={'role':label})
serial='urn:uuid:'+hashlib.sha256(('grant-writer-'+cargo['package']['version']).encode()).hexdigest()[:32]
out={'bomFormat':'CycloneDX','specVersion':'1.5','serialNumber':serial,'version':1,'metadata':{'component':{'type':'application','name':'mdanderson-grant-agent','version':cargo['package']['version']}},'components':components}
path=pathlib.Path(sys.argv[1]) if len(sys.argv)>1 else root/'release-sbom.cdx.json'
path.write_text(json.dumps(out,indent=2,sort_keys=True)+'\n')
print(path)
