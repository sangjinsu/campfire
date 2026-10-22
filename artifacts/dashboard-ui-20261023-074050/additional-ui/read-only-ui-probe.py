from pathlib import Path
import sys,os,json,time,tempfile,hashlib,math
ROOT=Path('/Users/sangjinsu/Documents/ChatGPT/campfire');sys.path.insert(0,str(ROOT/'tools'))
import validate_dashboard as d
import validate_godot as guest
out=ROOT/'artifacts/dashboard-ui-20261023-074050/additional-ui';out.mkdir(exist_ok=False)
r=d.Runner(out,'/opt/homebrew/bin/godot'); manifest=d.source_manifest(); binary=d.sha(d.BINARY)
result={'success':False,'timestamp_kst':time.strftime('%Y-%m-%d %H:%M:%S KST'),'source_sha256_before':d.digest(manifest),'source_manifest':manifest,'binary_sha256_before':binary,'checks':{},'failures':[],'scope':'Actual separate-loopback server and production Godot 104 rooms; layout is real Chromium. History edge cases below explicitly use controlled browser fetch responses. No physical OS input claimed.'}
def js(expression):return json.loads(r.js('JSON.stringify('+expression+')'))
def soft(label,condition,details):
 result['checks'][label]={'success':bool(condition),'details':details}
 if not condition:result['failures'].append(label)
def layout():
 return js("(() => {const table=document.getElementById('room-table-scroll'); const box=e=>{const r=e.getBoundingClientRect();return {x:r.x,y:r.y,width:r.width,height:r.height}};const charts=[...document.querySelectorAll('svg.chart')].map(svg=>{const labels=[...svg.querySelectorAll('text')].map(t=>{const m=t.getScreenCTM(); const font=parseFloat(getComputedStyle(t).fontSize);return {text:t.textContent,fontCss:font,scaleY:Math.hypot(m.c,m.d),effectiveFontPx:font*Math.hypot(m.c,m.d),bbox:box(t)}});const overlaps=[];for(let i=0;i<labels.length;i++)for(let j=i+1;j<labels.length;j++){const a=labels[i].bbox,b=labels[j].bbox;if(Math.min(a.x+a.width,b.x+b.width)-Math.max(a.x,b.x)>.5&&Math.min(a.y+a.height,b.y+b.height)-Math.max(a.y,b.y)>.5)overlaps.push([i,j]);}return {id:svg.id,labels,overlaps}});const targets=[...document.querySelectorAll('button,input,select')].filter(e=>!e.closest('[hidden]')&&e.getClientRects().length).map(e=>{const target=e.type==='checkbox'?e.closest('label'):e;return {id:e.id||e.className,tag:e.tagName,type:e.type,disabled:e.disabled,bbox:box(target)}});return {viewport:innerWidth,rootWidth:document.documentElement.scrollWidth,bodyWidth:document.body.scrollWidth,charts,targets,table:{rows:document.querySelectorAll('#room-rows tr').length,clientHeight:table.clientHeight,scrollHeight:table.scrollHeight,clientWidth:table.clientWidth,scrollWidth:table.scrollWidth,sticky:[...document.querySelectorAll('th')].map(e=>({position:getComputedStyle(e).position,top:getComputedStyle(e).top})),pagination:box(document.querySelector('.pagination'))}}})()")
