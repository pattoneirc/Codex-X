// Manual release-profile benchmark. It never accepts a user-supplied Codex
// directory; all databases, config, histories and backups belong to TempDir.
use super::super::super::rollout_stream::decoded_rollout_reader;
use std::io::BufReader;
use std::time::Instant;

fn random_ascii(buffer: &mut [u8], seed: &mut u64) {
    const ASCII: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_-";
    for byte in buffer {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        *byte = ASCII[(*seed >> 8) as usize & 63];
    }
}

fn event_payload(writer: &mut impl Write, size: usize, seed: &mut u64) -> u64 {
    let prefix = b"{\"type\":\"event_msg\",\"payload\":{\"type\":\"agent_message\",\"message\":\"";
    let suffix = b"\"}}\n";
    writer.write_all(prefix).unwrap();
    let mut buffer = [0; 64 * 1024];
    let mut remaining = size;
    while remaining != 0 {
        let count = remaining.min(buffer.len());
        random_ascii(&mut buffer[..count], seed);
        writer.write_all(&buffer[..count]).unwrap();
        remaining -= count;
    }
    writer.write_all(suffix).unwrap();
    (prefix.len() + size + suffix.len()) as u64
}

fn content_hash(path: &Path) -> [u8; 32] {
    let file = fs::File::open(path).unwrap();
    let mut reader = BufReader::new(decoded_rollout_reader(file, path).unwrap());
    let mut first = Vec::new();
    reader.read_until(b'\n', &mut first).unwrap();
    assert!(first.len() < 4096);
    let first: serde_json::Value = serde_json::from_slice(&first).unwrap();
    assert_eq!(first["type"], "session_meta");
    let mut hash = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = reader.read(&mut buffer).unwrap();
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    hash.finalize().into()
}

fn timed<T>(action: impl FnOnce() -> T) -> (T, f64) {
    let started = Instant::now();
    let value = action();
    (value, started.elapsed().as_secs_f64() * 1000.0)
}

