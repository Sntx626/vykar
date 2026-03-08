use std::collections::HashMap;

use proptest::prelude::*;

use crate::index::{ChunkIndex, ChunkIndexEntry, IndexBlob};
use crate::repo::format::{
    pack_object_streaming_with_context, unpack_object_expect_with_context_into, ObjectType,
};
use crate::snapshot::item::{ChunkRef, Item, ItemType};
use vykar_crypto::aes_gcm::Aes256GcmEngine;
use vykar_crypto::chacha20_poly1305::ChaCha20Poly1305Engine;
use vykar_types::chunk_id::ChunkId;
use vykar_types::pack_id::PackId;

// ---------------------------------------------------------------------------
// Step 3: Format-layer encryption round-trip
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]
    #[test]
    fn format_layer_roundtrip_aes(
        plaintext in prop::collection::vec(any::<u8>(), 0..65536),
        enc_key in prop::array::uniform32(any::<u8>()),
        cid_key in prop::array::uniform32(any::<u8>()),
        context in prop::collection::vec(any::<u8>(), 0..64),
    ) {
        let engine = Aes256GcmEngine::new(&enc_key, &cid_key);
        let packed = pack_object_streaming_with_context(
            ObjectType::ChunkData, &context, plaintext.len(), &engine,
            |buf| { buf.extend_from_slice(&plaintext); Ok(()) },
        ).unwrap();
        let mut output = Vec::new();
        unpack_object_expect_with_context_into(
            &packed, ObjectType::ChunkData, &context, &engine, &mut output,
        ).unwrap();
        prop_assert_eq!(&plaintext, &output);
    }

    #[test]
    fn format_layer_roundtrip_chacha(
        plaintext in prop::collection::vec(any::<u8>(), 0..65536),
        enc_key in prop::array::uniform32(any::<u8>()),
        cid_key in prop::array::uniform32(any::<u8>()),
        context in prop::collection::vec(any::<u8>(), 0..64),
    ) {
        let engine = ChaCha20Poly1305Engine::new(&enc_key, &cid_key);
        let packed = pack_object_streaming_with_context(
            ObjectType::ChunkData, &context, plaintext.len(), &engine,
            |buf| { buf.extend_from_slice(&plaintext); Ok(()) },
        ).unwrap();
        let mut output = Vec::new();
        unpack_object_expect_with_context_into(
            &packed, ObjectType::ChunkData, &context, &engine, &mut output,
        ).unwrap();
        prop_assert_eq!(&plaintext, &output);
    }

    #[test]
    fn format_layer_wrong_context_fails(
        plaintext in prop::collection::vec(any::<u8>(), 1..1024),
        enc_key in prop::array::uniform32(any::<u8>()),
        cid_key in prop::array::uniform32(any::<u8>()),
        ctx1 in prop::collection::vec(any::<u8>(), 1..32),
        ctx2 in prop::collection::vec(any::<u8>(), 1..32),
    ) {
        prop_assume!(ctx1 != ctx2);
        let engine = Aes256GcmEngine::new(&enc_key, &cid_key);
        let packed = pack_object_streaming_with_context(
            ObjectType::ChunkData, &ctx1, plaintext.len(), &engine,
            |buf| { buf.extend_from_slice(&plaintext); Ok(()) },
        ).unwrap();
        let mut output = Vec::new();
        let result = unpack_object_expect_with_context_into(
            &packed, ObjectType::ChunkData, &ctx2, &engine, &mut output,
        );
        prop_assert!(result.is_err());
    }
}

// ---------------------------------------------------------------------------
// Step 4a: Item serde round-trip — variant-specific strategies
// ---------------------------------------------------------------------------

fn arb_chunk_id() -> impl Strategy<Value = ChunkId> {
    prop::array::uniform32(any::<u8>()).prop_map(ChunkId)
}

fn arb_chunk_ref() -> impl Strategy<Value = ChunkRef> {
    (arb_chunk_id(), 1..16_777_216u32, 1..16_777_216u32).prop_map(|(id, size, csize)| ChunkRef {
        id,
        size,
        csize,
    })
}