server=None;fixture=None;temp=None
try:
 server,base,key=r.server('additional-ui-server');result['test_base_url']=base
 r.browse('goto',base+'/admin');r.login(key);r.until("!document.getElementById('dashboard').hidden && !document.getElementById('refresh').disabled")
 temp=tempfile.TemporaryDirectory(prefix='campfire-ui-extra-rooms-');work=Path(temp.name)
 fixture=r.launch('additional-room-fixture',guest.engine_command(r.godot,'res://tests/dashboard_rooms.gd',[base,work,104]))
 guest.wait_for(lambda:guest.read_json(work/'ready.json'),100,'104 additional actual rooms',[fixture])
 r.browse('click','#refresh');r.until("document.querySelectorAll('#room-rows tr').length===100 && !document.getElementById('refresh').disabled")
 for width,height in [(1440,1100),(390,844)]:
  r.browse('viewport',f'{width}x{height}');r.js('window.scrollTo(0,0); true');value=layout();d.dump(out/f'layout-{width}.json',value)
  fonts=[v['effectiveFontPx'] for c in value['charts'] for v in c['labels']]
  overlaps=[(c['id'],c['overlaps']) for c in value['charts'] if c['overlaps']]
  soft(f'font-at-least-12-{width}',min(fonts)>=12,{'minimum_effective_px':min(fonts),'formula':'parseFloat(computed font-size) * hypot(getScreenCTM().c, getScreenCTM().d)'})
  soft(f'no-svg-label-overlap-{width}',not overlaps,overlaps)
  small=[t for t in value['targets'] if t['bbox']['height']<43.99 or (t['tag']=='BUTTON' and t['bbox']['width']<43.99)]
  soft(f'touch-targets-44-{width}',not small,small)
  soft(f'bounded-sticky-table-{width}',value['table']['rows']==100 and value['table']['clientHeight']<=440 and value['table']['scrollHeight']>value['table']['clientHeight'] and all(t['position']=='sticky' and t['top']=='0px' for t in value['table']['sticky']),value['table'])
  soft(f'no-body-horizontal-overflow-{width}',value['rootWidth']<=width and value['bodyWidth']<=width,{'root':value['rootWidth'],'body':value['bodyWidth']})
  r.browse('screenshot','--viewport',out/f'actual-layout-{width}.png')
 r.browse('viewport','1440x1100')
 before=js("(() => {const s=document.getElementById('room-table-scroll');s.scrollTop=300;const b=document.querySelectorAll('.room-button')[5];b.focus({preventScroll:true});return {scroll:s.scrollTop,room:b.textContent,footer:document.getElementById('footer-status').textContent,headY:document.querySelector('th').getBoundingClientRect().top,scrollY:s.getBoundingClientRect().top}})()")
 time.sleep(5.6)
 after=js("({scroll:document.getElementById('room-table-scroll').scrollTop,room:document.activeElement.textContent,footer:document.getElementById('footer-status').textContent})")
 soft('automatic-five-second-scroll-focus-preserved',before['scroll']==after['scroll']==300 and before['room']==after['room'] and before['footer']!=after['footer'],{'before':before,'after':after})
 soft('sticky-header-screen-position',abs(before['headY']-before['scrollY'])<3,{'headY':before['headY'],'scrollY':before['scrollY']})
 r.browse('click','#next-page');r.until("document.querySelectorAll('#room-rows tr').length===4 && !document.getElementById('refresh').disabled")
 page=js("({top:document.getElementById('room-table-scroll').scrollTop,description:document.getElementById('page-description').textContent,previousEnabled:!document.getElementById('previous-page').disabled})")
 soft('page-resets-table-scroll',page['top']==0 and page['previousEnabled'],page)
 r.browse('click','#previous-page');r.until("document.querySelectorAll('#room-rows tr').length===100 && !document.getElementById('refresh').disabled")
 r.js("document.getElementById('room-table-scroll').scrollTop=300; true");r.browse('select','#game-filter','arena-b');r.browse('click','#filters button[type=submit]');r.until("document.querySelectorAll('#room-rows tr').length===2 && !document.getElementById('refresh').disabled")
 filtered=js("({top:document.getElementById('room-table-scroll').scrollTop,text:document.getElementById('filter-description').textContent})")
 soft('filter-resets-table-scroll',filtered['top']==0 and 'arena-b' in filtered['text'],filtered)
 r.browse('click','#reset-filters');r.until("document.querySelectorAll('#room-rows tr').length===100 && !document.getElementById('refresh').disabled")
 control=Path('/tmp/campfire-dashboard-ui-history-control-20261023.js')
 control.write_text("""(() => {const original=window.fetch.bind(window);const make=(i,t,cpu=20)=>({sample_seq:i,collected_at:t,metrics:{active_connections:i},rates:{inputs_per_second:i*2,snapshots_per_second:i},process:{cpu_percent:cpu,rss_bytes:10485760}});window.__historyCase='eleven';window.fetch=async(url,opt)=>{const response=await original(url,opt);if(!String(url).includes('/history'))return response;const body=await response.json(),now=Date.now();let samples=[];switch(window.__historyCase){case 'one':samples=[make(1,now)];break;case 'eleven':samples=Array.from({length:11},(_,i)=>make(i+1,now-10000+i*1000));break;case 'fifteen':samples=[make(1,now-900000),make(901,now)];break;case 'gap-null-multicore':samples=[make(1,now-5000,0),make(2,now-4000,null),make(3,now,125)];break;}body.data={samples,reset:true,oldest_seq:samples.length?samples[0].sample_seq:null};return new Response(JSON.stringify(body),{status:200,headers:{'Content-Type':'application/json'}})};return true})()""")
 r.browse('eval',control)
 for case in ['empty','one','eleven','fifteen','gap-null-multicore']:
  r.js("window.__historyCase="+json.dumps(case)+";document.getElementById('refresh').click();true");r.until("!document.getElementById('refresh').disabled")
  value=js("({description:document.getElementById('history-description').textContent,connectionSummary:document.getElementById('chart-connections-summary').textContent,cpuValue:document.getElementById('chart-cpu-value').textContent,charts:[...document.querySelectorAll('svg.chart')].map(s=>({id:s.id,aria:s.getAttribute('aria-label'),text:[...s.querySelectorAll('text')].map(t=>t.textContent),paths:[...s.querySelectorAll('path')].map(p=>p.getAttribute('d')),points:s.querySelectorAll('circle').length}))})")
  d.dump(out/f'controlled-history-{case}.json',value);r.browse('screenshot','--viewport',out/f'controlled-history-{case}.png')
  success={'empty':'첫 이력' in value['description'],'one':'관측 시작' in value['description'],'eleven':'관측 10초' in value['description'],'fifteen':'관측 15분 0초' in value['description'],'gap-null-multicore':value['cpuValue']=='125%'}[case]
  soft('controlled-history-'+case,success,value['description'])
 served={}
 for name,route in [('index.html','/admin'),('app.js','/admin/assets/app.js'),('style.css','/admin/assets/style.css')]:
  _,_,body=d.http(base,route);served[name]={'served':hashlib.sha256(body.encode()).hexdigest(),'source':d.sha(ROOT/'crates/campfire-server/src/dashboard/assets'/name)}
 soft('actual-served-assets-match-source',all(v['served']==v['source'] for v in served.values()),served)
 (work/'stop.json').write_text('{}');fixture.wait(timeout=8)
 result['success']=not result['failures']
except Exception as e:result['failure']=str(e)
finally:
 r.close()
 if temp:temp.cleanup()
 result['source_sha256_after']=d.digest(d.source_manifest());result['binary_sha256_after']=d.sha(d.BINARY)
 result['source_unchanged']=result['source_sha256_before']==result['source_sha256_after'];result['binary_unchanged']=binary==result['binary_sha256_after'];result['owned_cleanup_complete']=all(p.poll() is not None for p in r.processes)
 try:result['secret_scan_files']=r.scan()
 except Exception as e:result['success']=False;result['failure']=str(e)
 d.dump(out/'summary.json',result)
 print(json.dumps({'success':result['success'],'failures':result['failures'],'failure':result.get('failure'),'artifact':str(out)},ensure_ascii=False))
