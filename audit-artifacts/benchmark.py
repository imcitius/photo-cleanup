"""Repeatable local metadata benchmark: 55,000 rows / 11,000 families.
Synthetic paths do not exist: this measures SQL, planning and serialization,
not image I/O or NAS latency. Soft budgets are for this development machine.
"""
import json,pathlib,socket,sqlite3,statistics,subprocess,tempfile,time,urllib.request
root=pathlib.Path(__file__).resolve().parents[1]
with tempfile.TemporaryDirectory(prefix='pc-audit-bench-') as tmp:
 tmp=pathlib.Path(tmp).resolve();dbpath=tmp/'db'
 subprocess.run([str(root/'target/debug/photo-cleanup'),'--db',str(dbpath),'status'],capture_output=True,check=True)
 with sqlite3.connect(dbpath) as db:
  db.executemany('INSERT INTO files(id,path,name,disk,dev,inode,nlink,size,mtime,pixel_hash) VALUES(?,?,?,?,1,?,1,1000000,1,?)',
   ((i+1,str(tmp/'absent'/str(i%5)/f'{i//5}.jpg'),f'{i//5}.jpg','disk',i+1,(i//5).to_bytes(32,'little')) for i in range(55000)))
  db.executemany("INSERT INTO families(id,key_kind,keeper_file) VALUES(?,'linked',?)",((i+1,i*5+1) for i in range(11000)))
  db.executemany('INSERT INTO family_members(family_id,file_id,role,quality) VALUES(?,?,?,1)',((i//5+1,i+1,'original' if i%5==0 else 'copy') for i in range(55000)))
 with socket.socket() as s:s.bind(('127.0.0.1',0));port=s.getsockname()[1]
 proc=subprocess.Popen([str(root/'target/debug/photo-cleanup'),'--db',str(dbpath),'serve','--bind',f'127.0.0.1:{port}','--thumbs',str(tmp/'thumbs')],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
 def req(path,body=None):
  r=urllib.request.Request(f'http://127.0.0.1:{port}/api'+path,data=json.dumps(body).encode() if body else None,headers={'Content-Type':'application/json'})
  with urllib.request.urlopen(r,timeout=180) as response:return json.load(response)
 try:
  for _ in range(300):
   try:req('/status');break
   except OSError:time.sleep(.05)
  results={'files':55000,'families':11000,'build':'dev opt-level=1','storage':'local temporary SQLite; absent image paths','measurements':{}}
  for name,path,body,budget in [
   ('family_list_100','/families?limit=100',None,1),
   ('single_family_preview','/preview',{'kind':'plan-apply','params':{'family_id':1}},1),
   ('full_44000_candidate_preview','/preview',{'kind':'plan-apply','params':{}},10)]:
   samples=[]
   for i in range(4):
    start=time.perf_counter();data=req(path,body);elapsed=time.perf_counter()-start
    if i:samples.append(round(elapsed,4))
   results['measurements'][name]={'seconds':samples,'median':statistics.median(samples),'soft_budget_seconds':budget,'within_budget':max(samples)<=budget,'items':len(data.get('items',data.get('families',[])))}
  print(json.dumps(results,indent=2))
 finally:proc.terminate();proc.wait(timeout=20)