fn arb_xattrs() -> impl Strategy<Value = Option<HashMap<String, Vec<u8>>>> {
    prop::option::of(prop::collection::hash_map(
        "[a-z.]{1,16}",
        prop::collection::vec(any::<u8>(), 0..64),
        0..8,
    ))
}

fn arb_file_item() -> impl Strategy<Value = Item> {
    (
        "[a-z/]{1,32}",
        any::<u32>(),
        any::<u32>(),
        any::<u32>(),
        prop::option::of("[a-z]{1,8}"),
        prop::option::of("[a-z]{1,8}"),
        any::<i64>(),
        prop::option::of(any::<i64>()),
        prop::option::of(any::<i64>()),
        (
            arb_xattrs(),
            any::<bool>(),
            prop::collection::vec(arb_chunk_ref(), 1..10),
        ),
    )
        .prop_map(
            |(path, mode, uid, gid, user, group, mtime, atime, ctime, (xattrs, empty, chunks))| {
                if empty {
                    Item {
                        path,
                        entry_type: ItemType::RegularFile,
                        mode,
                        uid,
                        gid,
                        user,
                        group,
                        mtime,
                        atime,
                        ctime,
                        size: 0,
                        chunks: vec![],
                        link_target: None,
                        xattrs,
                    }
                } else {
                    let size = chunks.iter().map(|c| c.size as u64).sum();
                    Item {
                        path,
                        entry_type: ItemType::RegularFile,
                        mode,
                        uid,
                        gid,
                        user,
                        group,
                        mtime,
                        atime,
                        ctime,
                        size,
                        chunks,
                        link_target: None,
                        xattrs,
                    }
                }
            },
        )
}

fn arb_dir_item() -> impl Strategy<Value = Item> {
    (
        "[a-z/]{1,32}",
        any::<u32>(),
        any::<u32>(),
        any::<u32>(),
        prop::option::of("[a-z]{1,8}"),
        prop::option::of("[a-z]{1,8}"),
        any::<i64>(),
        prop::option::of(any::<i64>()),
        prop::option::of(any::<i64>()),
        arb_xattrs(),
    )
        .prop_map(
            |(path, mode, uid, gid, user, group, mtime, atime, ctime, xattrs)| Item {
                path,
                entry_type: ItemType::Directory,
                mode,
                uid,
                gid,
                user,
                group,
                mtime,
                atime,
                ctime,
                size: 0,
                chunks: vec![],
                link_target: None,
                xattrs,
            },
        )
}

fn arb_symlink_item() -> impl Strategy<Value = Item> {
    (
        "[a-z/]{1,32}",
        any::<u32>(),
        any::<u32>(),
        any::<u32>(),
        prop::option::of("[a-z]{1,8}"),
        prop::option::of("[a-z]{1,8}"),
        any::<i64>(),
        prop::option::of(any::<i64>()),
        prop::option::of(any::<i64>()),
        arb_xattrs(),
        "[a-z/]{1,32}",
    )
        .prop_map(
            |(path, mode, uid, gid, user, group, mtime, atime, ctime, xattrs, target)| Item {
                path,
                entry_type: ItemType::Symlink,
                mode,
                uid,
                gid,
                user,
                group,
                mtime,
                atime,
                ctime,
                size: 0,
                chunks: vec![],
                link_target: Some(target),
                xattrs,
            },
        )
}

fn arb_item() -> impl Strategy<Value = Item> {
    prop_oneof![arb_file_item(), arb_dir_item(), arb_symlink_item()]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]
    #[test]
    fn item_serde_roundtrip(item in arb_item()) {
        let bytes = rmp_serde::to_vec(&item).unwrap();
        let decoded: Item = rmp_serde::from_slice(&bytes).unwrap();
        prop_assert_eq!(item, decoded);
    }

    #[test]
    fn items_vec_serde_roundtrip(items in prop::collection::vec(arb_item(), 0..50)) {
        let bytes = rmp_serde::to_vec(&items).unwrap();
        let decoded: Vec<Item> = rmp_serde::from_slice(&bytes).unwrap();
        prop_assert_eq!(items, decoded);
    }
}

