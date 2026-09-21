use axum::{body::{Body,to_bytes},http::Request};
use tower::ServiceExt;
use serde_json::{json,Value};
use std::{sync::Arc,time::Duration};
async fn req(st:&Arc<pc_api::AppState>, method:&str,path:&str,v:Value)->Value {
 let r=pc_api::router(st.clone()).oneshot(Request::builder().method(method).uri(path).header("content-type","application/json").body(Body::from(v.to_string())).unwrap()).await.unwrap();
 let status=r.status();let bytes=to_bytes(r.into_body(),10000000).await.unwrap();let data:Value=serde_json::from_slice(&bytes).unwrap();assert!(status.is_success(),"{status}: {data}");data
}
async fn job(st:&Arc<pc_api::AppState>,kind:&str,params:Value){
 let id=req(st,"POST","/api/jobs",json!({"kind":kind,"params":params})).await["job_id"].as_i64().unwrap();
 for _ in 0..1000 {let j=req(st,"GET",&format!("/api/jobs/{id}"),Value::Null).await;
 if j["state"]=="done" {tokio::time::sleep(Duration::from_millis(10)).await;return}
 assert!(j["state"]=="queued"||j["state"]=="running","{j}");tokio::time::sleep(Duration::from_millis(20)).await;}
 panic!("timed out")
}
async fn world()->(tempfile::TempDir,Arc<pc_api::AppState>,i64,i64,String){
 let t=tempfile::tempdir().unwrap();let root=t.path().canonicalize().unwrap();let archive=root.join("archive");let scans=archive.join("scans");let exports=archive.join("exports");std::fs::create_dir_all(&scans).unwrap();std::fs::create_dir_all(&exports).unwrap();
 let img=image::RgbImage::from_fn(640,480,|x,y| {let a=((x as f32/640.0*4.1+0.9)*std::f32::consts::TAU).sin();let b=((y as f32/480.0*2.8+2.4)*std::f32::consts::TAU).cos();let v=|k:f32|(128.0+(a+b)*45.0*k).clamp(0.0,255.0) as u8;image::Rgb([v(1.0),v(0.85),v(0.6)])});
 img.save(scans.join("frame.bmp")).unwrap();img.save(exports.join("frame.jpg")).unwrap();
 let st=Arc::new(pc_api::AppState::new(&root.join("db"),&root.join("thumbs"),None).unwrap());
 job(&st,"index",json!({"roots":[archive],"min_size":0})).await;
 // The task starts from an established group. Supply an explicit shared
 // provenance link, so this test isolates curation from perceptual recall.
 st.db.lock().unwrap().conn.execute("UPDATE meta SET xmp_original_id='audit-source'",[]).unwrap();
 job(&st,"families",json!({})).await;
 let groups=req(&st,"GET","/api/families",Value::Null).await;assert_eq!(groups["total"],1,"{groups}");let family=groups["families"][0]["id"].as_i64().unwrap();
 let jpeg=groups["families"][0]["members"].as_array().unwrap().iter().find(|m|m["name"]=="frame.jpg").unwrap()["file_id"].as_i64().unwrap();
 let plan=req(&st,"POST","/api/preview",json!({"kind":"plan-apply","params":{}})).await;assert_eq!(plan["total_files"],0,"encodings must not be automatic copies: {plan}");
 (t,st,family,jpeg,exports.display().to_string())
}
#[tokio::test]
async fn b01_single_bmp_jpeg_choice_is_in_reviewed_plan(){
 let (_t,st,family,jpeg,_)=world().await;
 req(&st,"POST",&format!("/api/families/{family}/keep-only"),json!({"file_id":jpeg})).await;
 let p=req(&st,"POST","/api/preview",json!({"kind":"plan-apply","params":{"family_id":family}})).await;
 assert_eq!(p["total_files"],1,"{p}");assert_eq!(p["items"][0]["manual"],true);
}
#[tokio::test]
async fn b02_bulk_bmp_jpeg_choice_must_reach_the_folder_plan(){
 let (_t,st,_family,_jpeg,dir)=world().await;
 let r=req(&st,"POST","/api/keepers/keep-folder-only",json!({"dir":dir})).await;assert_eq!(r["marked"],1);
 let p=req(&st,"POST","/api/preview",json!({"kind":"plan-apply","params":{}})).await;assert_eq!(p["total_files"],1,"global plan must hold the manual choice");
 let scoped=req(&st,"POST","/api/preview",json!({"kind":"plan-apply","params":{"keeper_folder":dir}})).await;
 assert_eq!(scoped["total_files"],1,"the UI's folder plan dropped the rejected BMP: {scoped}");
}
