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

    bm.extract(&archive, &extract_dir, None, None, None).unwrap();
    assert_eq!(fs::read(extract_dir.join("src/hello.txt")).unwrap(), b"Hello, arkx!");
    assert_eq!(fs::read(extract_dir.join("src/data.bin")).unwrap(), vec![0xAB; 4096]);
    assert_eq!(fs::read(extract_dir.join("src/nested/inner.txt")).unwrap(), b"nested content");
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

    bm.extract(&archive, &extract_dir, None, None, None).unwrap();
    assert_eq!(fs::read(extract_dir.join("src/hello.txt")).unwrap(), b"Hello, arkx!");
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

    bm.extract(&archive, &extract_dir, None, None, None).unwrap();
    assert_eq!(fs::read(extract_dir.join("src/hello.txt")).unwrap(), b"Hello, arkx!");
}

#[test]
fn tar_zst_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let (_, sources) = create_src_dir(tmp.path());
    let archive = tmp.path().join("test.tar.zst");
    let extract_dir = tmp.path().join("out_zst");

    let bm = backend();
    bm.create(&archive, &sources, 6, None, None).unwrap();

    bm.extract(&archive, &extract_dir, None, None, None).unwrap();
    assert_eq!(fs::read(extract_dir.join("src/hello.txt")).unwrap(), b"Hello, arkx!");
}

#[test]
fn list_shows_correct_sizes() {
    let tmp = tempfile::tempdir().unwrap();
    let (_, sources) = create_src_dir(tmp.path());
    let archive = tmp.path().join("sized.zip");

    let bm = backend();
    bm.create(&archive, &sources, 6, None, None).unwrap();

    let info = bm.detect_and_list(&archive).unwrap();
    let hello = info.entries.iter().find(|e| e.path.contains("hello.txt")).unwrap();
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
    bm.extract(&archive, &extract_dir, None, None, None).unwrap();
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