// ---------------------------------------------------------------------------
// Step 4b: ChunkIndex serde round-trip
// ---------------------------------------------------------------------------

fn arb_pack_id() -> impl Strategy<Value = PackId> {
    prop::array::uniform32(any::<u8>()).prop_map(PackId)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]
    #[test]
    fn chunk_index_serde_roundtrip(
        entries in prop::collection::hash_map(
            prop::array::uniform32(any::<u8>()).prop_map(ChunkId),
            (1..100u32, 1..16_777_216u32, arb_pack_id(), 0..u32::MAX as u64)
                .prop_map(|(rc, ss, pid, po)| (rc, ChunkIndexEntry { refcount: 1, stored_size: ss, pack_id: pid, pack_offset: po })),
            0..200,
        ),
        generation in any::<u64>(),
    ) {
        let mut index = ChunkIndex::new();
        for (id, (target_rc, entry)) in &entries {
            // add() sets refcount=1 and records the location
            index.add(*id, entry.stored_size, entry.pack_id, entry.pack_offset);
            // Bump refcount to match the target
            for _ in 1..*target_rc {
                index.increment_refcount(id);
            }
        }

        let blob = IndexBlob { generation, chunks: index };
        let bytes = rmp_serde::to_vec(&blob).unwrap();
        let decoded: IndexBlob = rmp_serde::from_slice(&bytes).unwrap();

        prop_assert_eq!(decoded.generation, generation);
        for (id, (target_rc, entry)) in &entries {
            let actual = decoded.chunks.get(id).unwrap();
            prop_assert_eq!(actual.refcount, *target_rc);
            prop_assert_eq!(actual.stored_size, entry.stored_size);
            prop_assert_eq!(actual.pack_id, entry.pack_id);
            prop_assert_eq!(actual.pack_offset, entry.pack_offset);
        }
    }
}

// ---------------------------------------------------------------------------
// Chunking property tests
// ---------------------------------------------------------------------------

mod chunker {
    use std::io::Cursor;

    use proptest::prelude::*;

    use crate::chunker::{chunk_data, chunk_stream};
    use crate::config::ChunkerConfig;

    /// Generate valid ChunkerConfig values that respect fastcdc's hard bounds:
    /// MINIMUM_MIN=64, AVERAGE_MIN=256, MAXIMUM_MIN=1024.
    /// Uses even min_size values to avoid cut_gear rounding issues.
    fn arb_chunker_config() -> impl Strategy<Value = ChunkerConfig> {
        // min_size: even values 64–2048
        (32..=1024u32)
            .prop_flat_map(|half_min| {
                let min_size = half_min * 2; // 64–2048, always even
                let avg_lo = 256u32.max(min_size * 2);
                let avg_hi = min_size * 8;
                (Just(min_size), avg_lo..=avg_hi)
            })
            .prop_flat_map(|(min_size, avg_size)| {
                let max_lo = 1024u32.max(avg_size * 2);
                let max_hi = avg_size * 4;
                (Just(min_size), Just(avg_size), max_lo..=max_hi)
            })
            .prop_map(|(min_size, avg_size, max_size)| ChunkerConfig {
                min_size,
                avg_size,
                max_size,
            })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(1000))]

        /// Chunks cover the input with no gaps or overlaps.
        #[test]
        fn chunker_completeness(
            data in prop::collection::vec(any::<u8>(), 0..65536),
            config in arb_chunker_config(),
        ) {
            let chunks = chunk_data(&data, &config);

            // Verify offset continuity
            let mut expected_offset = 0usize;
            for &(offset, length) in &chunks {
                prop_assert_eq!(offset, expected_offset, "gap or overlap at offset {}", offset);
                expected_offset = offset + length;
            }
            prop_assert_eq!(expected_offset, data.len(), "chunks don't cover entire input");

            // Reconstruct and compare
            let reconstructed: Vec<u8> = chunks.iter()
                .flat_map(|&(offset, length)| &data[offset..offset + length])
                .copied()
                .collect();
            prop_assert_eq!(&data, &reconstructed);
        }

        /// Same input and config always produce identical chunk boundaries.
        #[test]
        fn chunker_determinism(
            data in prop::collection::vec(any::<u8>(), 0..65536),
            config in arb_chunker_config(),
        ) {
            let run1 = chunk_data(&data, &config);
            let run2 = chunk_data(&data, &config);
            prop_assert_eq!(run1, run2);
        }

        /// Non-final chunks respect min_size <= length <= max_size.
        #[test]
        fn chunker_size_bounds(
            data in prop::collection::vec(any::<u8>(), 0..65536),
            config in arb_chunker_config(),
        ) {
            let chunks = chunk_data(&data, &config);
            let min = config.min_size as usize;
            let max = config.max_size as usize;

            for (i, &(_offset, length)) in chunks.iter().enumerate() {
                let is_last = i == chunks.len() - 1;
                if is_last {
                    prop_assert!(length <= max,
                        "last chunk {length} exceeds max_size {max}");
                } else {
                    prop_assert!(length >= min,
                        "chunk {i} length {length} < min_size {min}");
                    prop_assert!(length <= max,
                        "chunk {i} length {length} > max_size {max}");
                }
            }
        }

        /// Stream and slice APIs produce identical chunk boundaries.
        #[test]
        fn chunker_stream_matches_slice(
            data in prop::collection::vec(any::<u8>(), 0..65536),
            config in arb_chunker_config(),
        ) {
            let slice_chunks = chunk_data(&data, &config);
            let stream_chunks: Vec<(usize, usize)> = chunk_stream(Cursor::new(&data), &config)
                .map(|result| {
                    let chunk = result.expect("stream chunking should succeed");
                    (chunk.offset as usize, chunk.length)
                })
                .collect();
            prop_assert_eq!(slice_chunks, stream_chunks);
        }
    }
}

