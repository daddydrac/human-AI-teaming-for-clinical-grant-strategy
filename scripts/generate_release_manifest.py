#!/usr/bin/env python3
import hashlib,json,pathlib,platform,re,sys,tomllib,datetime
root=pathlib.Path(__file__).resolve().parents[1]
out=pathlib.Path(sys.argv[1]) if len(sys.argv)>1 else root/'release-manifest.json'
exclude_parts={'.git','releases','backups','benchmarks','exports','__pycache__','target','.venv','.pytest_cache'}
exclude_names={'.env','.runtime.env'}
files=[]
for p in sorted(root.rglob('*')):
    if not p.is_file():continue
    rel=p.relative_to(root)
    if any(part in exclude_parts for part in rel.parts) or p.name in exclude_names:continue
    if p==out:continue
    files.append({'path':str(rel),'bytes':p.stat().st_size,'sha256':hashlib.sha256(p.read_bytes()).hexdigest()})
with open(root/'core'/'Cargo.toml','rb') as f: version=tomllib.load(f)['package']['version']
def sha(path):
    p=root/path; return hashlib.sha256(p.read_bytes()).hexdigest() if p.exists() else None
base_images={}
for df in ('Dockerfile.core','Dockerfile.ui','Dockerfile.renderer','Dockerfile.embedding-cpu'):
    refs=[]
    for line in (root/df).read_text().splitlines():
        m=re.match(r'FROM\s+([^\s]+)',line)
        if m: refs.append(m.group(1))
    base_images[df]=refs
manifest={
 'schema_version':2,'application':'mdanderson-grant-agent','version':version,
 'generated_at_utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),
 'source_files':files,
 'locks':{'cargo_lock_present':(root/'core/Cargo.lock').exists(),'cargo_lock_sha256':sha(pathlib.Path('core/Cargo.lock')),'competitive_config_sha256':sha(pathlib.Path('config/competitive_intelligence.json')),'sponsor_formats_sha256':sha(pathlib.Path('config/sponsor_formats.json'))},
 'runtime_defaults':{'competitive_refresh_seconds':14400,'ports':{'ui':7860,'core':8080,'renderer':8090,'embedding_cpu':8010},'weak_mac_profile':'docker_cpu','apple_silicon_profile':'apple_mlx'},
 'container_base_references':base_images,
 'build_environment':{'platform':platform.platform(),'machine':platform.machine()},
 'model_provenance':'Apple MLX startup resolves model repositories to immutable revisions and records them at runtime; release operators should preserve the generated runtime manifest with regulated runs.',
 'release_controls':{'loopback_only_ports':True,'read_only_root_filesystems':True,'capabilities_dropped':True,'no_new_privileges':True,'signed_release_supported':True,'sbom_format':'CycloneDX 1.5'},
 'contains_secrets':False
}
out.write_text(json.dumps(manifest,indent=2,sort_keys=True)+'\n')
print(out)
