use crate::config::ObjectStorageConfig;
use anyhow::{anyhow, Result};
use bytes::Bytes;
use opendal::{services, Operator};

#[derive(Clone)]
pub struct ObjectStorage {
    op: Operator,
}

impl ObjectStorage {
    pub async fn from_config(cfg: &ObjectStorageConfig) -> Result<Self> {
        let op = match cfg.kind.as_str() {
            "fs" | "filesystem" => {
                std::fs::create_dir_all(&cfg.root)?;
                let builder = services::Fs::default().root(&cfg.root);
                Operator::new(builder)?
            }
            "s3" => {
                let bucket = cfg
                    .bucket
                    .as_deref()
                    .ok_or_else(|| anyhow!("S3 bucket is required"))?;
                let mut builder = services::S3::default().bucket(bucket).root(&cfg.root);
                if let Some(v) = &cfg.endpoint {
                    builder = builder.endpoint(v);
                }
                if let Some(v) = &cfg.region {
                    builder = builder.region(v);
                }
                if let Some(v) = &cfg.access_key_id {
                    builder = builder.access_key_id(v);
                }
                if let Some(v) = &cfg.secret_access_key {
                    builder = builder.secret_access_key(v);
                }
                Operator::new(builder)?
            }
            other => return Err(anyhow!("unsupported object storage kind: {other}")),
        };
        Ok(Self { op })
    }

    pub async fn check(&self) -> Result<()> {
        let key = format!("health/{}.txt", uuid::Uuid::now_v7());
        self.put(&key, Bytes::from_static(b"ok")).await?;
        let got = self.get(&key).await?;
        if got.as_ref() != b"ok" {
            return Err(anyhow!("object storage read-back mismatch"));
        }
        self.delete(&key).await?;
        Ok(())
    }

    pub async fn put(&self, key: &str, data: Bytes) -> Result<()> {
        self.op.write(key, data).await?;
        Ok(())
    }

    pub async fn get(&self, key: &str) -> Result<Bytes> {
        Ok(self.op.read(key).await?.to_bytes())
    }

    pub async fn delete(&self, key: &str) -> Result<()> {
        self.op.delete(key).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ObjectStorageConfig;
    use std::time::Duration;

    #[tokio::test]
    async fn object_store_backend_contract() {
        let key = format!("contract/{}.bin", uuid::Uuid::now_v7());
        let bytes = Bytes::from_static(b"\0steve object store contract\xff\n");

        let dir = tempfile::tempdir().expect("tempdir");
        let filesystem = ObjectStorage::from_config(&ObjectStorageConfig {
            kind: "fs".into(),
            root: dir.path().to_string_lossy().into_owned(),
            bucket: None,
            endpoint: None,
            region: None,
            access_key_id: None,
            secret_access_key: None,
        })
        .await
        .expect("create filesystem store");
        run_object_store_contract(filesystem, &key, bytes.clone()).await;

        if let Ok(endpoint) = std::env::var("STEVE_TEST_S3_ENDPOINT") {
            let s3 = ObjectStorage::from_config(&ObjectStorageConfig {
                kind: "s3".into(),
                root: "/".into(),
                bucket: Some("steve".into()),
                endpoint: Some(endpoint),
                region: Some("us-east-1".into()),
                access_key_id: Some("test".into()),
                secret_access_key: Some("test".into()),
            })
            .await
            .expect("create S3 store");
            run_object_store_contract(s3, &key, bytes).await;
        }
    }

    async fn run_object_store_contract(store: ObjectStorage, key: &str, bytes: Bytes) {
        tokio::time::timeout(Duration::from_secs(15), async {
            store.put(key, bytes.clone()).await.expect("write object");

            let got = store.get(key).await;
            let deleted = store.delete(key).await;
            let missing = store.get(key).await;

            deleted.expect("delete object");
            assert_eq!(got.expect("read object").as_ref(), bytes.as_ref());
            let error = missing.expect_err("read after delete must fail");
            assert!(
                error
                    .downcast_ref::<opendal::Error>()
                    .is_some_and(|error| error.kind() == opendal::ErrorKind::NotFound),
                "read after delete returned {error:?}, expected OpenDAL NotFound"
            );
        })
        .await
        .expect("object store contract timed out");
    }
}
