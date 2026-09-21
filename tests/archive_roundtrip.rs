use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

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
    bm.create(&archive, &sources, 6, None, None, None).unwrap();
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
    bm.create(&archive, &sources, 6, None, None, None).unwrap();

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
    bm.create(&archive, &sources, 6, None, None, None).unwrap();

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
    bm.create(&archive, &sources, 6, None, None, None).unwrap();

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
        bm.create(&archive, &[src], 6, None, None, None).unwrap();
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
    assert!(bm.create(&archive, &[src], 6, None, None, None).is_err());
    assert!(!archive.exists());

    let a = tmp.path().join("a.txt");
    let b = tmp.path().join("b.txt");
    fs::write(&a, "a").unwrap();
    fs::write(&b, "b").unwrap();
    let archive2 = tmp.path().join("multi.gz");
    assert!(bm.create(&archive2, &[a, b], 6, None, None, None).is_err());
    assert!(!archive2.exists());
}

#[test]
fn list_shows_correct_sizes() {
    let tmp = tempfile::tempdir().unwrap();
    let (_, sources) = create_src_dir(tmp.path());
    let archive = tmp.path().join("sized.zip");

    let bm = backend();
    bm.create(&archive, &sources, 6, None, None, None).unwrap();

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
    bm.create(&archive, &sources, 6, None, None, None).unwrap();

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
    let result = bm.create(&archive, &[], 6, None, None, None);
    assert!(result.is_err());
}

#[test]
fn backend_manager_detects_and_lists() {
    let tmp = tempfile::tempdir().unwrap();
    let (_, sources) = create_src_dir(tmp.path());
    let archive = tmp.path().join("detect.zip");

    let bm = backend();
    bm.create(&archive, &sources, 6, None, None, None).unwrap();

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
    bm.create(&archive, &sources, 6, None, None, None).unwrap();

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
    bm.create(&archive, &sources, 6, None, None, None).unwrap();

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
    bm.create(&archive, &sources, 6, None, None, None).unwrap();
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
    bm.create(&archive, &sources, 6, Some("secret"), None, None)
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

#[test]
fn nested_new_folder_zip_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let (_, sources) = create_src_dir(tmp.path());
    let archive = tmp.path().join("nf.zip");

    let bm = backend();
    bm.create(&archive, &sources, 6, None, None, None).unwrap();

    // A single dialog name may now contain nested components.
    bm.new_folder(&archive, "a/b/c/", None).unwrap();

    let info = bm.detect_and_list(&archive).unwrap();
    assert!(info.entries.iter().any(|e| e.path == "a/b/c/"));

    let out = tmp.path().join("out_nf");
    bm.extract(&archive, &out, None, None, None).unwrap();
    assert!(out.join("a/b/c").is_dir());
}

#[test]
fn nested_new_folder_rejects_traversal() {
    let tmp = tempfile::tempdir().unwrap();
    let (_, sources) = create_src_dir(tmp.path());
    let archive = tmp.path().join("nf-unsafe.zip");

    let bm = backend();
    bm.create(&archive, &sources, 6, None, None, None).unwrap();

    for bad in ["a/../b", "a/..", "a//b", ".."] {
        let err = bm
            .new_folder(&archive, &format!("{bad}/"), None)
            .unwrap_err();
        assert!(
            matches!(err, arkx::core::error::ArkxError::InvalidInput(_)),
            "{bad}: got {err}"
        );
    }
}

