// Every source, SQLite database, backup and lock in these tests is temporary.
fn compress_rollout(logical: &Path) -> std::path::PathBuf {
    let compressed = logical.with_extension("jsonl.zst");
    let mut source = fs::File::open(logical).unwrap();
    let output = fs::File::create(&compressed).unwrap();
    let mut encoder = zstd::stream::write::Encoder::new(output, 1).unwrap();
    std::io::copy(&mut source, &mut encoder).unwrap();
    encoder.finish().unwrap();
    drop(source);
    fs::remove_file(logical).unwrap();
    compressed
}

fn decoded(path: &Path) -> Vec<u8> {
    let mut source = super::super::super::rollout_stream::decoded_rollout_reader(
        fs::File::open(path).unwrap(),
        path,
    )
    .unwrap();
    let mut bytes = Vec::new();
    source.read_to_end(&mut bytes).unwrap();
    bytes
}

fn indexed_compressed_fixture(
    label: &str,
) -> (
    std::path::PathBuf,
    String,
    std::path::PathBuf,
    std::path::PathBuf,
) {
    let dir = temp_codex_dir(label);
    write_config(&dir, SHARED_SESSION_PROVIDER);
    let id = "01a0e854-0000-7000-8000-000000000001".to_string();
    let logical = write_rollout(&dir, &id, "openai");
    create_thread_database_with_rollout(&dir.join("state_5.sqlite"), &id, "openai", &logical);
    let compressed = compress_rollout(&logical);
    (dir, id, logical, compressed)
}

