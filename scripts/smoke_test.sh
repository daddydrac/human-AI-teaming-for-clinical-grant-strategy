#!/usr/bin/env bash
set -euo pipefail
CORE_URL="${CORE_URL:-http://localhost:8080}" python3 - <<'PY'
import json, os, sys, uuid
from urllib.error import HTTPError
from urllib.request import Request, urlopen

core=os.environ['CORE_URL'].rstrip('/')
access_token=os.environ.get('SMOKE_ACCESS_TOKEN','').strip()

def call(method,path,payload=None,key=None,expected=(200,)):
    raw=None if payload is None else json.dumps(payload,separators=(',',':'),sort_keys=True).encode()
    headers={'accept':'application/json'}
    if access_token: headers['authorization']=f'Bearer {access_token}'
    if raw is not None: headers['content-type']='application/json'
    if method not in {'GET','HEAD','OPTIONS'}: headers['idempotency-key']=key or str(uuid.uuid4())
    request=Request(core+path,data=raw,headers=headers,method=method)
    try:
        response=urlopen(request,timeout=120);status=response.status;body=response.read();response_headers=dict(response.headers.items())
    except HTTPError as error:
        status=error.code;body=error.read();response_headers=dict(error.headers.items())
    if status not in expected:
        raise AssertionError(f'{method} {path}: expected {expected}, got {status}: {body[:1000]!r}')
    parsed=json.loads(body) if body else None
    return status,parsed,{k.lower():v for k,v in response_headers.items()}

print('[1/10] service health and workflow registry')
_,health,_=call('GET','/health')
assert health['status']=='ok'
_,ready,_=call('GET','/health/ready')
assert ready['status']=='ready' and ready['model']['ok'] and ready['embedding']['ok']
registry_status,registry,_=call('GET','/api/workflow-definitions',expected=(200,401))
if registry_status==401:
    _,bootstrap,_=call('GET','/api/auth/bootstrap/status')
    assert isinstance(bootstrap.get('bootstrap_required'),bool)
    print('Public pre-login smoke test passed; set SMOKE_ACCESS_TOKEN to run the authenticated project mutation suite.')
    sys.exit(0)
assert len(registry['core_steps'])==5 and registry['optional_modules']

print('[2/10] create a lean composable project with an idempotent request')
workflow={'schema_version':registry['schema_version'],'definition_version':registry['definition_version'],
    'template':registry['default_preset_key'],'enabled_modules':[],'required_modules':[],
    'review_mode':None,'review_required':False,'grant_type':'research','target_deadline':None,
    'model_routing_mode':'local_only','local_model_provider':None,'local_model':None,'cloud_model':None,'cloud_task_kinds':[]}
payload={'title':f'Composable acceptance {uuid.uuid4()}','sponsor':'Acceptance sponsor','mechanism':'TEST','sections':['Specific Aims'],'workflow':workflow}
request_key=str(uuid.uuid4())
_,created,_=call('POST','/api/projects',payload,request_key)
project=created['id']
_,replayed,replay_headers=call('POST','/api/projects',payload,request_key)
assert replayed==created and replay_headers.get('idempotency-replayed')=='true'

print('[3/10] reject same idempotency key with a different payload')
call('POST','/api/projects',{**payload,'title':'Different request content'},request_key,expected=(409,))

print('[4/10] disabled modules are absent from navigation and blockers')
_,status,_=call('GET',f'/api/projects/{project}/workflow/status')
optional={m['key'] for m in registry['optional_modules']}
assert len(status['steps'])==5,status
assert not any(step.get('key') in optional for step in status['steps'])
assert not any(blocker.get('step') in optional for blocker in status['blockers'])

print('[5/10] ingest exact source content without model execution')
source='Applicants must include a Specific Aims section and a human-approved research plan.'
_,document,_=call('POST',f'/api/projects/{project}/documents',{'name':'acceptance-nofo.txt','kind':'funding_opportunity','text':source})
assert document['added'] and document['document_id']>0

print('[6/10] produce and validate a content-addressed portable project package')
_,package,_=call('GET',f'/api/projects/{project}/portable-export')
assert package['format']=='grantspace-portable-project' and len(package['payload_sha256'])==64
_,validation,_=call('POST','/api/project-imports/validate',{'package':package})
assert validation['valid'] and validation['counts']['documents']==1

print('[7/10] transactionally import under a new project identity')
_,imported,_=call('POST','/api/project-imports',{'package':package})
assert imported['id']!=project
_,imported_project,_=call('GET',f"/api/projects/{imported['id']}")
assert imported_project['title']==payload['title']
_,imported_workflow,_=call('GET',f"/api/projects/{imported['id']}/workflow")
assert imported_workflow['config']['enabled_modules']==[]

print('[8/10] collaboration workspace persists scoped messages and assigned tasks')
_,identity,_=call('GET','/api/me')
team_workflow={**workflow,'enabled_modules':['team_collaboration']}
_,team_created,_=call('POST','/api/projects',{**payload,'title':f'Team acceptance {uuid.uuid4()}','workflow':team_workflow})
team_project=team_created['id']
routing={'schema_version':1,'project_owner_user_id':identity['id'],'routes':[
    {'artifact_type':'proposal_section','owner_user_id':identity['id'],'approver_user_ids':[identity['id']],'minimum_approvals':1}
]}
_,routing_record,_=call('POST',f'/api/projects/{team_project}/workflow/artifacts/collaboration_record',{'body':routing,'source':'acceptance','expected_version':0})
call('POST',f'/api/projects/{team_project}/workflow/artifacts/collaboration_record/approve',{'version':routing_record['version'],'approver':identity['id']})
_,message,_=call('POST',f'/api/projects/{team_project}/channels/general',{'body':'Authenticated acceptance message','mentioned_user_ids':[]})
assert message['id']>0
_,task,_=call('POST',f'/api/projects/{team_project}/tasks',{'title':'Acceptance task','description':'Exercise the shared task contract','owner_user_id':identity['id'],'source':'human','priority':'high','due_at':None,'dependencies':[]})
_,workspace,_=call('GET',f'/api/projects/{team_project}/collaboration/workspace')
assert workspace['permissions']['role']=='owner'
assert any(item['id']==task['id'] for item in workspace['tasks'])
assert workspace['approval_routing']['configured'] is True

print('[9/10] task owner can update status and receives an auditable notification')
call('POST',f"/api/projects/{team_project}/tasks/{task['id']}/status",{'status':'complete'})
_,workspace,_=call('GET',f'/api/projects/{team_project}/collaboration/workspace')
assert next(item for item in workspace['tasks'] if item['id']==task['id'])['status']=='complete'
assert any(item['kind']=='task_assigned' for item in workspace['notifications'])

print('[10/10] tampered portable payload fails before writes')
tampered=json.loads(json.dumps(package));tampered['payload']['project']['title']='Tampered title'
call('POST','/api/project-imports/validate',{'package':tampered},expected=(400,))
print('Composable authenticated API smoke test passed.')
PY
