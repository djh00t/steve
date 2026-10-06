#!/usr/bin/env python3
"""Throwaway fixture-only gateway spike. Never supplies real credentials."""
import argparse, hashlib, http.client, json, os, select, socket, sqlite3
import statistics, subprocess, tempfile, threading, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import sys
sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[2] / 'scripts'))
from sdk_smoke import ready_address, stop

KEY = 'fake-fixture-provider-key'
MARKER = 'SYNTHETIC_CONTENT_CANARY'

def require(value, message):
    if not value: raise RuntimeError(message)

def port():
    with socket.socket() as s:
        s.bind(('127.0.0.1', 0)); return s.getsockname()[1]

def request(address, path, data, close_early=False):
    c = http.client.HTTPConnection(address, timeout=35)
    begin = time.perf_counter()
    c.request('POST', path, json.dumps(data), {'Content-Type':'application/json',
        'Authorization':'Bearer fake-client-key', 'x-api-key':'fake-client-key',
        'anthropic-version':'2023-06-01'})
    r = c.getresponse(); first = r.read1(1); first_time = time.perf_counter()-begin
    body = first if close_early else first+r.read()
    c.close()
    return {'request_id':r.getheader('x-request-id'),'status':r.status,'first_byte_ms':first_time*1000,
        'total_ms':(time.perf_counter()-begin)*1000,'body':body}

def rss(child):
    return int(subprocess.check_output(['ps','-p',str(child.pid),'-o','rss=']).strip())

def payload(protocol, stream=False, scenario='normal'):
    d={'model':'up-anth' if protocol=='messages' else 'up-chat','stream':stream,
       'max_tokens':16,'metadata':{'scenario':scenario},'spike_unknown':{'preserve':True}}
    if protocol=='responses': d['input']=MARKER+' '+scenario
    else: d['messages']=[{'role':'user','content':MARKER+' '+scenario}]
    return d

class Fixture(BaseHTTPRequestHandler):
    protocol_version='HTTP/1.1'
    records=[]; cancelled=[]; templates={}
    def log_message(self,*args): pass
    def do_POST(self):
        data=json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        text=json.dumps(data); scenario=next((s for s in ['cancel','missing','error','malformed','timeout'] if s in text),'normal')
        self.records.append({'path':self.path,'scenario':scenario,'auth_ok':self.headers.get('Authorization')=='Bearer '+KEY or self.headers.get('x-api-key')==KEY,'unknown_preserved':'spike_unknown' in data})
        if scenario=='timeout': time.sleep(31)
        code=503 if scenario=='error' else 200
        stream=data.get('stream',False)
        body=self.templates[(self.path,stream)]
        if self.path=='/v1/responses' and not stream:
            response=json.loads(body); response['usage']['input_tokens_details']={'cached_tokens':0}; response['usage']['output_tokens_details']={'reasoning_tokens':0}; body=json.dumps(response).encode()
        if scenario=='malformed': body=b'{"raw_canary":"'+MARKER.encode()+b'" INVALID}'
        if not stream and scenario=='missing':
            d=json.loads(body); d.pop('usage',None); body=json.dumps(d).encode()
        self.send_response(code)
        self.send_header('Content-Type','text/event-stream' if stream else 'application/json')
        self.send_header('Connection','close'); self.end_headers()
        try:
            if stream:
                chunks=body.split(b'\n\n'); self.wfile.write(chunks[0]+b'\n\n'); self.wfile.flush()
                if scenario=='cancel':
                    deadline=time.monotonic()+2
                    while time.monotonic()<deadline:
                        if select.select([self.connection],[],[],.05)[0] and not self.connection.recv(1,socket.MSG_PEEK):
                            self.cancelled.append(self.path); return
                    return
                time.sleep(.3)
                self.wfile.write(b'\n\n'.join(chunks[1:])); self.wfile.flush()
            else: self.wfile.write(body); self.wfile.flush()
        except (BrokenPipeError,ConnectionResetError):
            self.cancelled.append(self.path)
        finally: self.close_connection=True


