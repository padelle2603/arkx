use std::fs;
use std::path::PathBuf;

fn backend() -> arkx::core::backends::BackendManager {
    arkx::core::backends::BackendManager::new()
}

fn create_src_dir(base: &std::path::Path) -> (PathBuf, Vec<PathBuf>) {
    let src = base.join("src");
    fs::create_dir(&src).unwrap();
    let subdir = src.join("nested");
    fs::create_dir(&subdir).unwrap();
    fs::write(src.join("hello.txt"), b"Hello, arkx!").unwrap();
    fs::write(src.join("data.bin"), vec![0xAB; 4096]).unwrap();
    fs::write(subdir.join("inner.txt"), b"nested content").unwrap();
    fs::create_dir(src.join("empty_folder")).unwrap();
    (src.clone(), vec![src])
}

#[test]
fn zip_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let (_src, sources) = create_src_dir(tmp.path());
    let archive = tmp.path().join("test.zip");
    let extract_dir = tmp.path().join("out_zip");

    let bm = backend();
    bm.create(&archive, &sources, 6, None, None).unwrap();
    assert!(archive.exists());
    assert!(fs::metadata(&archive).unwrap().len() > 0);

    let info = bm.detect_and_list(&archive).unwrap();
    assert_eq!(info.format, "ZIP");
    assert!(info.num_files >= 3);

    bm.extract(&archive, &extract_dir, None, None, None)
        .unwrap();
    assert_eq!(
        fs::read(extract_dir.join("src/hello.txt")).unwrap(),
        b"Hello, arkx!"
    );
    assert_eq!(
        fs::read(extract_dir.join("src/data.bin")).unwrap(),
        vec![0xAB; 4096]
    );
    assert_eq!(
        fs::read(extract_dir.join("src/nested/inner.txt")).unwrap(),
        b"nested content"
    );
    assert!(extract_dir.join("src/empty_folder").is_dir());
}

#[test]
fn tar_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let (_, sources) = create_src_dir(tmp.path());
    let archive = tmp.path().join("test.tar");
    let extract_dir = tmp.path().join("out_tar");

    let bm = backend();
    bm.create(&archive, &sources, 6, None, None).unwrap();

    let info = bm.detect_and_list(&archive).unwrap();
    assert!(info.format.contains("TAR"));

    bm.extract(&archive, &extract_dir, None, None, None)
        .unwrap();
    assert_eq!(
        fs::read(extract_dir.join("src/hello.txt")).unwrap(),
        b"Hello, arkx!"
    );
}

#[test]
fn tar_gz_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let (_, sources) = create_src_dir(tmp.path());
    let archive = tmp.path().join("test.tar.gz");
    let extract_dir = tmp.path().join("out_tgz");

    let bm = backend();
    bm.create(&archive, &sources, 6, None, None).unwrap();

    let info = bm.detect_and_list(&archive).unwrap();
    assert!(info.format.contains("TAR.GZ") || info.format.contains("TAR"));

    bm.extract(&archive, &extract_dir, None, None, None)
        .unwrap();
    assert_eq!(
        fs::read(extract_dir.join("src/hello.txt")).unwrap(),
        b"Hello, arkx!"
    );
}

#[test]
fn tar_zst_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let (_, sources) = create_src_dir(tmp.path());
    let archive = tmp.path().join("test.tar.zst");
    let extract_dir = tmp.path().join("out_zst");

    let bm = backend();
    bm.create(&archive, &sources, 6, None, None).unwrap();

    bm.extract(&archive, &extract_dir, None, None, None)
        .unwrap();
    assert_eq!(
        fs::read(extract_dir.join("src/hello.txt")).unwrap(),
        b"Hello, arkx!"
    );
}