#[test]
#[ignore = "manual release-profile measurement using temporary session fixtures only"]
fn measure_session_sync_on_temporary_plain_and_compressed_logs() {
    const FILES: usize = 24;
    const LARGE_RECORD_BYTES: usize = 34 * 1024 * 1024;
    const SMALL_RECORDS_PER_FILE: usize = 1000;
    const SMALL_RECORD_BYTES: usize = 512;
    let directory = tempfile::tempdir().unwrap();
    let dir = directory.path();
    write_config(dir, SHARED_SESSION_PROVIDER);
    let database = dir.join("state_5.sqlite");
    let conn = Connection::open(&database).unwrap();
    conn.execute_batch("CREATE TABLE threads(id TEXT PRIMARY KEY, model_provider TEXT NOT NULL, title TEXT, rollout_path TEXT, archived INTEGER NOT NULL DEFAULT 0)").unwrap();
    let mut raw_bytes = 0u64;
    let mut raw_compressed_bytes = 0u64;
    let mut plain_bytes = 0u64;
    let mut compressed_bytes = 0u64;
    let mut record_count = 0usize;
    let mut fixtures = Vec::new();
    let mut seed = 0x1357_2468_90ab_cdef;
    for index in 0..FILES {
        let id = format!("01a0e854-0000-7000-8000-{index:012}");
        let logical = dir.join(format!("sessions/2026/10/16/rollout-benchmark-{id}.jsonl"));
        fs::create_dir_all(logical.parent().unwrap()).unwrap();
        let compressed = index % 2 == 1;
        let path = if compressed {
            logical.with_extension("jsonl.zst")
        } else {
            logical.clone()
        };
        let mut file = fs::File::create(&path).unwrap();
        let mut encoder = if compressed {
            Some(zstd::stream::write::Encoder::new(file.try_clone().unwrap(), 1).unwrap())
        } else {
            None
        };
        let mut output = std::io::BufWriter::new(if let Some(encoder) = encoder.as_mut() {
            encoder as &mut dyn Write
        } else {
            &mut file as &mut dyn Write
        });
        let meta = format!("{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\",\"source\":\"cli\",\"model_provider\":\"openai\"}}}}\n");
        output.write_all(meta.as_bytes()).unwrap();
        let mut bytes = meta.len() as u64;
        record_count += 1;
        if index < 2 {
            bytes += event_payload(&mut output, LARGE_RECORD_BYTES, &mut seed);
            record_count += 1;
        } else {
            for _ in 0..SMALL_RECORDS_PER_FILE {
                bytes += event_payload(&mut output, SMALL_RECORD_BYTES, &mut seed);
                record_count += 1;
            }
        }
        output.flush().unwrap();
        drop(output);
        if let Some(encoder) = encoder {
            encoder.finish().unwrap();
        }
        drop(file);
        let stored_bytes = fs::metadata(&path).unwrap().len();
        raw_bytes += bytes;
        if compressed {
            compressed_bytes += stored_bytes;
            raw_compressed_bytes += bytes;
        } else {
            plain_bytes += stored_bytes;
        }
        conn.execute(
            "INSERT INTO threads(id,model_provider,title,rollout_path) VALUES(?1,'openai',?1,?2)",
            (&id, logical.display().to_string()),
        )
        .unwrap();
        let hash = content_hash(&path);
        fixtures.push((id, logical, path, hash));
    }
    drop(conn);
    let config_dir = Some(dir.display().to_string());
    let (before, status_before_ms) =
        timed(|| session_sync_status_inner(config_dir.clone(), None).unwrap());
    assert!(before.scan_complete, "{:?}", before.scan_failures);
    assert!(before.needs_sync);
    assert_eq!(before.rollout_files, FILES);
    assert_eq!(before.mismatched_sessions, FILES);
    let (result, sync_ms) =
        timed(|| sync_sessions_provider_inner(config_dir.clone(), None).unwrap());
    assert!(
        result.status.scan_complete,
        "{:?}",
        result.status.scan_failures
    );
    assert!(!result.status.needs_sync);
    assert_eq!(result.updated_rollouts, FILES);
    assert_eq!(result.updated_threads, FILES);
    let (after, status_after_ms) =
        timed(|| session_sync_status_inner(config_dir.clone(), None).unwrap());
    assert!(after.scan_complete);
    assert!(!after.needs_sync);
    let raw_hashes = fixtures
        .iter()
        .map(|(_, _, path, _)| rollout_digest(path))
        .collect::<Vec<_>>();
    let (repeat, idempotent_sync_ms) =
        timed(|| sync_sessions_provider_inner(config_dir, None).unwrap());
    assert_eq!(repeat.updated_rollouts, 0);
    assert_eq!(repeat.updated_threads, 0);
    assert!(repeat.backup_dir.is_empty());
    assert!(repeat.status.scan_complete);
    assert!(!repeat.status.needs_sync);
    let conn = Connection::open(&database).unwrap();
    for ((id, logical, path, expected_content_hash), raw_hash) in fixtures.iter().zip(raw_hashes) {
        assert_eq!(content_hash(path), *expected_content_hash);
        assert_eq!(
            rollout_digest(path),
            raw_hash,
            "idempotent sync must not recompress matching storage"
        );
        let (provider, reference): (String, String) = conn
            .query_row(
                "SELECT model_provider,rollout_path FROM threads WHERE id=?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(provider, SHARED_SESSION_PROVIDER);
        assert_eq!(reference, logical.display().to_string());
    }
    println!(
        "SESSION_SYNC_BENCHMARK {}",
        serde_json::json!({
            "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
            "fixture_only": true, "files": FILES, "plain_files": FILES / 2,
            "compressed_files": FILES / 2, "jsonl_records": record_count,
            "largest_record_payload_bytes": LARGE_RECORD_BYTES,
            "decoded_jsonl_bytes": raw_bytes, "plain_stored_bytes": plain_bytes,
            "compressed_decoded_bytes": raw_compressed_bytes, "compressed_stored_bytes": compressed_bytes,
            "total_stored_bytes": plain_bytes + compressed_bytes,
            "status_before_ms": status_before_ms, "sync_ms": sync_ms,
            "status_after_ms": status_after_ms, "idempotent_sync_ms": idempotent_sync_ms,
            "updated_rollouts": result.updated_rollouts, "updated_threads": result.updated_threads,
            "all_content_hashes_preserved": true, "logical_references_unchanged": true,
            "cache_context": "warm local filesystem after fixture generation; no cache flush",
            "comparison": "current implementation only; previous implementation did not support compressed rollouts"
        })
    );
}
