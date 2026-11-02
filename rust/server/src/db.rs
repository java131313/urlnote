// MongoDB connection, indexes, GridFS buckets and shared state.

use futures_util::AsyncReadExt;
use futures_util::AsyncWriteExt;
use mongodb::bson::oid::ObjectId;
use mongodb::bson::{Bson, Document};
use mongodb::gridfs::{GridFsBucket, GridFsUploadStream};
use mongodb::options::{FindOneAndUpdateOptions, GridFsBucketOptions, IndexOptions, ReturnDocument};
use mongodb::{bson::doc, Client, Collection, Database, IndexModel};

/// GridFS bucket names: note/mark content and uploaded images are blobs;
/// everything structured lives in plain collections.
pub const BUCKET_CONTENTS: &str = "notecontents";
pub const BUCKET_MARKS: &str = "markcontents";
pub const BUCKET_IMAGES: &str = "images";

#[derive(Clone)]
pub struct AppState {
    pub db: Database,
    pub cors_origins: Vec<String>,
    pub static_dir: Option<std::path::PathBuf>,
}

impl AppState {
    pub async fn connect() -> Self {
        let uri = std::env::var("MONGODB_URI").unwrap_or_else(|_| "mongodb://localhost:27017".into());
        let name = std::env::var("MONGO_DB").unwrap_or_else(|_| "urlnote".into());
        let client = Client::with_uri_str(&uri)
            .await
            .unwrap_or_else(|e| panic!("mongodb connect failed ({uri}): {e}"));
        let db = client.database(&name);
        init_indexes(&db).await;
        let cors_origins = std::env::var("CORS_ORIGINS")
            .unwrap_or_default()
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        let static_dir = std::env::var("STATIC_DIR")
            .ok()
            .map(std::path::PathBuf::from)
            .or_else(|| {
                for cand in ["frontend/dist", "../frontend/dist", "dist"] {
                    let p = std::path::Path::new(cand);
                    if p.join("index.html").exists() {
                        return Some(p.to_path_buf());
                    }
                }
                None
            });
        AppState {
            db,
            cors_origins,
            static_dir,
        }
    }

    pub fn bucket(&self, name: &str) -> GridFsBucket {
        self.db.gridfs_bucket(
            GridFsBucketOptions::builder()
                .bucket_name(name.to_string())
                .build(),
        )
    }

    pub fn col(&self, name: &str) -> Collection<Document> {
        self.db.collection(name)
    }

    /// Upload bytes to a GridFS bucket; returns the new file id.
    pub async fn gridfs_put(&self, bucket: &str, filename: &str, bytes: &[u8]) -> Result<ObjectId, String> {
        let b = self.bucket(bucket);
        let mut up: GridFsUploadStream = b
            .open_upload_stream(filename)
            .await
            .map_err(|e| e.to_string())?;
        up.write_all(bytes).await.map_err(|e| e.to_string())?;
        up.close().await.map_err(|e| e.to_string())?;
        match up.id() {
            Bson::ObjectId(o) => Ok(*o),
            other => Err(format!("unexpected gridfs id: {other}")),
        }
    }

    pub async fn gridfs_get(&self, bucket: &str, id: ObjectId) -> Result<Vec<u8>, String> {
        let b = self.bucket(bucket);
        let mut cursor = b
            .open_download_stream(Bson::ObjectId(id))
            .await
            .map_err(|e| e.to_string())?;
        let mut buf = Vec::new();
        cursor.read_to_end(&mut buf).await.map_err(|e| e.to_string())?;
        Ok(buf)
    }

    pub async fn gridfs_delete(&self, bucket: &str, id: ObjectId) {
        let _ = self.bucket(bucket).delete(Bson::ObjectId(id)).await;
    }
}

async fn init_indexes(db: &Database) {
    // sessions: TTL auto-purge (stronger than the D1 manual cleanup)
    let sessions = db.collection::<Document>("sessions");
    let _ = sessions
        .create_index(
            IndexModel::builder()
                .keys(doc! { "expires_at": 1 })
                .options(IndexOptions::builder().expire_after(std::time::Duration::ZERO).build())
                .build(),
        )
        .await;
    // search tokens (multikey)
    let notes = db.collection::<Document>("notes");
    let _ = notes
        .create_index(IndexModel::builder().keys(doc! { "tokens": 1 }).build())
        .await;
    let _ = notes
        .create_index(IndexModel::builder().keys(doc! { "updated_at": -1 }).build())
        .await;
    // dictionary
    let words = db.collection::<Document>("dict_word");
    let _ = words
        .create_index(
            IndexModel::builder()
                .keys(doc! { "lang": 1, "word": 1 })
                .options(IndexOptions::builder().unique(true).build())
                .build(),
        )
        .await;
    let _ = words
        .create_index(IndexModel::builder().keys(doc! { "group_id": 1 }).build())
        .await;
    let groups = db.collection::<Document>("dict_group");
    let _ = groups
        .create_index(
            IndexModel::builder()
                .keys(doc! { "key": 1 })
                .options(
                    IndexOptions::builder()
                        .unique(true)
                        .sparse(true)
                        .build(),
                )
                .build(),
        )
        .await;
    let _ = db
        .collection::<Document>("presence")
        .create_index(IndexModel::builder().keys(doc! { "note_id": 1 }).build())
        .await;
}

/// Next value of a named counter (used for dict_group ids).
pub async fn next_seq(db: &Database, name: &str) -> Result<i64, String> {
    let counters = db.collection::<Document>("counters");
    let doc = counters
        .find_one_and_update(
            doc! { "_id": name },
            doc! { "$inc": { "seq": mongodb::bson::Bson::Int64(1) } },
        )
        .with_options(
            FindOneAndUpdateOptions::builder()
                .upsert(true)
                .return_document(ReturnDocument::After)
                .build(),
        )
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "counter missing".to_string())?;
    // $inc created the field: read tolerantly (Int32/Int64)
    doc.get("seq")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| "counter seq missing".to_string())
}

pub fn doc_str(d: &Document, key: &str) -> String {
    d.get_str(key).unwrap_or_default().to_string()
}

pub fn doc_i64(d: &Document, key: &str) -> i64 {
    d.get_i64(key).unwrap_or(0)
}

pub fn doc_objid(d: &Document, key: &str) -> Option<ObjectId> {
    match d.get(key) {
        Some(Bson::ObjectId(id)) => Some(*id),
        _ => None,
    }
}

pub fn doc_bool(d: &Document, key: &str) -> bool {
    d.get_bool(key).unwrap_or(false)
}

pub fn doc_str_array(d: &Document, key: &str) -> Vec<String> {
    d.get_array(key)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}