#[test]
fn single_file_roundtrip_all_codecs() {
    let content = b"single-file payload".to_vec();
    for ext in ["gz", "bz2", "xz", "zst", "lz4"] {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("hello.txt");
        fs::write(&src, &content).unwrap();
        let archive = tmp.path().join(format!("hello.txt.{ext}"));
        let out = tmp.path().join("out");

        let bm = backend();
        bm.create(&archive, &[src], 6, None, None).unwrap();
        assert!(archive.exists());
        assert!(fs::metadata(&archive).unwrap().len() > 0);

        bm.extract(&archive, &out, None, None, None).unwrap();
        // Archive named hello.txt.<ext> extracts back to hello.txt
        // (file_stem of the archive name).
        assert_eq!(fs::read(out.join("hello.txt")).unwrap(), content);
    }
}

#[test]
fn single_file_rejects_folders_and_multi() {
    let tmp = tempfile::tempdir().unwrap();
    let (src, _) = create_src_dir(tmp.path());
    let archive = tmp.path().join("src.gz");
    let bm = backend();
    assert!(bm.create(&archive, &[src], 6, None, None).is_err());
    assert!(!archive.exists());

    let a = tmp.path().join("a.txt");
    let b = tmp.path().join("b.txt");
    fs::write(&a, "a").unwrap();
    fs::write(&b, "b").unwrap();
    let archive2 = tmp.path().join("multi.gz");
    assert!(bm.create(&archive2, &[a, b], 6, None, None).is_err());
    assert!(!archive2.exists());
}

#[test]
fn list_shows_correct_sizes() {
    let tmp = tempfile::tempdir().unwrap();
    let (_, sources) = create_src_dir(tmp.path());
    let archive = tmp.path().join("sized.zip");

    let bm = backend();
    bm.create(&archive, &sources, 6, None, None).unwrap();

    let info = bm.detect_and_list(&archive).unwrap();
    let hello = info
        .entries
        .iter()
        .find(|e| e.path.contains("hello.txt"))
        .unwrap();
    assert_eq!(hello.size, 12);
    assert!(!hello.is_dir);
}

#[test]
fn extract_to_empty_dir_works() {
    let tmp = tempfile::tempdir().unwrap();
    let (_, sources) = create_src_dir(tmp.path());
    let archive = tmp.path().join("test.zip");

    let bm = backend();
    bm.create(&archive, &sources, 6, None, None).unwrap();

    let extract_dir = tmp.path().join("brand_new_dir");
    bm.extract(&archive, &extract_dir, None, None, None)
        .unwrap();
    assert!(extract_dir.join("src/hello.txt").exists());
}

#[test]
fn extract_nonexistent_archive_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let bm = backend();
    let result = bm.detect_and_list(&tmp.path().join("nope.zip"));
    assert!(result.is_err());
}

#[test]
fn create_empty_sources_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let archive = tmp.path().join("empty.zip");
    let bm = backend();
    let result = bm.create(&archive, &[], 6, None, None);
    assert!(result.is_err());
}

#[test]
fn backend_manager_detects_and_lists() {
    let tmp = tempfile::tempdir().unwrap();
    let (_, sources) = create_src_dir(tmp.path());
    let archive = tmp.path().join("detect.zip");

    let bm = backend();
    bm.create(&archive, &sources, 6, None, None).unwrap();

    let info = bm.detect_and_list(&archive).unwrap();
    assert!(info.num_files >= 1);
    assert_eq!(info.format, "ZIP");
}

#[test]
fn add_to_zip_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let (_, sources) = create_src_dir(tmp.path());
    let archive = tmp.path().join("add.zip");

    let bm = backend();
    bm.create(&archive, &sources, 6, None, None).unwrap();

    let extra = tmp.path().join("extra.txt");
    fs::write(&extra, b"added content").unwrap();
    bm.add(
        &archive,
        &[(extra, "src/added.txt".to_string())],
        None,
        None,
    )
    .unwrap();

    let info = bm.detect_and_list(&archive).unwrap();
    assert!(info.entries.iter().any(|e| e.path == "src/added.txt"));

    let out = tmp.path().join("out_add");
    bm.extract(&archive, &out, None, None, None).unwrap();
    assert_eq!(
        fs::read(out.join("src/added.txt")).unwrap(),
        b"added content"
    );
}

