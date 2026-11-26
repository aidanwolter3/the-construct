//! A row's image results (decision `pick-a-photo`): the pictures an
//! `images` filler's search finds for a row, so a layout can show them and
//! let people pick one (an ordinary update of a field of the row).

use protocol::{DataBundle, EntityRecord};
use serde_json::{Value, json};

use crate::db;
use crate::filler::Filler;
use crate::model::Definition;
use pod_core::Pod;

/// Most results a row's list shows.
pub const MAX_RESULTS: usize = web_extract::search::MAX_RESULTS;

/// The query key of row `id`'s image results.
pub fn query_key(collection: &str, id: &str) -> String {
    format!("collection:{collection}:images:{id}")
}

/// The entity URI of row `id`'s `n`-th image result.
pub fn result_uri(collection: &str, id: &str, n: usize) -> String {
    format!("collections://{collection}/{id}/images/{n}")
}

/// Why a row has no image results to show.
#[derive(Debug, PartialEq)]
pub enum NoResults {
    /// No such collection, or no live row `id`.
    NotFound,
    /// The collection has no `images` filler (or not the one named).
    NoFiller,
}

/// The `images` filler to search with: `filler` (an id), else the first.
fn images_filler<'a>(def: &'a Definition, filler: Option<&str>) -> Option<&'a Filler> {
    def.fillers
        .iter()
        .filter(|f| f.is_images())
        .find(|f| filler.is_none_or(|id| f.id == id))
}

/// Row `id`'s image results: its `images` filler's search for the row's
/// query (cached like the fill's), each as an entity with `image`,
/// `thumbnail`, `page`, `title`, `source`, `n` (1 for the first) and
/// `row_id`. An empty query, no `web_search` jack or a failed search is an
/// empty list.
pub async fn results(
    pod: &Pod,
    collection: &str,
    id: &str,
    filler: Option<&str>,
) -> Result<DataBundle, NoResults> {
    let (name, rid) = (collection.to_owned(), id.to_owned());
    let found = pod
        .blocking(move |s| {
            s.with_conn(|c| {
                let Some((_, def)) = db::latest_def(c, &name)? else {
                    return Ok(None);
                };
                let row = db::row(c, &name, &rid)?.filter(|r| r.deleted_at.is_none());
                Ok(row.map(|r| (def, r)))
            })
        })
        .await
        .ok()
        .flatten();
    let Some((def, row)) = found else {
        return Err(NoResults::NotFound);
    };
    let Some(f) = images_filler(&def, filler) else {
        return Err(NoResults::NoFiller);
    };
    let query = f
        .query_field()
        .and_then(|q| row.values.get(q))
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("")
        .to_owned();
    let mut data = DataBundle::default();
    let key = query_key(collection, id);
    let mut uris = Vec::new();
    if !query.is_empty()
        && let Some(web) = crate::agent::pod_web(pod)
        && let Ok(images) = web_extract::search::search_images(
            &web.hosts,
            &web.cache,
            &web.client,
            &web.provider,
            &query,
            MAX_RESULTS,
            web.meter(),
        )
        .await
    {
        for (i, img) in images.iter().enumerate() {
            let uri = result_uri(collection, id, i + 1);
            let e = json!({
                "id": format!("{id}-{}", i + 1),
                "uri": uri,
                "row_id": id,
                "n": i + 1,
                "image": img.image,
                "thumbnail": img.thumbnail,
                "page": img.page,
                "title": img.title,
                "source": img.source,
            });
            if let Value::Object(m) = e {
                data.entities.insert(uri.clone(), EntityRecord(m));
            }
            uris.push(uri);
        }
    }
    data.queries.insert(key, uris);
    Ok(data)
}