// ---------------------------------------------------------------------------
// Step 5: Backup-restore round-trip — nested trees
// ---------------------------------------------------------------------------

#[cfg(test)]
mod backup_restore {
    use proptest::prelude::*;

    use crate::commands;
    use crate::tests::helpers::{backup_single_source, init_repo};

    fn arb_nested_file_tree() -> impl Strategy<Value = Vec<(String, Vec<u8>)>> {
        let path_strategy = prop_oneof![
            "[a-z]{1,6}\\.[a-z]{1,3}",
            "[a-z]{1,4}/[a-z]{1,6}\\.[a-z]{1,3}",
            "[a-z]{1,4}/[a-z]{1,4}/[a-z]{1,6}\\.[a-z]{1,3}",
        ];
        prop::collection::hash_map(
            path_strategy,
            prop_oneof![
                Just(vec![]),
                prop::collection::vec(any::<u8>(), 1..64),
                prop::collection::vec(any::<u8>(), 64..4096),
                prop::collection::vec(any::<u8>(), 4096..65536),
            ],
            1..20,
        )
        .prop_map(|m| m.into_iter().collect::<Vec<_>>())
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(16))]
        #[test]
        #[ignore] // run with: cargo test -- --ignored
        fn backup_restore_nested_roundtrip(files in arb_nested_file_tree()) {
            let tmp = tempfile::tempdir().unwrap();
            let repo_dir = tmp.path().join("repo");
            let source_dir = tmp.path().join("source");
            let restore_dir = tmp.path().join("restore");

            // Create nested directories and files
            for (rel_path, content) in &files {
                let full = source_dir.join(rel_path);
                if let Some(parent) = full.parent() {
                    std::fs::create_dir_all(parent).unwrap();
                }
                std::fs::write(&full, content).unwrap();
            }

            let config = init_repo(&repo_dir);
            backup_single_source(&config, &source_dir, "test", "snap-1");
            commands::restore::run(
                &config,
                None,
                "snap-1",
                restore_dir.to_str().unwrap(),
                None,
                config.xattrs.enabled,
            )
            .unwrap();

            for (rel_path, expected) in &files {
                let actual = std::fs::read(restore_dir.join(rel_path)).unwrap();
                prop_assert_eq!(expected, &actual, "mismatch: {}", rel_path);
            }
        }
    }
}
