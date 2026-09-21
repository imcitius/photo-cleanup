"""Isolated HTTP audit; creates/deletes only its own TemporaryDirectory."""
import contextlib, hashlib, json, pathlib, random, socket, sqlite3, struct, subprocess, tempfile, time, urllib.request, urllib.error
ROOT=pathlib.Path(__file__).resolve().parents[1]
results={}
def bmp(seed):
    w=h=128; r=random.Random(seed); pixels=r.randbytes(w*h*3)
    return b'BM'+struct.pack('<IHHI',54+len(pixels),0,0,54)+struct.pack('<IiiHHIIiiII',40,w,h,1,24,0,len(pixels),0,0,0,0)+pixels
with tempfile.TemporaryDirectory(prefix='pc-audit-') as tmp:
    root=pathlib.Path(tmp).resolve(); archive=root/'archive'; archive.mkdir(); out=root/'output';out.mkdir()
    for folder in ['originals','backup']: (archive/folder).mkdir()
    for i in range(150):
        for folder in ['originals','backup']: (archive/folder/f'20240101_{i:04}.bmp').write_bytes(bmp(i))
    def snapshot(): return {str(p.relative_to(archive)):hashlib.sha256(p.read_bytes()).hexdigest() for p in archive.rglob('*') if p.is_file()}
    before=snapshot()
    with socket.socket() as sock: sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
    log=open(ROOT/'audit-artifacts/scenario-server.log','w')
    proc=subprocess.Popen([str(ROOT/'target/debug/photo-cleanup'),'--db',str(root/'db'),'serve','--bind',f'127.0.0.1:{port}','--thumbs',str(root/'thumbs')],stdout=log,stderr=log)
    def api(method,path,body=None):
        req=urllib.request.Request(f'http://127.0.0.1:{port}/api'+path,data=json.dumps(body).encode() if body is not None else None,method=method,headers={'Content-Type':'application/json'})
        with urllib.request.urlopen(req,timeout=180) as r:return json.load(r)
    def wait(j):
        for _ in range(9000):
            r=api('GET',f'/jobs/{j}')
            if r['state'] not in ['queued','running']:
                assert r['state']=='done',r
                assert not r.get('progress',{}).get('refusals'),r
                # Known finish-publication race is measured separately by API stress.
                time.sleep(.01);return r
            time.sleep(.02)
        raise AssertionError('job timed out')
    def job(kind,params):return wait(api('POST','/jobs',{'kind':kind,'params':params})['job_id'])
    def preview(kind,params):return api('POST','/preview',{'kind':kind,'params':params})
    def apply(p):return wait(api('POST','/jobs',{'kind':p['kind'],'params':p['params'],'plan_token':p['token']})['job_id'])
    try:
        for _ in range(300):
            try:api('GET','/status');break
            except (OSError,urllib.error.URLError):time.sleep(.05)
        started=time.perf_counter();job('all',{'roots':[str(archive)],'min_size':0})
        results['all_300_seconds']=round(time.perf_counter()-started,3)
        p=preview('plan-apply',{'roles':['copy']});results['automatic_candidates']=len(p['items']);assert len(p['items'])==150
        apply(p);assert len(snapshot())==300 # includes quarantine files
        for e in api('GET','/journal'):
            if e['status']=='done':apply(preview('journal-undo',{'journal_id':e['id']}))
        results['300_file_quarantine_undo_sha256_identical']=snapshot()==before;assert snapshot()==before
        p=preview('organize-apply',{'root':str(out),'allow_duplicates':True});assert len(p['items'])==300
        j=apply(p);apply(preview('organize-undo',{'run_id':j['run_id']}))
        results['300_file_organize_undo_sha256_identical']=snapshot()==before;assert snapshot()==before
        started=time.perf_counter();job('thumbs',{'all':True});results['rebuild_300_thumbnails_seconds']=round(time.perf_counter()-started,3)
        families=api('GET','/families?limit=1')['families']; f=families[0];fid=f['id']
        members=f['members']; a,b=sorted(m['file_id'] for m in members)[:2]
        api('POST',f'/families/{fid}/keep-only',{'file_id':a});api('POST',f'/families/{fid}/keep-only',{'file_id':b})
        job('families',{})
        with sqlite3.connect(root/'db') as db:
            actual=db.execute('select keeper_file from families where id=(select family_id from family_members where file_id=?)',(b,)).fetchone()[0]
            results['last_manual_choice']={'expected':b,'actual_after_rebuild':actual,'preserved':actual==b}
            actual_family=db.execute('select family_id from family_members where file_id=?',(b,)).fetchone()[0]
        p=preview('plan-apply',{'family_id':actual_family});results['manual_choice_plan_ids']=[i['file_id'] for i in p['items']]
        apply(p)
        with sqlite3.connect(root/'db') as db:
            results['manual_choice_present_after_apply']=db.execute("select count(*) from files where id in (?,?) and state='present'",(a,b)).fetchone()[0]
        # Old-database quarantine with a nested directory, no journal.
        nested=archive/'old' / '.photo-cleanup-quarantine'/'Library Previews.lrdata'/'sub';nested.mkdir(parents=True)
        (nested/'cache').write_bytes(b'preview cache')
        job('scan',{'roots':[str(archive)]})
        orphan=api('GET','/quarantine/orphans'); item=next(i for i in orphan['items'] if i['name']=='cache')
        result=api('POST','/quarantine/orphans',{})
        results['nested_orphan_restore']={'source':item['path'].replace(str(root),'$TEMP'),'restore_to':item['restore_to'].replace(str(root),'$TEMP'),'response':result,'still_inside_quarantine':(archive/'old'/'.photo-cleanup-quarantine'/'Library Previews.lrdata'/'cache').exists()}
    finally:
        proc.terminate();proc.wait(timeout=20);log.close()
print(json.dumps(results,ensure_ascii=False,indent=2))
