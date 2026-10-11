//! Full-text search across record kinds. The caller decides which kinds
//! (by capability); each kind is one static query on its own GIN index.

use homestead_domain::search::{SearchHit, SearchKind};
use uuid::Uuid;

use crate::{Result, Store};

/// `$1` is a prefix tsquery (see `domain::search::prefix_query`), `$2` a
/// limit. Each returns `(id, title, subtitle, rank)`.
pub fn search_sql(kind: SearchKind) -> &'static str {
    match kind {
        SearchKind::Animal => {
            "select id, name, species || ' · ' || replace(status, '_', ' '), ts_rank(search, q)
             from homestead.animals, to_tsquery('simple', $1) q where search @@ q order by 4 desc limit $2"
        }
        SearchKind::Task => {
            "select id, title, status || coalesce(' · due ' || due_on::text, ''), ts_rank(search, q)
             from homestead.tasks, to_tsquery('simple', $1) q where search @@ q
             order by status <> 'open', 4 desc limit $2"
        }
        SearchKind::Item => {
            "select id, name, trim_scale(on_hand)::text || ' ' || unit || ' on hand', ts_rank(search, q)
             from homestead.inventory_items, to_tsquery('simple', $1) q where search @@ q order by 4 desc limit $2"
        }
        SearchKind::Planting => {
            "select p.id, p.crop || case when p.variety <> '' then ' ''' || p.variety || '''' else '' end,
                    l.name || ' · sown ' || p.sown_on::text || ' · ' || p.status, ts_rank(p.search, q)
             from homestead.plantings p join homestead.locations l on l.id = p.location_id,
                  to_tsquery('simple', $1) q
             where p.search @@ q order by 4 desc limit $2"
        }
        SearchKind::SeedLot => {
            "select id, crop || case when variety <> '' then ' ''' || variety || '''' else '' end,
                    trim(supplier || ' ' || supplier_lot), ts_rank(search, q)
             from homestead.seed_lots, to_tsquery('simple', $1) q where search @@ q order by 4 desc limit $2"
        }
        SearchKind::Location => {
            "select id, name, kind, ts_rank(search, q)
             from homestead.locations, to_tsquery('simple', $1) q where search @@ q order by 4 desc limit $2"
        }
        SearchKind::Contact => {
            "select id, coalesce(nullif(name, ''), email),
                    email || case when unsubscribed_at is not null then ' · unsubscribed' else '' end, ts_rank(search, q)
             from homestead.contacts, to_tsquery('simple', $1) q where search @@ q order by 4 desc limit $2"
        }
        SearchKind::Gift => {
            "select id, donor_name, replace(kind, '_', '-') || ' gift · ' || received_on::text, ts_rank(search, q)
             from homestead.gifts, to_tsquery('simple', $1) q where search @@ q order by 4 desc limit $2"
        }
        SearchKind::Event => {
            "select id, title, held_on::text || case when place <> '' then ' · ' || place else '' end,
                    ts_rank(search, q)
             from homestead.community_events, to_tsquery('simple', $1) q where search @@ q order by 4 desc limit $2"
        }
        SearchKind::Equipment => {
            "select id, name, category || ' · ' || replace(status, '_', ' '), ts_rank(search, q)
             from homestead.equipment, to_tsquery('simple', $1) q where search @@ q order by 4 desc limit $2"
        }
        SearchKind::Vendor => {
            "select id, name, '', ts_rank(search, q)
             from homestead.vendors, to_tsquery('simple', $1) q where search @@ q order by 4 desc limit $2"
        }
        SearchKind::Purchase => {
            "select id, coalesce(nullif(description, ''), category),
                    purchased_on::text || ' · $' || amount::text || coalesce(' · ' || external_reference, ''),
                    ts_rank(search, q)
             from homestead.purchases, to_tsquery('simple', $1) q where search @@ q order by 4 desc limit $2"
        }
    }
}

impl Store {
    /// The best matches of each of `kinds`, best first overall. `query` must
    /// come from `prefix_query`.
    pub async fn search(&self, kinds: &[SearchKind], query: &str, per_kind: i64) -> Result<Vec<SearchHit>> {
        let mut hits = Vec::new();
        for kind in kinds {
            let rows: Vec<(Uuid, String, String, f32)> =
                sqlx::query_as(search_sql(*kind)).bind(query).bind(per_kind).fetch_all(&self.pool).await?;
            hits.extend(rows.into_iter().map(|(id, title, subtitle, rank)| SearchHit {
                kind: *kind,
                id,
                title,
                subtitle,
                rank,
                url: kind.url(id),
            }));
        }
        hits.sort_by(|a, b| b.rank.total_cmp(&a.rank).then_with(|| a.title.cmp(&b.title)));
        Ok(hits)
    }
}