def main():
    a=argparse.ArgumentParser(); a.add_argument('--steve',type=Path,required=True); a.add_argument('--gateway',type=Path,required=True); a.add_argument('--output',type=Path,required=True); args=a.parse_args()
    env={'PATH':os.environ['PATH'],'TMPDIR':tempfile.gettempdir(),'STEVE_SPIKE_KEY':KEY,'STEVE_OPERATOR':'fixture-operator'}
    children=[]; handles=[]; results={'fixture_only':True,'source_base':'1f3e37bb59c76e6bf56670b87460d4e2da547e5d','probe_sha256':hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),'platform':subprocess.check_output(['uname','-sm']).decode().strip(),'steve_sha256':hashlib.sha256(args.steve.read_bytes()).hexdigest(),'gateway_sha256':hashlib.sha256(args.gateway.read_bytes()).hexdigest(),'paths':{}}
    with tempfile.TemporaryDirectory(prefix='steve-gateway-spike-') as tmp:
        root=Path(tmp)
        def launch(command,name):
            log=root/(name+'.log'); h=log.open('wb'); handles.append(h)
            child=subprocess.Popen(command,env=env,stdout=h,stderr=subprocess.STDOUT); children.append(child)
            return child,log
        try:
            fc=root/'fixture.toml'; fc.write_text('[logging]\njson=true\n')
            fixture,flog=launch([str(args.steve),'--config',str(fc),'test-upstream','--listen','127.0.0.1:0'],'template')
            addr=ready_address(fixture,flog,'test_upstream_ready','local_addr')
            for proto,path in [('chat','/v1/chat/completions'),('responses','/v1/responses'),('messages','/v1/messages')]:
                for stream in [False,True]: Fixture.templates[(path,stream)]=request(addr,path,payload(proto,stream))['body']
            stop(fixture)
            server=ThreadingHTTPServer(('127.0.0.1',0),Fixture); server.daemon_threads=True
            thread=threading.Thread(target=server.serve_forever,daemon=True); thread.start()
            upstream=f'127.0.0.1:{server.server_port}'
            gp,gr,gs,ga=port(),port(),port(),port()
            gateway_config={'config':{'readinessAddr':f'127.0.0.1:{gr}','statsAddr':f'127.0.0.1:{gs}','adminAddr':f'127.0.0.1:{ga}','logging':{'level':'error','filter':'false'}},'gateways':{'default':{'port':gp,'bindAddress':'127.0.0.1'}},'llm':{'gateways':['default'],'models':[{'name':'up-chat','provider':'openAI','params':{'model':'up-chat','apiKey':KEY,'baseUrl':f'http://{upstream}/v1'}},{'name':'up-anth','provider':'anthropic','params':{'model':'up-anth','apiKey':KEY,'baseUrl':f'http://{upstream}/v1'}}]}}
            gateway_config['llm']['models'].append({'name':'bridge','provider':'openAI','params':{'model':'up-chat','apiKey':KEY,'baseUrl':f'http://{upstream}/v1'}})
            gconfig=root/'gateway.json'; gconfig.write_text(json.dumps(gateway_config))
            validated=subprocess.run([str(args.gateway),'-f',str(gconfig),'--validate-only'],env=env,capture_output=True,timeout=15)
            require(validated.returncode==0, 'gateway validation: '+validated.stderr.decode()[-2000:]+validated.stdout.decode()[-2000:])
            before=time.perf_counter(); gateway,glog=launch([str(args.gateway),'-f',str(gconfig)],'gateway')
            deadline=time.monotonic()+15
            while time.monotonic()<deadline:
                try:
                    with socket.create_connection(('127.0.0.1',gp),timeout=.1):break
                except OSError: time.sleep(.02)
            else: raise RuntimeError('gateway startup failed: '+glog.read_text()[-1000:])
            results['gateway_startup_ms']=(time.perf_counter()-before)*1000
            results['gateway_rss_kib']=rss(gateway)
            daemons=[]
            for label,base in [('direct',upstream),('gateway',f'127.0.0.1:{gp}')]:
                d=root/label; d.mkdir(); journal=d/'accounting'
                subprocess.run([str(args.steve),'accounting','provision','--root',str(journal)],env=env,check=True,capture_output=True,timeout=15)
                config=f'[server]\ninference_bind="127.0.0.1:0"\nmanagement_bind="127.0.0.1:0"\ndrain_timeout_seconds=5\n[database]\nurl={json.dumps("sqlite://"+str(d/"steve.db")+"?mode=rwc")}\n[object_storage]\nroot={json.dumps(str(d/"objects"))}\n[queues]\naccounting_journal={json.dumps(str(journal))}\n[logging]\njson=true\nlevel="info"\n[native]\n'
                for provider,protocol in [('open','openai'),('anth','anthropic')]:
                    config+=f'[[native.providers]]\nid="{provider}"\nprotocol="{protocol}"\nbase_url="http://{base}"\ncredential_env="STEVE_SPIKE_KEY"\n'
                for model,provider,protocols in [('up-chat','open',['chat','responses']),('up-anth','anth',['messages']),('bridge','anth',['messages'])]:
                    config+=f'[[native.models]]\nid="{model}"\nprovider="{provider}"\nupstream_model="{model}"\nprotocols={json.dumps(protocols)}\ninput_micro_usd_per_million=1\noutput_micro_usd_per_million=1\n'
                cf=d/'config.toml';cf.write_text(config); before=time.perf_counter()
                child,log=launch([str(args.steve),'--config',str(cf),'serve'],label)
                inference=ready_address(child,log,'listeners_ready','inference')
                ready_address(child,log,'listeners_ready','management')
                measurements={'startup_ms':(time.perf_counter()-before)*1000,'rss_kib':rss(child),'cases':{}}
                for proto,path in [('chat','/v1/chat/completions'),('responses','/v1/responses'),('messages','/v1/messages')]:
                    for stream in [False,True]:
                        r=request(inference,path,payload(proto,stream)); key=proto+('_sse' if stream else '_json')
                        measurements['cases'][key]={k:v for k,v in r.items() if k!='body'}
                        measurements['cases'][key]['contains_expected_text']=b'steve-test' in r['body']
                        measurements['cases'][key]['usage_present']=b'usage' in r['body']
                    r=request(inference,path,payload(proto,False,'missing'))
                    measurements['cases'][proto+'_missing']={'status':r['status'],'wire_usage_present':b'usage' in r['body'],'wire_usage':json.loads(r['body']).get('usage') if r['status']==200 else None}
                    n=len(Fixture.records); cancelled=len(Fixture.cancelled)
                    r=request(inference,path,payload(proto,True,'cancel'),True); time.sleep(2.1)
                    measurements['cases'][proto+'_cancel']={'status':r['status'],'upstream_attempts':len(Fixture.records)-n,'upstream_cancelled':len(Fixture.cancelled)>cancelled}
                for stream in [False,True]:
                    data=payload('messages',stream); data['model']='bridge'; n=len(Fixture.records)
                    r=request(inference,'/v1/messages',data)
                    measurements['cases']['bridge_'+('sse' if stream else 'json')]={'status':r['status'],'actual_upstream_path':Fixture.records[-1]['path'],'contains_expected_text':b'steve-test' in r['body'],'first_byte_ms':r['first_byte_ms']}
                for scenario in ['error','malformed','timeout']:
                    n=len(Fixture.records); r=request(inference,'/v1/chat/completions',payload('chat',False,scenario))
                    measurements['cases'][scenario]={'status':r['status'],'total_ms':r['total_ms'],'upstream_attempts':len(Fixture.records)-n}
                held=[]
                for _ in range(32):
                    connection=http.client.HTTPConnection(inference,timeout=5); data=payload('chat',True,'cancel')
                    connection.request('POST','/v1/chat/completions',json.dumps(data),{'Content-Type':'application/json'})
                    response=connection.getresponse(); require(response.status==200,'hold setup'); response.read1(1); held.append((connection,response))
                overloaded=request(inference,'/v1/chat/completions',payload('chat'))
                management=ready_address(child,log,'listeners_ready','management')
                health=http.client.HTTPConnection(management,timeout=5);health.request('GET','/health/ready');hr=health.getresponse(); readiness=json.loads(hr.read());health.close()
                measurements['admission']={'overload_status':overloaded['status'],'health_status':hr.status,'inference_active':readiness['admission']['inference']['active']}
                for connection,response in held:response.close();connection.close()
                time.sleep(.2)
                times=[]; before=time.perf_counter()
                for _ in range(30):times.append(request(inference,'/v1/chat/completions',payload('chat'))['first_byte_ms'])
                measurements['sequential_json_rps']=30/(time.perf_counter()-before)
                measurements['json_first_byte_ms_median']=statistics.median(times)
                measurements['rss_after_kib']=rss(child)
                daemons.append((child,d,log,measurements)); results['paths'][label]=measurements
            for child,d,log,measurements in daemons:
                before=time.perf_counter();stop(child);measurements['drain_ms']=(time.perf_counter()-before)*1000;measurements['drain_exit_code']=child.returncode
                c=sqlite3.connect(d/'steve.db'); measurements['terminal_kinds']=c.execute('select kind,count(*) from steve_background_events group by kind').fetchall(); measurements['terminal_id_count']=c.execute("select count(*),count(distinct id),count(distinct json_extract(payload,'$.attempt_id')) from steve_background_events where kind='chat.attempt.terminal.v1'").fetchone(); terminal_request_ids={row[0] for row in c.execute("select json_extract(payload,'$.request_id') from steve_background_events where kind='chat.attempt.terminal.v1'")};c.close()
                records=[json.loads(line) for line in log.read_text().splitlines() if line.startswith('{')]
                measurements['native_events']=[r.get('fields',{}) for r in records if r.get('fields',{}).get('message')=='native_response']
                measurements['durable_native_requests_by_protocol']={p:sum(1 for e in measurements['native_events'] if e.get('protocol')==p and e.get('request_id') in terminal_request_ids) for p in ['chat','responses','messages']}
            results['gateway_rss_after_kib']=rss(gateway)
            results['fixture_records']=Fixture.records
            results['all_fixture_credentials_correct']=all(r['auth_ok'] for r in Fixture.records)
            results['log_canary_leak']=any(MARKER in p.read_text(errors='replace') or KEY in p.read_text(errors='replace') or 'fake-client-key' in p.read_text(errors='replace') for p in root.rglob('*.log'))
            server.shutdown();server.server_close()
            args.output.write_text(json.dumps(results,indent=2))
            for path,measurement in results['paths'].items():
                require(measurement['admission']=={'overload_status':503,'health_status':200,'inference_active':32},path+' admission qualification')
                require(measurement['drain_exit_code']==0,path+' idle shutdown')
            require(not results['log_canary_leak'],'content or key canary leak')
            require(results['all_fixture_credentials_correct'],'fixture auth substitution')
            print(json.dumps({'output':str(args.output),'privacy_leak':results['log_canary_leak'],'credentials_ok':results['all_fixture_credentials_correct']}))
        finally:
            for c in reversed(children):stop(c)
            for h in handles:h.close()

if __name__=='__main__':main()