#[test]
fn compressed_logical_reference_syncs_and_keeps_sqlite_alias_and_content() {
    let (dir, id, logical, compressed) = indexed_compressed_fixture("zst-logical-alias");
    let mut text = decoded(&compressed);
    text.extend_from_slice(b"\r\n{\"type\":\"event_msg\",\"payload\":{\"message\":\"untouched\\ntext\",\"unicode\":\"\xe4\xb8\xad\xe6\x96\x87\"}}");
    fs::write(
        &compressed,
        zstd::stream::encode_all(text.as_slice(), 1).unwrap(),
    )
    .unwrap();
    let original = fs::read(&compressed).unwrap();
    let database = dir.join("state_5.sqlite");
    Connection::open(&database)
        .unwrap()
        .execute("UPDATE threads SET title='renamed title'", [])
        .unwrap();
    let status = session_sync_status_inner(Some(dir.display().to_string()), None).unwrap();
    assert!(status.scan_complete, "{:?}", status.scan_failures);
    assert!(status.needs_sync);
    assert_eq!(status.rollout_files, 1);
    assert_eq!(status.sessions[0].title, "renamed title");
    assert_eq!(
        fs::read(&compressed).unwrap(),
        original,
        "status is read-only"
    );
    let result = sync_sessions_provider_inner(Some(dir.display().to_string()), None).unwrap();
    assert!(result.status.scan_complete);
    assert!(!result.status.needs_sync);
    assert_eq!(result.updated_rollouts, 1);
    assert_eq!(thread_provider(&database, &id), SHARED_SESSION_PROVIDER);
    let expected = String::from_utf8(text).unwrap().replace(
        "\"model_provider\":\"openai\"",
        "\"model_provider\":\"custom\"",
    );
    assert_eq!(decoded(&compressed), expected.as_bytes());
    let stored: String = Connection::open(&database)
        .unwrap()
        .query_row(
            "SELECT rollout_path FROM threads WHERE id=?1",
            [&id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stored, logical.display().to_string());
    assert!(
        !logical.exists(),
        "sync must not expand or rename logical storage"
    );
    let next = fs::read(&compressed).unwrap();
    let again = sync_sessions_provider_inner(Some(dir.display().to_string()), None).unwrap();
    assert_eq!(again.updated_rollouts, 0);
    assert_eq!(again.updated_threads, 0);
    assert!(again.backup_dir.is_empty());
    assert_eq!(
        fs::read(&compressed).unwrap(),
        next,
        "matched compressed bytes stay identical"
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn plain_rollout_wins_over_stale_compressed_sibling() {
    let (dir, id, logical, compressed) = indexed_compressed_fixture("zst-plain-priority");
    let stale = fs::read(&compressed).unwrap();
    write_rollout_at(&logical, &id, "openai");
    let result = sync_sessions_provider_inner(Some(dir.display().to_string()), None).unwrap();
    assert!(
        result.status.scan_complete,
        "{:?}",
        result.status.scan_failures
    );
    assert_eq!(result.updated_rollouts, 1);
    assert_eq!(result.status.rollout_files, 1);
    assert_eq!(fs::read(&compressed).unwrap(), stale);
    assert!(fs::read_to_string(&logical)
        .unwrap()
        .contains("\"model_provider\":\"custom\""));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn compressed_archive_and_internal_threads_are_not_promoted_or_modified() {
    let (dir, id, logical, compressed) = indexed_compressed_fixture("zst-hidden-threads");
    let database = dir.join("state_5.sqlite");
    let archived_id = "01a0e854-0000-7000-8000-000000000002";
    let archived_logical = dir.join(format!(
        "archived_sessions/rollout-test-{archived_id}.jsonl"
    ));
    write_rollout_at(&archived_logical, archived_id, "openai");
    let archived = compress_rollout(&archived_logical);
    let internal_id = "01a0e854-0000-7000-8000-000000000003";
    let internal_logical = write_rollout(&dir, internal_id, "openai");
    fs::write(&internal_logical, format!("{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{internal_id}\",\"source\":{{\"internal\":\"guardian\"}},\"model_provider\":\"openai\"}}}}\n")).unwrap();
    let internal = compress_rollout(&internal_logical);
    let archive_bytes = fs::read(&archived).unwrap();
    let internal_bytes = fs::read(&internal).unwrap();
    let conn = Connection::open(&database).unwrap();
    for (thread, path, archived) in [
        (archived_id, &archived_logical, 1),
        (internal_id, &internal_logical, 0),
    ] {
        conn.execute("INSERT INTO threads(id,model_provider,rollout_path,archived) VALUES(?1,'openai',?2,?3)", (thread, path.display().to_string(), archived)).unwrap();
    }
    drop(conn);
    let catalog = dir.join("sqlite/codex-dev.db");
    create_catalog_database(&catalog, &[]);
    let result = sync_sessions_provider_inner(Some(dir.display().to_string()), None).unwrap();
    assert!(
        result.status.scan_complete,
        "{:?}",
        result.status.scan_failures
    );
    assert!(!result.status.needs_sync);
    assert_eq!(result.updated_rollouts, 1);
    assert_eq!(thread_provider(&database, &id), SHARED_SESSION_PROVIDER);
    assert_eq!(thread_provider(&database, internal_id), "openai");
    assert_eq!(thread_provider(&database, archived_id), "openai");
    assert_eq!(fs::read(&archived).unwrap(), archive_bytes);
    assert_eq!(fs::read(&internal).unwrap(), internal_bytes);
    assert!(catalog_provider_and_visibility(&catalog, internal_id).is_none());
    assert!(catalog_provider_and_visibility(&catalog, archived_id).is_none());
    assert_eq!(
        result
            .status
            .sessions
            .iter()
            .find(|row| row.id == internal_id)
            .unwrap()
            .is_subagent,
        true
    );
    assert!(!logical.exists());
    assert!(compressed.exists());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn compressed_revert_uses_thread_id_and_current_alias_for_internal_classification() {
    let dir = temp_codex_dir("zst-revert-identity");
    write_config(&dir, SHARED_SESSION_PROVIDER);
    let thread = "01a0e854-0000-7000-8000-000000000004";
    let rollout = "01a0e854-0000-7000-8000-000000000005";
    let old = write_rollout(&dir, thread, "openai");
    fs::write(&old, format!("{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{thread}\",\"source\":\"subagent\",\"model_provider\":\"openai\"}}}}\n")).unwrap();
    let old = compress_rollout(&old);
    let old_bytes = fs::read(&old).unwrap();
    let current = dir.join(format!(
        "sessions/2026/10/01/rollout-2026-10-01-{thread}_{rollout}.jsonl"
    ));
    write_rollout_at(&current, thread, "openai");
    let compressed = compress_rollout(&current);
    let database = dir.join("state_5.sqlite");
    create_thread_database_with_rollout(&database, thread, "openai", &current);
    let catalog = dir.join("sqlite/codex-dev.db");
    create_catalog_database(&catalog, &[]);
    let result = sync_sessions_provider_inner(Some(dir.display().to_string()), None).unwrap();
    assert!(
        result.status.scan_complete,
        "{:?}",
        result.status.scan_failures
    );
    assert_eq!(result.updated_rollouts, 1);
    assert_eq!(thread_provider(&database, thread), SHARED_SESSION_PROVIDER);
    assert!(catalog_provider_and_visibility(&catalog, rollout).is_none());
    assert_eq!(fs::read(&old).unwrap(), old_bytes);
    assert!(String::from_utf8(decoded(&compressed))
        .unwrap()
        .contains("\"model_provider\":\"custom\""));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn corrupted_or_truncated_compressed_reference_never_reports_synced() {
    for invalid in [b"not zstandard".to_vec(), {
        let mut compressed = zstd::stream::encode_all(&b"{\"type\":\"session_meta\",\"payload\":{\"id\":\"t\",\"model_provider\":\"custom\"}}\n"[..], 1).unwrap();
        compressed.truncate(compressed.len() - 4);
        compressed
    }] {
        let (dir, id, _logical, compressed) = indexed_compressed_fixture("zst-corrupt");
        fs::write(&compressed, &invalid).unwrap();
        let status = session_sync_status_inner(Some(dir.display().to_string()), None).unwrap();
        assert!(!status.scan_complete);
        assert!(!status.scan_failures.is_empty());
        sync_sessions_provider_inner(Some(dir.display().to_string()), None)
            .expect_err("invalid compressed data blocks every mutation");
        assert_eq!(thread_provider(&dir.join("state_5.sqlite"), &id), "openai");
        assert_eq!(fs::read(&compressed).unwrap(), invalid);
        fs::remove_dir_all(dir).unwrap();
    }
}

#[test]
fn compressed_transaction_rollback_restores_exact_original_compressed_bytes() {
    let (dir, id, _logical, compressed) = indexed_compressed_fixture("zst-rollback");
    let original = fs::read(&compressed).unwrap();
    let error = sync_sessions_provider_with_hook(Some(dir.display().to_string()), None, |point| {
        if point == MutationPoint::AfterSqliteCommit(0) {
            assert_ne!(fs::read(&compressed).unwrap(), original);
            return Err(CodexxError::Config("fixture rollback".into()));
        }
        Ok(())
    })
    .unwrap_err();
    assert!(error.to_string().contains("fixture rollback"));
    assert_eq!(fs::read(&compressed).unwrap(), original);
    assert_eq!(thread_provider(&dir.join("state_5.sqlite"), &id), "openai");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn compressed_source_replacement_or_expansion_during_sync_aborts_without_overwrite() {
    for expansion in [false, true] {
        for point_to_change in [
            MutationPoint::BeforeRolloutMutation,
            MutationPoint::BeforeEachRolloutMutation(0),
        ] {
            let (dir, id, logical, compressed) =
                indexed_compressed_fixture("zst-concurrent-representation");
            let original = fs::read(&compressed).unwrap();
            let external = format!("{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\",\"model_provider\":\"external\"}}}}\n").into_bytes();
            let external_compressed = zstd::stream::encode_all(external.as_slice(), 1).unwrap();
            sync_sessions_provider_with_hook(Some(dir.display().to_string()), None, |point| {
                if point == point_to_change {
                    if expansion {
                        fs::write(&logical, &external).unwrap();
                    } else {
                        fs::write(&compressed, &external_compressed).unwrap();
                    }
                }
                Ok(())
            })
            .expect_err("external replacement/expansion must abort synchronization");
            assert_eq!(thread_provider(&dir.join("state_5.sqlite"), &id), "openai");
            if expansion {
                assert_eq!(fs::read(&logical).unwrap(), external);
                assert_eq!(fs::read(&compressed).unwrap(), original);
            } else {
                assert_eq!(fs::read(&compressed).unwrap(), external_compressed);
            }
            fs::remove_dir_all(dir).unwrap();
        }
    }
}

#[test]
fn compressed_reference_with_wrong_metadata_id_blocks_sync_without_uuid_reassociation() {
    let (dir, id, logical, compressed) = indexed_compressed_fixture("zst-wrong-identity");
    let other = "01a0e854-0000-7000-8000-000000000009";
    let wrong = format!("{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{other}\",\"model_provider\":\"openai\"}}}}\n");
    let bytes = zstd::stream::encode_all(wrong.as_bytes(), 1).unwrap();
    fs::write(&compressed, &bytes).unwrap();
    // A different file with the right ID must not replace an explicit current alias.
    let fallback = dir.join(format!("sessions/history/rollout-old-{id}.jsonl"));
    write_rollout_at(&fallback, &id, "openai");
    let status = session_sync_status_inner(Some(dir.display().to_string()), None).unwrap();
    assert!(!status.scan_complete);
    assert!(status
        .scan_failures
        .iter()
        .any(|failure| failure.contains("线程 ID 不一致")));
    sync_sessions_provider_inner(Some(dir.display().to_string()), None).unwrap_err();
    assert_eq!(fs::read(&compressed).unwrap(), bytes);
    assert_eq!(thread_provider(&dir.join("state_5.sqlite"), &id), "openai");
    assert!(!logical.exists());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn active_reference_moved_to_compressed_archive_is_reported_as_archive_conflict() {
    let (dir, id, logical, compressed) = indexed_compressed_fixture("zst-archive-conflict");
    let archived = dir
        .join("archived_sessions")
        .join(compressed.file_name().unwrap());
    fs::create_dir_all(archived.parent().unwrap()).unwrap();
    fs::rename(&compressed, &archived).unwrap();
    let bytes = fs::read(&archived).unwrap();
    let status = session_sync_status_inner(Some(dir.display().to_string()), None).unwrap();
    assert!(!status.scan_complete);
    assert!(status
        .scan_failures
        .iter()
        .any(|failure| failure.contains("归档状态不一致")
            && failure.contains(&id)
            && failure.contains(&logical.display().to_string())
            && failure.contains(&archived.display().to_string())));
    assert!(!status
        .scan_failures
        .iter()
        .any(|failure| failure.contains("均缺失")));
    sync_sessions_provider_inner(Some(dir.display().to_string()), None).unwrap_err();
    assert_eq!(fs::read(&archived).unwrap(), bytes);
    assert_eq!(thread_provider(&dir.join("state_5.sqlite"), &id), "openai");
    assert!(!logical.exists());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn compressed_expansion_after_commit_preserves_external_plain_log_and_retains_backup() {
    let (dir, id, logical, compressed) = indexed_compressed_fixture("zst-expansion-after-commit");
    let external = format!("{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\",\"model_provider\":\"external\"}}}}\n");
    let error = sync_sessions_provider_with_hook(Some(dir.display().to_string()), None, |point| {
        if point == MutationPoint::AfterSqliteCommit(0) {
            fs::write(&logical, external.as_bytes()).unwrap();
        }
        Ok(())
    })
    .expect_err("postcommit expansion must not be mistaken for a successful synchronization");
    assert!(error.to_string().contains("变化") || error.to_string().contains("回滚"));
    assert_eq!(fs::read(&logical).unwrap(), external.as_bytes());
    assert_eq!(thread_provider(&dir.join("state_5.sqlite"), &id), "openai");
    assert!(compressed.exists());
    let backups = crate::sessions::provider_sync_backup_root(&dir);
    assert!(fs::read_dir(backups).unwrap().next().is_some());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn provider_can_really_switch_during_sync_while_session_target_stays_fixed() {
    let _app_db_guard = crate::app_db::test_db_guard();
    let dir = temp_codex_dir("provider-switch-during-session-sync");
    write_config(&dir, "openai");
    let id = "01a0e854-0000-7000-8000-000000000011";
    let logical = write_rollout(&dir, id, "historical-provider");
    let compressed = compress_rollout(&logical);
    let database = dir.join("state_5.sqlite");
    create_thread_database_with_rollout(&database, id, "historical-provider", &logical);
    let mut switched = false;
    let result = sync_sessions_provider_with_hook(Some(dir.display().to_string()), None, |point| {
        if point == MutationPoint::BeforeSqliteLock {
            crate::providers::switch_provider_inner(crate::providers::ProviderInput {
                config_dir: Some(dir.display().to_string()),
                provider_id: Some("session-sync-switch-fixture".into()),
                provider_name: "Session sync switch fixture".into(),
                base_url: "https://session-sync-fixture.invalid/v1".into(),
                model: "fixture-model".into(),
                api_key: Some("fixture-key-never-sent".into()),
                wire_api: Some("responses".into()),
                requires_openai_auth: None,
            })?;
            assert_eq!(current_model_provider(&dir, None)?, "custom");
            switched = true;
        }
        Ok(())
    })
    .unwrap();
    assert!(
        switched,
        "the real provider switch ran while the maintenance lock was held"
    );
    assert_eq!(result.status.target_provider, "openai");
    assert!(!result.status.needs_sync);
    assert_eq!(thread_provider(&database, id), "openai");
    assert!(String::from_utf8(decoded(&compressed))
        .unwrap()
        .contains("\"model_provider\":\"openai\""));
    assert_eq!(current_model_provider(&dir, None).unwrap(), "custom");
    let current = session_sync_status_inner(Some(dir.display().to_string()), None).unwrap();
    assert_eq!(current.target_provider, "custom");
    assert!(
        current.needs_sync,
        "a new explicit status read sees the new target, without stale scan caching"
    );
    let live_config = fs::read_to_string(dir.join("config.toml")).unwrap();
    assert!(live_config.contains("https://session-sync-fixture.invalid/v1"));
    fs::remove_dir_all(dir).unwrap();
}
