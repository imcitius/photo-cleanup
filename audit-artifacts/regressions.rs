//! Audit regression proposals: these assert desired safety and fail on 0.2.15.
use pc_db::{Db, files::NewFile};
use std::{fs, path::Path};

fn add(db: &Db, run: i64, p: &Path) -> i64 {
    let md = fs::metadata(p).unwrap();
    db.upsert_file(&NewFile {
        path: p.display().to_string(), name: p.file_name().unwrap().to_string_lossy().into(),
        dev: pc_core::volume::device_of(&md,p) as i64, inode: pc_core::volume::inode_of(&md) as i64, size: md.len() as i64, mtime: pc_core::time::mtime_unix(&md),
        container: Some("bmp".into()), phash: Some(1), pixel_hash: Some(vec![1;32]),
        ..Default::default()
    }, run).unwrap()
}
fn candidate(id: i64, p: &Path) -> pc_family::plan::Candidate {
    pc_family::plan::Candidate { file_id: id, family_id: 0, path: p.display().to_string(),
        size: fs::metadata(p).unwrap().len() as i64, role: pc_family::Role::Unknown,
        keeper_id: 0, keeper_path: String::new(), reason: String::new(), manual: true }
}
#[test]
fn a01_different_rgb_pixels_must_not_pass_same_picture() {
    let t = tempfile::tempdir().unwrap();
    // Find two visibly different colours with exactly the same integer luminance.
    let red = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(128,128,image::Rgb([255,0,0])));
    let luma = red.to_luma8().get_pixel(0,0)[0];
    let green = (0..=255).map(|g| image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(128,128,image::Rgb([0,g,0]))))
        .find(|im| im.to_luma8().get_pixel(0,0)[0] == luma).unwrap();
    let a=t.path().join("red.bmp"); let b=t.path().join("green.bmp");
    red.save(&a).unwrap(); green.save(&b).unwrap();
    assert_ne!(red.to_rgb8(),green.to_rgb8());
    assert!(!pc_apply::same_picture(&a,&b).unwrap(), "different colours were accepted as exact pixels");
}
#[test]
fn a02_organize_undo_must_preserve_every_archive_file() {
    let t=tempfile::tempdir().unwrap(); let root=t.path().join("archive");
    let src=root.join("old/photo.bmp"); fs::create_dir_all(src.parent().unwrap()).unwrap();
    fs::write(&src,b"photo").unwrap();
    let litter=src.with_file_name("._unrelated"); fs::write(&litter,b"resource fork").unwrap();
    let db=Db::open(&t.path().join("db")).unwrap(); let run=db.start_run(&[root.display().to_string()],"audit").unwrap();
    let id=add(&db,run,&src);
    let plan=pc_organize::compute(&db,&pc_organize::Options{root:t.path().join("out"),respect_lightroom:false,..Default::default()}).unwrap();
    assert_eq!(plan.moves.len(),1); assert_eq!(plan.moves[0].file_id,id);
    assert_eq!(pc_apply::organize(&db,run,&plan.moves).unwrap().moved,1);
    pc_apply::undo_run(&db,run).unwrap();
    assert_eq!(fs::read(&src).unwrap(),b"photo");
    assert!(litter.exists(),"organize deleted an unjournalled archive file");
}
#[test]
fn a03_old_journal_must_not_change_reused_file_id_after_reset() {
    let t=tempfile::tempdir().unwrap(); let p=t.path().join("old.bmp"); fs::write(&p,b"old").unwrap();
    let db=Db::open(&t.path().join("db")).unwrap(); let run=db.start_run(&[],"audit").unwrap();
    let old=add(&db,run,&p); pc_apply::files::quarantine_file(&db,run,&candidate(old,&p),None).unwrap();
    let jid=db.journal_quarantined(None).unwrap()[0].id;
    db.reset_index().unwrap();
    let other=t.path().join("other.bmp"); fs::write(&other,b"new").unwrap(); let new=add(&db,run,&other); assert_eq!(old,new);
    pc_apply::purge_entry(&db,jid).unwrap();
    let state:String=db.conn.query_row("SELECT state FROM files WHERE id=?1",[new],|r|r.get(0)).unwrap();
    assert_eq!(state,"present","purging old quarantine hid an unrelated new file");
}
#[test]
fn a04_undo_must_not_take_a_sidecar_it_did_not_move() {
    let t=tempfile::tempdir().unwrap(); let p=t.path().join("photo.bmp"); fs::write(&p,b"photo").unwrap();
    let q=t.path().join(pc_core::QUARANTINE_DIR); fs::create_dir(&q).unwrap(); fs::write(q.join("photo.xmp"),b"unrelated metadata").unwrap();
    let db=Db::open(&t.path().join("db")).unwrap(); let run=db.start_run(&[],"audit").unwrap(); let id=add(&db,run,&p);
    pc_apply::files::quarantine_file(&db,run,&candidate(id,&p),None).unwrap();
    pc_apply::undo(&db,db.journal_quarantined(None).unwrap()[0].id).unwrap();
    assert!(q.join("photo.xmp").exists(),"undo moved an unrelated sidecar which was never journalled");
}
#[test]
fn a05_missing_source_must_count_as_failed_thumbnail_rebuild() {
    let t=tempfile::tempdir().unwrap(); let p=t.path().join("photo.bmp"); fs::write(&p,b"photo").unwrap();
    let db=Db::open(&t.path().join("db")).unwrap(); let run=db.start_run(&[],"audit").unwrap(); add(&db,run,&p); fs::remove_file(&p).unwrap();
    let r=pc_work::thumbs::rebuild(&db,&pc_core::ThumbStore::new(t.path().join("thumbs")),true,None,&Default::default()).unwrap();
    assert_eq!(r.checked,1); assert_eq!(r.failed,1,"a missing/offline file was not counted as unreadable");
}
#[test]
fn a06_core_quarantine_must_recheck_a_new_lightroom_lock() {
    let t=tempfile::tempdir().unwrap();let root=t.path().join("archive");
    let preview=root.join("Library Previews.lrdata");fs::create_dir_all(&preview).unwrap();fs::write(preview.join("cache"),b"cached").unwrap();
    let db=Db::open(&t.path().join("db")).unwrap();pc_work::scan::run(&db,&[root.clone()],"audit").unwrap();
    let b=db.list_bundles(&Default::default()).unwrap().into_iter().find(|b| b.path==preview.display().to_string()).unwrap();
    assert!(b.removable());fs::write(root.join("Library.lrcat.lock"),b"open").unwrap();
    let _=pc_apply::quarantine(&db,db.latest_run().unwrap().unwrap(),&b,None);
    assert!(preview.exists(),"CLI/core moved previews after Lightroom was opened");
}
