"""Check all real migration prefixes using the actual Rust Db::open via CLI."""
import json,pathlib,re,sqlite3,subprocess,tempfile
root=pathlib.Path(__file__).resolve().parents[1]
migrations=re.findall(r'r#"(.*?)"#', (root/'crates/pc-db/src/schema.rs').read_text(), re.S)
results=[]
with tempfile.TemporaryDirectory(prefix='pc-migration-audit-') as tmp:
 for n in range(len(migrations)+1):
  p=pathlib.Path(tmp)/f'v{n}.db'
  with sqlite3.connect(p) as db:
   db.execute('CREATE TABLE schema_version(version INTEGER NOT NULL)')
   for version,sql in enumerate(migrations[:n],1):
    db.executescript(sql);db.execute('INSERT INTO schema_version VALUES(?)',(version,));db.commit()
   if n>=1:db.execute("INSERT INTO runs VALUES(7,123,NULL,'[]','audit-old')")
   if n>=2:
    db.execute("INSERT INTO files(id,path,name,disk,dev,inode,nlink,size,mtime) VALUES(42,'/archive/keep.bmp','keep.bmp','disk',1,42,1,100,123)")
    db.execute("INSERT INTO meta(file_id,camera_model,taken_at) VALUES(42,'camera',123)")
   if n>=3:
    db.execute("INSERT INTO families(id,key_kind,keeper_file) VALUES(9,'single',42)")
    db.execute("INSERT INTO family_members(family_id,file_id,role) VALUES(9,42,'original')")
   if n>=9:db.execute('INSERT INTO manual_keepers VALUES(42)')
   if n>=10:db.execute('INSERT INTO manual_rejects VALUES(42,123)')
   db.commit()
   tables=[r[0] for r in db.execute("SELECT name FROM sqlite_master WHERE type='table' AND name != 'schema_version'")]
   before={table:([r[1] for r in db.execute(f'PRAGMA table_info({table})')],db.execute(f'SELECT * FROM {table} ORDER BY rowid').fetchall()) for table in tables}
  result=subprocess.run([str(root/'target/debug/photo-cleanup'),'--db',str(p),'status'],capture_output=True,text=True)
  assert result.returncode==0,result.stderr
  with sqlite3.connect(p) as db:
   assert db.execute('SELECT max(version) FROM schema_version').fetchone()[0]==len(migrations)
   for table,(cols,rows) in before.items():assert db.execute(f'SELECT {",".join(cols)} FROM {table} ORDER BY rowid').fetchall()==rows,(n,table)
   assert db.execute('PRAGMA foreign_key_check').fetchall()==[]
  results.append({'from':n,'to':len(migrations),'old_columns_and_rows_preserved':True})
print(json.dumps(results,indent=2))
