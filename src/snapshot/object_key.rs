use object_store::path::Path;
use uuid::Uuid;

use crate::model::SERVER_STORAGE_BYTES;

pub(crate) const MAX_MANIFEST_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ObjectKind {
    Chunk,
    Manifest { generation: i64, checksum: String },
    Export { export_id: Uuid, attempt: i32 },
}

#[derive(Debug, Clone)]
pub(crate) struct WorkerObjectKey {
    path: Path,
    kind: ObjectKind,
}

impl WorkerObjectKey {
    pub(crate) fn parse(value: &str) -> Result<Self, InvalidObjectKey> {
        let kind = if let Some(hash) = value.strip_prefix("chunks/") {
            if !is_lower_hex(hash, 64) {
                return Err(InvalidObjectKey);
            }
            ObjectKind::Chunk
        } else if let Some(name) = value.strip_prefix("snapshots/") {
            let Some((generation, checksum)) = name
                .strip_suffix(".json")
                .and_then(|name| name.split_once('-'))
            else {
                return Err(InvalidObjectKey);
            };
            if !is_positive_decimal(generation) || !is_lower_hex(checksum, 64) {
                return Err(InvalidObjectKey);
            }
            ObjectKind::Manifest {
                generation: generation.parse().map_err(|_| InvalidObjectKey)?,
                checksum: checksum.to_owned(),
            }
        } else if let Some(suffix) = value.strip_prefix("exports/") {
            let mut parts = suffix.split('/');
            let (Some(export_id), Some(archive_name), None) =
                (parts.next(), parts.next(), parts.next())
            else {
                return Err(InvalidObjectKey);
            };
            let Ok(parsed_id) = Uuid::parse_str(export_id) else {
                return Err(InvalidObjectKey);
            };
            let Some(attempt) = archive_name.strip_suffix(".zip") else {
                return Err(InvalidObjectKey);
            };
            if parsed_id.to_string() != export_id || !is_positive_decimal(attempt) {
                return Err(InvalidObjectKey);
            }
            ObjectKind::Export {
                export_id: parsed_id,
                attempt: attempt.parse().map_err(|_| InvalidObjectKey)?,
            }
        } else {
            return Err(InvalidObjectKey);
        };

        let path = Path::parse(value).map_err(|_| InvalidObjectKey)?;
        if path.as_ref() != value {
            return Err(InvalidObjectKey);
        }
        Ok(Self { path, kind })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn kind(&self) -> ObjectKind {
        self.kind.clone()
    }

    pub(crate) fn max_bytes(&self) -> u64 {
        match &self.kind {
            ObjectKind::Manifest { .. } => MAX_MANIFEST_BYTES,
            ObjectKind::Chunk | ObjectKind::Export { .. } => SERVER_STORAGE_BYTES as u64,
        }
    }
}

pub fn physical_object_path(server_id: Uuid, relative_key: &str) -> Result<Path, InvalidObjectKey> {
    let key = WorkerObjectKey::parse(relative_key)?;
    Ok(Path::from(format!("servers/{server_id}/{}", key.path())))
}

#[derive(Debug, thiserror::Error)]
#[error("invalid worker object key")]
pub struct InvalidObjectKey;

fn is_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_positive_decimal(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('0')
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && value.parse::<i64>().is_ok_and(|number| number > 0)
}

#[cfg(test)]
mod tests {
    use super::{ObjectKind, WorkerObjectKey, physical_object_path};
    use uuid::Uuid;

    #[test]
    fn accepts_only_canonical_server_relative_object_names() {
        let cases = [
            (format!("chunks/{}", "a".repeat(64)), ObjectKind::Chunk),
            (
                format!("snapshots/12-{}.json", "b".repeat(64)),
                ObjectKind::Manifest {
                    generation: 12,
                    checksum: "b".repeat(64),
                },
            ),
            (
                "exports/123e4567-e89b-12d3-a456-426614174000/2.zip".to_owned(),
                ObjectKind::Export {
                    export_id: uuid::Uuid::parse_str("123e4567-e89b-12d3-a456-426614174000")
                        .unwrap(),
                    attempt: 2,
                },
            ),
        ];

        for (key, kind) in cases {
            assert_eq!(WorkerObjectKey::parse(&key).unwrap().kind(), kind);
        }
    }

    #[test]
    fn rejects_other_namespaces_traversal_and_noncanonical_components() {
        for key in [
            "chunks/../secret",
            "chunks/AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "chunks/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/extra",
            "snapshots/0-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.json",
            "snapshots/01-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.json",
            "snapshots/1-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA.json",
            "exports/123e4567-e89b-12d3-a456-426614174000/0.zip",
            "exports/123e4567-e89b-12d3-a456-426614174000/1.zip/other",
            "registry/credentials.json",
            "servers/123e4567-e89b-12d3-a456-426614174000/chunks/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "../chunks/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ] {
            assert!(WorkerObjectKey::parse(key).is_err(), "accepted {key}");
        }
    }

    #[test]
    fn physical_namespace_is_derived_from_server_and_validated_relative_key() {
        let server_a = Uuid::parse_str("123e4567-e89b-12d3-a456-426614174000").unwrap();
        let server_b = Uuid::parse_str("123e4567-e89b-12d3-a456-426614174001").unwrap();
        let key = format!("chunks/{}", "a".repeat(64));
        let path_a = physical_object_path(server_a, &key).unwrap();
        let path_b = physical_object_path(server_b, &key).unwrap();
        assert_ne!(path_a, path_b);
        assert_eq!(path_a.as_ref(), format!("servers/{server_a}/{key}"));
        assert!(physical_object_path(server_a, "registry/credentials").is_err());
    }
}