#[test]
fn add_to_unsupported_format_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let _ = create_src_dir(tmp.path());
    // .gz is stream-compressed (extract-only): adding must fail with a clear
    // error, not silently corrupt the archive.
    let archive = tmp.path().join("x.gz");
    let _ = fs::copy(tmp.path().join("src/hello.txt"), &archive);

    let bm = backend();
    let extra = tmp.path().join("extra.txt");
    fs::write(&extra, b"x").unwrap();
    let err = bm
        .add(&archive, &[(extra, "extra.txt".to_string())], None, None)
        .unwrap_err();
    assert!(err.to_string().contains("cannot add to"));
}

#[test]
fn remove_from_zip_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let (_, sources) = create_src_dir(tmp.path());
    let archive = tmp.path().join("rm.zip");

    let bm = backend();
    bm.create(&archive, &sources, 6, None, None).unwrap();

    bm.remove(&archive, &["src/data.bin".to_string()], None, None)
        .unwrap();

    let info = bm.detect_and_list(&archive).unwrap();
    assert!(!info.entries.iter().any(|e| e.path == "src/data.bin"));
    assert!(info.entries.iter().any(|e| e.path.contains("hello.txt")));

    let out = tmp.path().join("out_rm");
    bm.extract(&archive, &out, None, None, None).unwrap();
    assert!(out.join("src/hello.txt").exists());
    assert!(!out.join("src/data.bin").exists());
}

#[test]
fn remove_unsupported_format_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let _ = create_src_dir(tmp.path());
    let archive = tmp.path().join("x.gz");
    let _ = fs::copy(tmp.path().join("src/hello.txt"), &archive);

    let bm = backend();
    let err = bm
        .remove(&archive, &["hello.txt".to_string()], None, None)
        .unwrap_err();
    assert!(err.to_string().contains("cannot remove from"));
}

#[test]
fn set_comment_zip_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let (_, sources) = create_src_dir(tmp.path());
    let archive = tmp.path().join("cmt.zip");

    let bm = backend();
    bm.create(&archive, &sources, 6, None, None).unwrap();
    assert_eq!(
        bm.detect_and_list(&archive).unwrap().comment.as_deref(),
        None
    );

    bm.set_comment(&archive, "hello comment").unwrap();
    assert_eq!(
        bm.detect_and_list(&archive).unwrap().comment.as_deref(),
        Some("hello comment")
    );
}

#[test]
fn set_comment_unsupported_format_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let archive = tmp.path().join("x.7z");
    fs::write(&archive, b"x").unwrap();
    let bm = backend();
    let err = bm.set_comment(&archive, "c").unwrap_err();
    assert!(err.to_string().contains("read-only"));
}

#[test]
fn zip_aes_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let (_, sources) = create_src_dir(tmp.path());
    let archive = tmp.path().join("sec.zip");

    let bm = backend();
    bm.create(&archive, &sources, 6, Some("secret"), None)
        .unwrap();

    let info = bm.detect_and_list(&archive).unwrap();
    assert!(
        info.entries.iter().any(|e| e.encrypted),
        "created zip is not encrypted"
    );

    let out = tmp.path().join("out_sec");
    bm.extract(&archive, &out, None, Some("secret"), None)
        .unwrap();
    assert_eq!(
        fs::read(out.join("src/hello.txt")).unwrap(),
        b"Hello, arkx!"
    );

    // Wrong password: typed error, nothing written on disk (the 7z fallback
    // is skipped once the native backend already rejected the credentials).
    let out_bad = tmp.path().join("out_bad");
    let err = bm
        .extract(&archive, &out_bad, None, Some("wrong"), None)
        .unwrap_err();
    assert!(
        matches!(err, arkx::core::error::ArkxError::WrongPassword),
        "got: {err}"
    );
    assert!(!out_bad.join("src/hello.txt").exists());
}