#[test]
fn nested_new_folder_7z_roundtrip() {
    if seven_available() != Some(true) {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let (_, sources) = create_src_dir(tmp.path());
    let archive = tmp.path().join("nf.7z");

    let bm = backend();
    bm.create(&archive, &sources, 6, None, None, None).unwrap();

    bm.new_folder(&archive, "a/b/c/", None).unwrap();
    // 7z lists directories without the trailing slash (Attributes flag).
    assert!(bm
        .detect_and_list(&archive)
        .unwrap()
        .entries
        .iter()
        .any(|e| e.is_dir && e.path.trim_end_matches('/') == "a/b/c"));
}

#[test]
fn entry_hashes_zip_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let (_, sources) = create_src_dir(tmp.path());
    let archive = tmp.path().join("hash.zip");

    let bm = backend();
    bm.create(&archive, &sources, 6, None, None, None).unwrap();

    let h = bm.entry_hashes(&archive, "src/hello.txt", None).unwrap();
    assert_eq!(
        h.sha256,
        "f95e87c2239da6717a2b71cf68536cc4af7756e23d56926303e82241a2a43f3e"
    );
    assert_eq!(h.md5, "e5951fdef328cbcd64e3f31dbdbe1f5b");
    assert_eq!(
        bm.entry_hashes(&archive, "src/hello.txt", None)
            .unwrap()
            .sha256,
        h.sha256
    );
}

#[test]
fn entry_hashes_7z_roundtrip() {
    if seven_available() != Some(true) {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let (_, sources) = create_src_dir(tmp.path());
    let archive = tmp.path().join("hash.7z");

    let bm = backend();
    bm.create(&archive, &sources, 6, None, None, None).unwrap();

    let h = bm.entry_hashes(&archive, "src/hello.txt", None).unwrap();
    assert_eq!(
        h.sha256,
        "f95e87c2239da6717a2b71cf68536cc4af7756e23d56926303e82241a2a43f3e"
    );
    assert_eq!(h.md5, "e5951fdef328cbcd64e3f31dbdbe1f5b");
}

fn seven_available() -> Option<bool> {
    Some(tool_in_path(&["7z", "7zz"]))
}

fn tool_in_path(names: &[&str]) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    for dir in std::env::split_paths(&path) {
        for name in names {
            if dir.join(name).is_file() {
                return true;
            }
        }
    }
    false
}

