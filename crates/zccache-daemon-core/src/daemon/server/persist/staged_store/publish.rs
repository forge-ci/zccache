//! Staged generation construction and atomic pointer publication.

use super::*;

pub(super) fn persist_staged_generation(
    artifact_dir: &Path,
    key_hex: &str,
    sources: &[StagedSource<'_>],
    mode: MaterializationMode,
) -> io::Result<PersistArtifactFileStats> {
    let publish_started = std::time::Instant::now();
    validate_key(key_hex)?;
    if sources.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "cannot publish an empty staged artifact",
        ));
    }

    let root = staged_root(artifact_dir);
    let _store_lock_guard = StagedReadGuard::acquire_for(artifact_dir, crate::core::wait::PROBE)
        .map_err(|error| publish_error(StagedPublishFailure::StoreSetup, error))?;
    #[cfg(test)]
    hook::pause(artifact_dir, StagedHookPoint::PublicationStoreLocked);
    let key_root = root.join(key_hex);
    fs::create_dir_all(&key_root)
        .map_err(|error| publish_error(StagedPublishFailure::StoreSetup, error))?;
    let publish_lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(key_root.join(PUBLISH_LOCK))
        .map_err(|error| publish_error(StagedPublishFailure::StoreSetup, error))?;
    let _publish_lock_guard =
        crate::core::wait::lock_exclusive_for(&publish_lock, crate::core::wait::PROBE)
            .map_err(|error| publish_error(StagedPublishFailure::StoreSetup, error))?;
    let temporary_generation = key_root.join(format!(
        ".tmp-{}-{}",
        std::process::id(),
        STAGED_ARTIFACT_TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    #[cfg(test)]
    fault::inject(artifact_dir, StagedFaultPoint::GenerationCreate)
        .map_err(|error| publish_error(StagedPublishFailure::StoreSetup, error))?;
    fs::create_dir(&temporary_generation)
        .map_err(|error| publish_error(StagedPublishFailure::StoreSetup, error))?;

    let mut retired_generation = None;
    let result = (|| {
        let mut outputs = Vec::with_capacity(sources.len());
        let mut stats = PersistArtifactFileStats {
            staged: true,
            ..PersistArtifactFileStats::default()
        };
        for (index, source) in sources.iter().enumerate() {
            let destination = output_path(&temporary_generation, index);
            #[cfg(test)]
            fault::inject(artifact_dir, StagedFaultPoint::OutputCopy(index))
                .map_err(|error| publish_error(StagedPublishFailure::OutputCopy, error))?;
            let (reflink, copied_bytes) = source.write_to(&destination, mode).map_err(|error| {
                publish_error(
                    StagedPublishFailure::OutputCopy,
                    io::Error::new(
                        error.kind(),
                        format!(
                            "staged output copy failed: {} -> {}: {error}",
                            source.describe(),
                            destination.display()
                        ),
                    ),
                )
            })?;
            #[cfg(test)]
            fault::inject(artifact_dir, StagedFaultPoint::OutputSync(index))
                .map_err(|error| publish_error(StagedPublishFailure::OutputDurability, error))?;
            sync_file(&destination).map_err(|error| {
                publish_error(
                    StagedPublishFailure::OutputDurability,
                    io::Error::new(
                        error.kind(),
                        format!(
                            "staged output durability sync failed: {}: {error}",
                            destination.display()
                        ),
                    ),
                )
            })?;
            crate::platform::fs::permissions::seal_cache_blob(&destination).map_err(|error| {
                publish_error(
                    StagedPublishFailure::OutputCopy,
                    io::Error::new(
                        error.kind(),
                        format!(
                            "staged output read-only finalization failed: {}: {error}",
                            destination.display()
                        ),
                    ),
                )
            })?;
            let hash_started = std::time::Instant::now();
            #[cfg(test)]
            fault::inject(artifact_dir, StagedFaultPoint::OutputHash(index))
                .map_err(|error| publish_error(StagedPublishFailure::Hash, error))?;
            let (size, digest_hex, digest_bytes) = digest_file(&destination).map_err(|error| {
                publish_error(
                    StagedPublishFailure::Hash,
                    io::Error::new(
                        error.kind(),
                        format!(
                            "staged output hash failed: {}: {error}",
                            destination.display()
                        ),
                    ),
                )
            })?;
            stats.staged_hash_ns = stats
                .staged_hash_ns
                .saturating_add(hash_started.elapsed().as_nanos() as u64);
            #[cfg(test)]
            fault::inject(artifact_dir, StagedFaultPoint::DurableDigest(index))
                .map_err(|error| publish_error(StagedPublishFailure::DurableDigest, error))?;
            // #1774: reuse the digest just computed above instead of having
            // the sidecar writer hash `destination` a second time.
            write_authoritative_blob_digest_for_precomputed(digest_bytes, &destination)
                .map(|digest| digest.commit())
                .map_err(|error| {
                    publish_error(
                        StagedPublishFailure::DurableDigest,
                        io::Error::new(
                            error.kind(),
                            format!(
                                "staged output durable digest failed: {}: {error}",
                                destination.display()
                            ),
                        ),
                    )
                })?;
            outputs.push(StagedOutput {
                index,
                size,
                digest_hex,
            });
            if reflink {
                stats.reflink_count += 1;
            } else {
                stats.copy_count += 1;
                stats.copy_bytes += copied_bytes;
            }
        }

        let generation_hex = generation_digest(key_hex, &outputs);
        validate_generation(&generation_hex)?;
        let pointer = pointer_path(artifact_dir, key_hex);
        if let Ok(previous_value) = fs::read_to_string(&pointer) {
            let previous = previous_value.trim();
            if !previous.is_empty() && previous != generation_hex {
                if validate_staged_generation(artifact_dir, key_hex, previous).is_ok() {
                    // #1244: the key has now been *proven* not to determine
                    // the bytes — two complete, internally valid generations
                    // disagree. Preserving either one means serving an answer
                    // we know might be wrong, which is a silent miscompile.
                    //
                    // Evict instead, so the next lookup is an honest miss.
                    // This is the "failure mode is a miss, never a wrong
                    // answer" invariant stated for the pending-write path.
                    //
                    // Retire the pointer now, so no new hit can discover the
                    // conflicting key. Existing readers hold shared store
                    // leases and still need the old path. Orphan detachment
                    // waits for exclusive store access before moving it.
                    retired_generation = Some(previous.to_string());
                    let mut evicted = true;
                    if let Err(error) = trash::defer_staged_tree(artifact_dir, &pointer) {
                        evicted = false;
                        tracing::warn!(
                            path = %pointer.display(),
                            %error,
                            "failed to remove staged pointer for conflicting key"
                        );
                    }
                    write_staged_lifecycle_event(
                        artifact_dir,
                        crate::core::lifecycle::EVENT_STAGED_PUBLICATION_CONFLICT,
                        serde_json::json!({
                            "cache_key": key_hex,
                            "existing_generation": previous,
                            "candidate_generation": generation_hex,
                            "evicted": evicted,
                            "elapsed_ns": publish_started.elapsed().as_nanos() as u64,
                        }),
                    );
                    tracing::error!(
                        event = "staged_publication_conflict",
                        cache_key = key_hex,
                        existing_generation = previous,
                        candidate_generation = generation_hex,
                        evicted,
                        "same cache key produced a different complete output generation; evicting the key"
                    );
                    return Err(publish_error(
                        StagedPublishFailure::Conflict,
                        io::Error::new(
                            io::ErrorKind::AlreadyExists,
                            "same cache key produced different staged output bytes",
                        ),
                    ));
                }
                if validate_generation(previous).is_ok() {
                    retired_generation = Some(previous.to_string());
                }
                write_staged_lifecycle_event(
                    artifact_dir,
                    crate::core::lifecycle::EVENT_STAGED_PUBLICATION_REPLACES_INVALID_GENERATION,
                    serde_json::json!({
                        "cache_key": key_hex,
                        "invalid_generation": previous,
                        "replacement_generation": generation_hex,
                    }),
                );
            }
        }
        let output_count = outputs.len();
        let final_generation = generation_dir(artifact_dir, key_hex, &generation_hex);
        let manifest = StagedManifest {
            version: STAGED_MANIFEST_VERSION,
            key_hex: key_hex.to_string(),
            generation_hex: generation_hex.clone(),
            outputs,
        };
        let manifest_bytes = bincode::serialize(&manifest).map_err(|error| {
            publish_error(
                StagedPublishFailure::Manifest,
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("staged manifest encode failed: {error}"),
                ),
            )
        })?;
        let temporary_manifest = manifest_path(&temporary_generation);
        #[cfg(test)]
        fault::inject(artifact_dir, StagedFaultPoint::ManifestWrite)
            .map_err(|error| publish_error(StagedPublishFailure::Manifest, error))?;
        fs::write(&temporary_manifest, manifest_bytes).map_err(|error| {
            publish_error(
                StagedPublishFailure::Manifest,
                io::Error::new(
                    error.kind(),
                    format!("staged manifest write failed: {error}"),
                ),
            )
        })?;
        #[cfg(test)]
        fault::inject(artifact_dir, StagedFaultPoint::ManifestSync)
            .map_err(|error| publish_error(StagedPublishFailure::Manifest, error))?;
        sync_file(&temporary_manifest).map_err(|error| {
            publish_error(
                StagedPublishFailure::Manifest,
                io::Error::new(
                    error.kind(),
                    format!("staged manifest sync failed: {error}"),
                ),
            )
        })?;
        #[cfg(test)]
        fault::inject(artifact_dir, StagedFaultPoint::GenerationSync)
            .map_err(|error| publish_error(StagedPublishFailure::GenerationPublish, error))?;
        sync_directory(&temporary_generation).map_err(|error| {
            publish_error(
                StagedPublishFailure::GenerationPublish,
                io::Error::new(
                    error.kind(),
                    format!("staged generation sync failed: {error}"),
                ),
            )
        })?;

        #[cfg(test)]
        fault::inject(artifact_dir, StagedFaultPoint::GenerationPublish)
            .map_err(|error| publish_error(StagedPublishFailure::GenerationPublish, error))?;
        let mut generation_renamed = false;
        match rename_staged_generation(&temporary_generation, &final_generation) {
            Ok(()) => generation_renamed = true,
            Err(error) if final_generation.exists() => {
                let _ = error;
                let _ = trash::defer_staged_tree(artifact_dir, &temporary_generation);
            }
            Err(error) => {
                return Err(publish_error(
                    StagedPublishFailure::GenerationPublish,
                    error,
                ));
            }
        }
        sync_directory(&key_root).map_err(|error| {
            publish_error(
                StagedPublishFailure::GenerationPublish,
                io::Error::new(
                    error.kind(),
                    format!("staged generation parent sync failed: {error}"),
                ),
            )
        })?;

        #[cfg(test)]
        if let Err(error) = fault::inject(artifact_dir, StagedFaultPoint::PointerCommit) {
            if generation_renamed {
                remove_uncommitted_generation(
                    artifact_dir,
                    &pointer,
                    &generation_hex,
                    &final_generation,
                );
            }
            return Err(publish_error(StagedPublishFailure::PointerCommit, error));
        }
        atomic_write(&pointer, generation_hex.as_bytes()).map_err(|error| {
            if generation_renamed {
                remove_uncommitted_generation(
                    artifact_dir,
                    &pointer,
                    &generation_hex,
                    &final_generation,
                );
            }
            publish_error(
                StagedPublishFailure::PointerCommit,
                io::Error::new(
                    error.kind(),
                    format!("staged generation pointer publish failed: {error}"),
                ),
            )
        })?;
        if let Some(parent) = pointer.parent() {
            #[cfg(test)]
            if let Err(error) = fault::inject(artifact_dir, StagedFaultPoint::PointerSync) {
                if generation_renamed {
                    remove_uncommitted_generation(
                        artifact_dir,
                        &pointer,
                        &generation_hex,
                        &final_generation,
                    );
                }
                return Err(publish_error(StagedPublishFailure::PointerCommit, error));
            }
            sync_directory(parent).map_err(|error| {
                if generation_renamed {
                    remove_uncommitted_generation(
                        artifact_dir,
                        &pointer,
                        &generation_hex,
                        &final_generation,
                    );
                }
                publish_error(
                    StagedPublishFailure::PointerCommit,
                    io::Error::new(
                        error.kind(),
                        format!("staged pointer parent sync failed: {error}"),
                    ),
                )
            })?;
        }
        // Replaced generations are unreferenced, but retained reader leases
        // may still name them. The exclusive orphan collector retires them.
        let total_ns = publish_started.elapsed().as_nanos() as u64;
        stats.staged_publication_ns = exclusive_publication_ns(total_ns, stats.staged_hash_ns);
        tracing::info!(
            event = "staged_publication_complete",
            cache_key = key_hex,
            generation = generation_hex,
            output_count,
            reflink_count = stats.reflink_count,
            copy_count = stats.copy_count,
            copy_bytes = stats.copy_bytes,
            elapsed_ns = total_ns,
            digest_ns = stats.staged_hash_ns,
            publication_ns = stats.staged_publication_ns,
            "published immutable staged artifact generation"
        );
        Ok(stats)
    })();

    if result.is_err() {
        let _ = trash::defer_staged_tree(artifact_dir, &temporary_generation);
    }
    drop(_publish_lock_guard);
    drop(_store_lock_guard);
    if let Some(generation) = retired_generation {
        match maintenance::detach_generation_if_unreferenced_for(
            artifact_dir,
            key_hex,
            &generation,
            std::time::Duration::ZERO,
        ) {
            Ok(detached) => detached.defer(),
            Err(error) if error.kind() == io::ErrorKind::TimedOut => {
                // A live hit still owns the old name. Periodic orphan
                // collection retries after its shared lease ends.
            }
            Err(error) => tracing::warn!(%error, "retired generation detachment deferred"),
        }
    }
    result
}
