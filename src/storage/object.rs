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
                let bucket = cfg.bucket.as_deref().ok_or_else(|| anyhow!("S3 bucket is required"))?;
                let mut builder = services::S3::default().bucket(bucket).root(&cfg.root);
                if let Some(v) = &cfg.endpoint { builder = builder.endpoint(v); }
                if let Some(v) = &cfg.region { builder = builder.region(v); }
                if let Some(v) = &cfg.access_key_id { builder = builder.access_key_id(v); }
                if let Some(v) = &cfg.secret_access_key { builder = builder.secret_access_key(v); }
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
        if got.as_ref() != b"ok" { return Err(anyhow!("object storage read-back mismatch")); }
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

    #[tokio::test]
    async fn filesystem_backend_round_trips_bytes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = ObjectStorageConfig {
            kind: "fs".into(),
            root: dir.path().to_string_lossy().into_owned(),
            bucket: None,
            endpoint: None,
            region: None,
            access_key_id: None,
            secret_access_key: None,
        };

        let store = ObjectStorage::from_config(&cfg).await.expect("create store");
        store
            .put("sessions/test.json", Bytes::from_static(br#"{"ok":true}"#))
            .await
            .expect("write");

        let got = store.get("sessions/test.json").await.expect("read");
        assert_eq!(got.as_ref(), br#"{"ok":true}"#);

        store.delete("sessions/test.json").await.expect("delete");
    }
}