#[test]
fn zip_volume_split_uses_info_zip_when_installed() {
    if !tool_in_path(&["zip"]) {
        eprintln!("Info-ZIP `zip` not found: skipping volume-split test");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("data.bin");
    fs::write(&src, vec![0x5A; 128 * 1024]).unwrap();
    let archive = tmp.path().join("vol.zip");

    let bm = backend();
    // 10k volumes on a 128k file force several parts.
    bm.create(&archive, &[src], 6, None, Some("10k"), None)
        .unwrap();

    // Info-ZIP naming: vol.z01, vol.z02, …, vol.zip (last part).
    let mut parts = 0;
    for n in 1..=99 {
        let part = tmp.path().join(format!("vol.z{n:02}"));
        if part.is_file() {
            parts += 1;
        } else {
            break;
        }
    }
    assert!(parts >= 1, "expected at least one .zNN volume");
    assert!(archive.is_file(), "expected the last part to be vol.zip");
}

#[test]
fn rar_volume_split_uses_rar_when_installed() {
    if !tool_in_path(&["rar"]) {
        eprintln!("`rar` not found: skipping volume-split test");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("data.bin");
    fs::write(&src, vec![0x5A; 128 * 1024]).unwrap();
    let archive = tmp.path().join("vol.rar");

    let bm = backend();
    bm.create(&archive, &[src], 6, None, Some("10k"), None)
        .unwrap();

    // rar -v naming: vol.part1.rar, vol.part2.rar, … (no base file).
    assert!(
        tmp.path().join("vol.part1.rar").is_file(),
        "expected vol.part1.rar"
    );
    assert!(!archive.exists(), "rar -v produces parts, not a base file");
}

/// Cheap deterministic pseudo-random buffer (compressible data would zip too
/// fast for the progress ticker to emit more than a couple of events).
fn pseudo_random(len: usize) -> Vec<u8> {
    let mut state: u32 = 0x9E3779B9;
    (0..len)
        .map(|_| {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            (state >> 24) as u8
        })
        .collect()
}

fn assert_equal_progress_stream(events: &[arkx::core::archive::ProgressInfo], total: u64) {
    // Progress must never regress or exceed the total...
    let mut last = 0u64;
    for e in events {
        assert!(e.current >= last, "regressed {} < {last}", e.current);
        last = e.current;
        assert!(e.percent <= 100.0 + 1e-3, "percent over 100: {}", e.percent);
        if e.total == total && total > 0 {
            assert!(e.current <= total, "over total: {} > {total}", e.current);
        }
    }
    // ...ends on the 100/100 "Completed" marker (the one allowed jump).
    let done = events.last().unwrap();
    assert_eq!(done.current, done.total, "last event not Completed");
}

#[test]
fn seven_zip_progress_stream_is_monotonic_and_completes() {
    use arkx::core::archive::ProgressInfo;

    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("huge.bin");
    fs::write(&src, pseudo_random(48 * 1024 * 1024)).unwrap();
    let archive = tmp.path().join("prog.7z");
    let extract_dir = tmp.path().join("out");

    let bm = backend();

    // 7z create: fixed-cadence ticker + Smoother, monotonic, ends 100%.
    let created: Vec<ProgressInfo> = {
        let seen: Arc<Mutex<Vec<ProgressInfo>>> = Arc::new(Mutex::new(Vec::new()));
        let emit = {
            let seen = seen.clone();
            move |info: ProgressInfo| {
                seen.lock().unwrap().push(info);
            }
        };
        bm.create(
            &archive,
            std::slice::from_ref(&src),
            6,
            None,
            None,
            Some(Box::new(emit)),
        )
        .unwrap();
        Arc::try_unwrap(seen).unwrap().into_inner().unwrap()
    };
    assert!(
        created.len() >= 2,
        "expected ≥2 events, got {}",
        created.len()
    );
    assert_equal_progress_stream(&created, 48 * 1024 * 1024);

    // 7z extract (same backend): byte-based / ramped-% ticker, same contract.
    let extracted: Vec<ProgressInfo> = {
        let seen: Arc<Mutex<Vec<ProgressInfo>>> = Arc::new(Mutex::new(Vec::new()));
        let emit = {
            let seen = seen.clone();
            move |info: ProgressInfo| {
                seen.lock().unwrap().push(info);
            }
        };
        bm.extract(&archive, &extract_dir, None, None, Some(Box::new(emit)))
            .unwrap();
        Arc::try_unwrap(seen).unwrap().into_inner().unwrap()
    };
    assert!(
        extracted.len() >= 2,
        "expected ≥2 events, got {}",
        extracted.len()
    );
    assert_equal_progress_stream(&extracted, 48 * 1024 * 1024);
}

#[test]
fn native_parallel_zip_progress_stream_is_monotonic_and_completes() {
    use arkx::core::archive::ProgressInfo;

    let tmp = tempfile::tempdir().unwrap();
    let mut sources = Vec::new();
    for i in 0..4 {
        let p = tmp.path().join(format!("blob{i}.bin"));
        fs::write(&p, pseudo_random(15 * 1024 * 1024)).unwrap();
        sources.push(p);
    }
    let archive = tmp.path().join("native_parallel.zip");

    let bm = backend();
    // 60 MiB total: below the minimum zip→7z threshold (64 MiB), so creation
    // stays on the native parallel writer and covers the read phase *and* the
    // (formerly silent) merge phase of the poller.
    let events: Vec<ProgressInfo> = {
        let seen: Arc<Mutex<Vec<ProgressInfo>>> = Arc::new(Mutex::new(Vec::new()));
        let emit = {
            let seen = seen.clone();
            move |info: ProgressInfo| {
                seen.lock().unwrap().push(info);
            }
        };
        bm.create(&archive, &sources, 6, None, None, Some(Box::new(emit)))
            .unwrap();
        Arc::try_unwrap(seen).unwrap().into_inner().unwrap()
    };
    assert!(
        events.len() >= 2,
        "expected ≥2 events, got {}",
        events.len()
    );
    assert_equal_progress_stream(&events, 60 * 1024 * 1024);
    assert!(archive.exists());
}
